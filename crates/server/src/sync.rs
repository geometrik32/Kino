//! Real-time multi-device playback synchronization (`POST /api/sync/join`,
//! `POST /api/sync/leave`, `GET /api/sync/ws`).

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::Response,
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use kino_core::{
    is_finished, MediaKind, Progress, ProgressUpdate, ReleaseChoiceUpdate, ReleaseRef, SyncClientMsg, SyncEventKind,
    SyncJoinRequest, SyncJoinResponse, SyncLeaveRequest, SyncMemberInfo, SyncServerMsg, SyncState,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::{
    api::{infohash_ok, persist_progress, AppState, CurrentUser},
    db,
    error::AppError,
    userdata,
};

const TICKET_TTL_SECS: i64 = 120;
const JOIN_HANDSHAKE_TIMEOUT_MS: i64 = 15_000;
const HEARTBEAT_TIMEOUT_MS: i64 = 18_000;
const BARRIER_TIMEOUT_MS: i64 = 30_000;
const AUTO_SAVE_INTERVAL_MS: i64 = 10_000;
const MAX_ROOMS_PER_USER: usize = 8;
const MAX_MEMBERS_PER_ROOM: usize = 16;
const WS_OUTBOUND_CAPACITY: usize = 64;
const WS_MAX_MESSAGE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncTicketClaims {
    pub uid: i64,
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub cid: String,
    pub exp: i64,
}

fn ticket_mac(secret: &[u8], payload: &str) -> Hmac<Sha256> {
    let key = Sha256::new().chain_update(secret).chain_update(b"kino-sync").finalize();
    let mut m = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC accepts any key length");
    m.update(payload.as_bytes());
    m
}

pub fn issue_ticket(secret: &[u8], claims: &SyncTicketClaims) -> String {
    let payload = B64.encode(serde_json::to_vec(claims).expect("sync ticket claims serialize"));
    let sig = B64.encode(ticket_mac(secret, &payload).finalize().into_bytes());
    format!("{payload}.{sig}")
}

pub fn verify_ticket(secret: &[u8], token: &str, now: DateTime<Utc>) -> Option<SyncTicketClaims> {
    let (payload, sig) = token.split_once('.')?;
    ticket_mac(secret, payload).verify_slice(&B64.decode(sig).ok()?).ok()?;
    let claims: SyncTicketClaims = serde_json::from_slice(&B64.decode(payload).ok()?).ok()?;
    (claims.exp > now.timestamp()).then_some(claims)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoomKey {
    pub user_id: i64,
    pub kind: MediaKind,
    pub tmdb_id: i64,
}

struct Member {
    client_id: String,
    device: String,
    conn_id: u64,
    connected: bool,
    joined_at_ms: i64,
    last_seen_ms: i64,
    ready: bool,
    buffering: bool,
    waiting_since_ms: Option<i64>,
    tx: Option<mpsc::Sender<SyncServerMsg>>,
}

struct Room {
    key: RoomKey,
    revision: u64,
    season: i32,
    episode: i32,
    release: ReleaseChoiceUpdate,
    file_index: Option<i32>,
    anchor_pos: f64,
    anchor_at_ms: i64,
    duration_sec: Option<f64>,
    paused: bool,
    waiting: bool,
    waiting_for: Vec<String>,
    rate: f32,
    audio_track: Option<String>,
    sound_client_id: Option<String>,
    last_device: String,
    last_saved_ms: i64,
    members: Vec<Member>,
}

#[derive(Debug, Clone)]
pub struct SaveJob {
    pub user_id: i64,
    pub update: ProgressUpdate,
}

fn sanitize_pos(pos: f64, duration: Option<f64>) -> f64 {
    if !pos.is_finite() {
        return 0.0;
    }
    let pos = pos.max(0.0);
    match duration {
        Some(d) if d.is_finite() && d > 0.0 => pos.min(d),
        _ => pos,
    }
}

fn sanitize_duration(duration: Option<f64>) -> Option<f64> {
    duration.filter(|d| d.is_finite() && *d > 0.0)
}

fn sanitize_rate(rate: f32) -> f32 {
    if rate.is_finite() {
        rate.clamp(0.25, 4.0)
    } else {
        1.0
    }
}

impl Room {
    fn current_pos(&self, now_ms: i64) -> f64 {
        if self.paused || self.waiting {
            return self.anchor_pos;
        }
        let dt = ((now_ms - self.anchor_at_ms).max(0) as f64) / 1000.0;
        sanitize_pos(self.anchor_pos + dt * f64::from(self.rate), self.duration_sec)
    }

    fn recompute_waiting(&mut self, now_ms: i64) {
        let mut names = Vec::new();
        for m in &self.members {
            if (!m.ready || m.buffering) && !names.contains(&m.device) {
                names.push(m.device.clone());
            }
        }
        let should_wait = !self.paused && self.members.len() > 1 && !names.is_empty();
        if should_wait && !self.waiting {
            self.anchor_pos = self.current_pos(now_ms);
            self.anchor_at_ms = now_ms;
        } else if !should_wait && self.waiting {
            self.anchor_at_ms = now_ms;
        }
        self.waiting = should_wait;
        self.waiting_for = names;
    }

    fn ensure_sound_owner(&mut self) {
        let valid = self
            .sound_client_id
            .as_deref()
            .is_some_and(|cid| self.members.iter().any(|m| m.client_id == cid));
        if !valid {
            self.sound_client_id = self.members.first().map(|m| m.client_id.clone());
        }
    }

    fn snapshot(&self, now_ms: i64, updated_by: Option<String>, event: SyncEventKind) -> SyncState {
        SyncState {
            revision: self.revision,
            kind: self.key.kind,
            tmdb_id: self.key.tmdb_id,
            season: self.season,
            episode: self.episode,
            release: self.release.clone(),
            position_sec: self.current_pos(now_ms),
            duration_sec: self.duration_sec,
            paused: self.paused,
            waiting: self.waiting,
            waiting_for: if self.waiting {
                self.waiting_for.clone()
            } else {
                Vec::new()
            },
            rate: self.rate,
            audio_track: self.audio_track.clone(),
            sound_client_id: self.sound_client_id.clone(),
            server_now_ms: now_ms,
            updated_by,
            event,
            members: self
                .members
                .iter()
                .map(|m| SyncMemberInfo {
                    client_id: m.client_id.clone(),
                    device: m.device.clone(),
                    ready: m.ready,
                    buffering: m.buffering,
                })
                .collect(),
        }
    }

    fn broadcast(&self, state: &SyncState) {
        let msg = SyncServerMsg::State(Box::new(state.clone()));
        for m in &self.members {
            if let Some(tx) = &m.tx {
                let _ = tx.try_send(msg.clone());
            }
        }
    }

    fn make_save_job(&mut self, now_ms: i64, pos_override: Option<f64>, finished: bool) -> Option<SaveJob> {
        let duration = self.duration_sec.unwrap_or(0.0);
        let raw_pos = if finished && duration > 0.0 {
            duration
        } else {
            pos_override.unwrap_or_else(|| self.current_pos(now_ms))
        };
        let pos = sanitize_pos(raw_pos, Some(duration).filter(|d| *d > 0.0));
        if duration <= 0.0 && !finished && pos < 1.0 {
            return None;
        }
        self.last_saved_ms = now_ms;
        Some(SaveJob {
            user_id: self.key.user_id,
            update: ProgressUpdate {
                kind: self.key.kind,
                tmdb_id: self.key.tmdb_id,
                season: if self.key.kind == MediaKind::Tv { self.season } else { 0 },
                episode: if self.key.kind == MediaKind::Tv {
                    self.episode
                } else {
                    0
                },
                position_sec: pos,
                duration_sec: duration.max(0.0),
                release: self.file_index.map(|file_index| ReleaseRef {
                    infohash: self.release.infohash.clone(),
                    file_index,
                    title: self.release.title.clone(),
                }),
                device: Some(self.last_device.clone()),
            },
        })
    }
}

/// Coordinates active playback rooms per `(user_id, kind, tmdb_id)`.
#[derive(Default)]
pub struct SyncHub {
    rooms: Mutex<HashMap<RoomKey, Arc<Mutex<Room>>>>,
    next_conn_id: AtomicU64,
}

impl SyncHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of currently active players across all users (`user_id -> (kind, tmdb_id, season, episode, paused)`).
    pub fn active_watchers(&self) -> HashMap<i64, (MediaKind, i64, i32, i32, bool)> {
        let now = Self::now_ms();
        let rooms = self.rooms.lock().expect("sync rooms lock");
        let mut out: HashMap<i64, (MediaKind, i64, i32, i32, bool)> = HashMap::new();
        for (key, room_arc) in rooms.iter() {
            if let Ok(room) = room_arc.try_lock() {
                let alive = room
                    .members
                    .iter()
                    .any(|m| now - m.last_seen_ms <= HEARTBEAT_TIMEOUT_MS);
                if alive {
                    out.entry(key.user_id)
                        .or_insert((key.kind, key.tmdb_id, room.season, room.episode, room.paused));
                }
            }
        }
        out
    }

    fn now_ms() -> i64 {
        Utc::now().timestamp_millis()
    }

    /// True when at least one live player room actively loaded a file from `hash` (`file_index` set).
    pub fn has_playing_hash(&self, hash: &str) -> bool {
        self.has_playing_hash_at(hash, Self::now_ms())
    }

    fn has_playing_hash_at(&self, hash: &str, now_ms: i64) -> bool {
        if hash.is_empty() {
            return false;
        }
        let room_list: Vec<Arc<Mutex<Room>>> = {
            let rooms = self.rooms.lock().expect("sync rooms lock");
            rooms.values().cloned().collect()
        };
        room_list.into_iter().any(|room_arc| {
            let room = room_arc.lock().expect("sync room lock");
            room.file_index.is_some()
                && room.release.infohash.eq_ignore_ascii_case(hash)
                && room
                    .members
                    .iter()
                    .any(|m| now_ms - m.last_seen_ms <= HEARTBEAT_TIMEOUT_MS)
        })
    }

    /// Broadcasts `PreloadReady` to active players watching `(kind, tmdb_id)` when their download finishes.
    pub fn notify_preload_ready(
        &self,
        ready_users: &[i64],
        kind: MediaKind,
        tmdb_id: i64,
        season: i32,
        infohash: &str,
    ) {
        self.notify_preload_ready_at(ready_users, kind, tmdb_id, season, infohash, Self::now_ms());
    }

    fn notify_preload_ready_at(
        &self,
        ready_users: &[i64],
        kind: MediaKind,
        tmdb_id: i64,
        season: i32,
        infohash: &str,
        now_ms: i64,
    ) {
        if infohash.is_empty() {
            return;
        }
        let room_list: Vec<(RoomKey, Arc<Mutex<Room>>)> = {
            let rooms = self.rooms.lock().expect("sync rooms lock");
            rooms
                .iter()
                .filter(|(k, _)| k.kind == kind && k.tmdb_id == tmdb_id)
                .map(|(k, v)| (*k, v.clone()))
                .collect()
        };
        let msg = SyncServerMsg::PreloadReady {
            kind,
            tmdb_id,
            season,
            infohash: infohash.to_ascii_lowercase(),
        };
        for (key, room_arc) in room_list {
            let room = room_arc.lock().expect("sync room lock");
            let has_live = room
                .members
                .iter()
                .any(|m| now_ms - m.last_seen_ms <= HEARTBEAT_TIMEOUT_MS);
            if !has_live {
                continue;
            }
            if ready_users.contains(&key.user_id) || room.release.infohash.eq_ignore_ascii_case(infohash) {
                for m in &room.members {
                    if let Some(tx) = &m.tx {
                        let _ = tx.try_send(msg.clone());
                    }
                }
            }
        }
    }

    /// Checks whether an incoming HTTP `PUT /api/progress` is compatible with
    /// any currently active sync room (preventing stale devices from overwriting
    /// the room's live timecode).
    pub fn should_accept_http_progress(&self, user_id: i64, u: &ProgressUpdate) -> bool {
        let key = RoomKey {
            user_id,
            kind: u.kind,
            tmdb_id: u.tmdb_id,
        };
        let room_arc = {
            let rooms = self.rooms.lock().expect("sync rooms lock");
            rooms.get(&key).cloned()
        };
        let Some(room_arc) = room_arc else {
            return true;
        };
        let room = room_arc.lock().expect("sync room lock");
        if room.members.is_empty() {
            return true;
        }
        let (season, episode) = match u.kind {
            MediaKind::Movie => (0, 0),
            MediaKind::Tv => (u.season, u.episode),
        };
        if (room.season, room.episode) != (season, episode) {
            return is_finished(u.position_sec, u.duration_sec);
        }
        if is_finished(u.position_sec, u.duration_sec) {
            return true;
        }
        let live_pos = room.current_pos(Self::now_ms());
        (u.position_sec - live_pos).abs() <= 6.0
    }

    pub fn join_in_memory(
        &self,
        user_id: i64,
        req: &SyncJoinRequest,
        saved_progress: &[Progress],
        saved_audio_track: Option<String>,
        now_ms: i64,
    ) -> (SyncState, bool) {
        let key = RoomKey {
            user_id,
            kind: req.kind,
            tmdb_id: req.tmdb_id,
        };
        let (season, episode) = match req.kind {
            MediaKind::Movie => (0, 0),
            MediaKind::Tv => (req.season.max(1), req.episode.max(1)),
        };
        let saved_pos_for = |s: i32, e: i32| -> f64 {
            saved_progress
                .iter()
                .find(|p| p.season == s && p.episode == e && !p.finished)
                .map(|p| p.position_sec)
                .unwrap_or(0.0)
        };
        let device = if req.device.trim().is_empty() {
            "Устройство".to_owned()
        } else {
            req.device.trim().chars().take(64).collect()
        };

        let initial_pos = req
            .reconnect_pos
            .filter(|p| p.is_finite() && *p >= 0.0)
            .unwrap_or_else(|| saved_pos_for(season, episode));

        // Look up or insert the room atomically under `self.rooms` to eliminate join TOCTOU races.
        let (room_arc, created_new) = {
            let mut rooms = self.rooms.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = rooms.get(&key).cloned() {
                (existing, false)
            } else {
                // Cap active rooms per user so a single account cannot bloat hub memory.
                let user_rooms = rooms.keys().filter(|k| k.user_id == user_id).count();
                if user_rooms >= MAX_ROOMS_PER_USER {
                    if let Some(oldest_key) = rooms
                        .iter()
                        .filter(|(k, _)| k.user_id == user_id)
                        .min_by_key(|(_, r)| r.lock().unwrap_or_else(|e| e.into_inner()).anchor_at_ms)
                        .map(|(k, _)| *k)
                    {
                        rooms.remove(&oldest_key);
                    }
                }
                let room = Room {
                    key,
                    revision: 1,
                    season,
                    episode,
                    release: req.release.clone(),
                    file_index: None,
                    anchor_pos: initial_pos,
                    anchor_at_ms: now_ms,
                    duration_sec: None,
                    paused: false,
                    waiting: false,
                    waiting_for: Vec::new(),
                    rate: 1.0,
                    audio_track: saved_audio_track.clone(),
                    sound_client_id: Some(req.client_id.clone()),
                    last_device: device.clone(),
                    last_saved_ms: now_ms,
                    members: vec![Member {
                        client_id: req.client_id.clone(),
                        device: device.clone(),
                        conn_id: 0,
                        connected: false,
                        joined_at_ms: now_ms,
                        last_seen_ms: now_ms,
                        ready: false,
                        buffering: true,
                        waiting_since_ms: Some(now_ms),
                        tx: None,
                    }],
                };
                let arc = Arc::new(Mutex::new(room));
                rooms.insert(key, arc.clone());
                (arc, true)
            }
        };

        let mut room = room_arc.lock().unwrap_or_else(|e| e.into_inner());
        if created_new {
            let state = room.snapshot(now_ms, Some(req.client_id.clone()), SyncEventKind::Snapshot);
            return (state, false);
        }

        if room.members.is_empty() {
            room.revision += 1;
            room.season = season;
            room.episode = episode;
            room.release = req.release.clone();
            room.file_index = None;
            room.anchor_pos = initial_pos;
            room.anchor_at_ms = now_ms;
            room.duration_sec = None;
            room.paused = false;
            room.waiting = false;
            room.waiting_for.clear();
            room.rate = 1.0;
            room.audio_track = saved_audio_track;
            room.sound_client_id = Some(req.client_id.clone());
            room.last_device = device.clone();
            room.last_saved_ms = now_ms;
            room.members.push(Member {
                client_id: req.client_id.clone(),
                device,
                conn_id: 0,
                connected: false,
                joined_at_ms: now_ms,
                last_seen_ms: now_ms,
                ready: false,
                buffering: true,
                waiting_since_ms: Some(now_ms),
                tx: None,
            });
            let state = room.snapshot(now_ms, Some(req.client_id.clone()), SyncEventKind::Snapshot);
            return (state, false);
        }

        if req.explicit_episode && (room.season != season || room.episode != episode) {
            room.season = season;
            room.episode = episode;
            room.release = req.release.clone();
            room.file_index = None;
            room.anchor_pos = saved_pos_for(season, episode);
            room.anchor_at_ms = now_ms;
            room.duration_sec = None;
            room.paused = false;
            for m in &mut room.members {
                m.ready = false;
                m.buffering = true;
                m.waiting_since_ms = Some(now_ms);
            }
        } else if req.explicit_release && room.release.infohash != req.release.infohash {
            room.anchor_pos = room.current_pos(now_ms);
            room.anchor_at_ms = now_ms;
            room.release = req.release.clone();
            room.file_index = None;
            room.duration_sec = None;
            for m in &mut room.members {
                m.ready = false;
                m.buffering = true;
                m.waiting_since_ms = Some(now_ms);
            }
        }

        room.last_device = device.clone();
        if let Some(m) = room.members.iter_mut().find(|m| m.client_id == req.client_id) {
            m.device = device;
            m.joined_at_ms = now_ms;
            m.last_seen_ms = now_ms;
            if req.reconnect_pos.is_none() {
                m.ready = false;
                m.buffering = true;
                m.waiting_since_ms = Some(now_ms);
            }
        } else {
            if room.members.len() >= MAX_MEMBERS_PER_ROOM {
                // Evict the stalest member to keep broadcast work bounded.
                if let Some((stale_idx, _)) = room.members.iter().enumerate().min_by_key(|(_, m)| m.last_seen_ms) {
                    room.members.remove(stale_idx);
                }
            }
            room.members.push(Member {
                client_id: req.client_id.clone(),
                device,
                conn_id: 0,
                connected: false,
                joined_at_ms: now_ms,
                last_seen_ms: now_ms,
                ready: false,
                buffering: true,
                waiting_since_ms: Some(now_ms),
                tx: None,
            });
        }
        room.ensure_sound_owner();
        room.recompute_waiting(now_ms);
        room.revision += 1;
        let state = room.snapshot(now_ms, Some(req.client_id.clone()), SyncEventKind::Members);
        room.broadcast(&state);
        (state, true)
    }

    pub fn attach_ws(
        &self,
        claims: &SyncTicketClaims,
        tx: mpsc::Sender<SyncServerMsg>,
        now_ms: i64,
    ) -> Option<(RoomKey, u64, SyncState)> {
        let key = RoomKey {
            user_id: claims.uid,
            kind: claims.kind,
            tmdb_id: claims.tmdb_id,
        };
        let room_arc = {
            let rooms = self.rooms.lock().expect("sync rooms lock");
            rooms.get(&key).cloned()
        }?;
        let conn_id = self.next_conn_id.fetch_add(1, Ordering::Relaxed) + 1;
        let mut room = room_arc.lock().expect("sync room lock");
        let member = room.members.iter_mut().find(|m| m.client_id == claims.cid)?;
        member.conn_id = conn_id;
        member.connected = true;
        member.last_seen_ms = now_ms;
        member.tx = Some(tx);
        let state = room.snapshot(now_ms, None, SyncEventKind::Snapshot);
        Some((key, conn_id, state))
    }

    pub fn handle_ws_msg(
        &self,
        key: RoomKey,
        client_id: &str,
        conn_id: u64,
        msg: SyncClientMsg,
        episode_resume_pos: Option<f64>,
        now_ms: i64,
    ) -> Option<SaveJob> {
        let room_arc = {
            let rooms = self.rooms.lock().expect("sync rooms lock");
            rooms.get(&key).cloned()
        }?;
        let mut room = room_arc.lock().expect("sync room lock");
        let member_idx = room
            .members
            .iter()
            .position(|m| m.client_id == client_id && m.conn_id == conn_id)?;

        room.members[member_idx].last_seen_ms = now_ms;
        let device = room.members[member_idx].device.clone();

        match msg {
            SyncClientMsg::Ping { client_ms } => {
                if let Some(tx) = &room.members[member_idx].tx {
                    let _ = tx.try_send(SyncServerMsg::Pong {
                        client_ms,
                        server_ms: now_ms,
                    });
                }
                None
            }
            SyncClientMsg::Ready {
                position_sec,
                duration_sec,
                file_index,
            } => {
                if let Some(d) = sanitize_duration(duration_sec) {
                    room.duration_sec = Some(d);
                }
                if let Some(fi) = file_index.filter(|i| *i >= 0) {
                    room.file_index = Some(fi);
                }
                let was_waiting = room.waiting;
                let changed = !room.members[member_idx].ready || room.members[member_idx].buffering;
                room.members[member_idx].ready = true;
                room.members[member_idx].buffering = false;
                room.members[member_idx].waiting_since_ms = None;
                if room.members.len() == 1 && !room.paused && position_sec.is_finite() && position_sec >= 0.0 {
                    room.anchor_pos = sanitize_pos(position_sec, room.duration_sec);
                    room.anchor_at_ms = now_ms;
                }
                room.recompute_waiting(now_ms);
                if was_waiting && !room.waiting {
                    room.revision += 1;
                    let state = room.snapshot(now_ms, None, SyncEventKind::Resumed);
                    room.broadcast(&state);
                } else if changed {
                    room.revision += 1;
                    let state = room.snapshot(now_ms, Some(client_id.to_owned()), SyncEventKind::Members);
                    room.broadcast(&state);
                }
                None
            }
            SyncClientMsg::Buffering { position_sec } => {
                let mut changed = !room.members[member_idx].buffering;
                room.members[member_idx].buffering = true;
                if room.members[member_idx].waiting_since_ms.is_none() {
                    room.members[member_idx].waiting_since_ms = Some(now_ms);
                }
                if room.members.len() > 1 && !room.paused && !room.waiting {
                    let live = room.current_pos(now_ms);
                    let pos = if position_sec.is_finite() && (live - position_sec).abs() <= 5.0 {
                        sanitize_pos(position_sec, room.duration_sec)
                    } else {
                        live
                    };
                    room.anchor_pos = pos;
                    room.anchor_at_ms = now_ms;
                    changed = true;
                }
                room.recompute_waiting(now_ms);
                if changed {
                    room.revision += 1;
                    let state = room.snapshot(now_ms, Some(client_id.to_owned()), SyncEventKind::Buffering);
                    room.broadcast(&state);
                }
                None
            }
            SyncClientMsg::Play { position_sec } => {
                room.last_device = device;
                room.anchor_pos = if room.paused || room.members.len() <= 1 {
                    sanitize_pos(position_sec, room.duration_sec)
                } else {
                    room.current_pos(now_ms)
                };
                room.anchor_at_ms = now_ms;
                room.paused = false;
                room.members[member_idx].ready = true;
                room.members[member_idx].buffering = false;
                room.members[member_idx].waiting_since_ms = None;
                room.recompute_waiting(now_ms);
                room.revision += 1;
                let state = room.snapshot(now_ms, Some(client_id.to_owned()), SyncEventKind::Play);
                room.broadcast(&state);
                None
            }
            SyncClientMsg::Pause { position_sec } => {
                room.last_device = device;
                let pos = sanitize_pos(position_sec, room.duration_sec);
                room.anchor_pos = pos;
                room.anchor_at_ms = now_ms;
                room.paused = true;
                room.recompute_waiting(now_ms);
                room.revision += 1;
                let state = room.snapshot(now_ms, Some(client_id.to_owned()), SyncEventKind::Pause);
                room.broadcast(&state);
                room.make_save_job(now_ms, Some(pos), false)
            }
            SyncClientMsg::Seek { position_sec } => {
                room.last_device = device;
                let pos = sanitize_pos(position_sec, room.duration_sec);
                room.anchor_pos = pos;
                room.anchor_at_ms = now_ms;
                if room.members.len() > 1 {
                    for m in &mut room.members {
                        m.ready = false;
                        m.buffering = true;
                        m.waiting_since_ms = Some(now_ms);
                    }
                }
                room.recompute_waiting(now_ms);
                room.revision += 1;
                let state = room.snapshot(now_ms, Some(client_id.to_owned()), SyncEventKind::Seek);
                room.broadcast(&state);
                room.make_save_job(now_ms, Some(pos), false)
            }
            SyncClientMsg::Rate { rate, position_sec } => {
                room.last_device = device;
                room.anchor_pos = sanitize_pos(position_sec, room.duration_sec);
                room.anchor_at_ms = now_ms;
                room.rate = sanitize_rate(rate);
                room.revision += 1;
                let state = room.snapshot(now_ms, Some(client_id.to_owned()), SyncEventKind::Rate);
                room.broadcast(&state);
                None
            }
            SyncClientMsg::Episode {
                season,
                episode,
                release,
            } => {
                if room.key.kind != MediaKind::Tv || season < 0 || episode < 1 {
                    return None;
                }
                if room.season == season && room.episode == episode {
                    return None;
                }
                // Flush the previous episode if it was near the end? The client
                // already sends `save(true)` on ended / close, and we start the new episode cleanly.
                room.last_device = device;
                room.season = season;
                room.episode = episode;
                if let Some(mut rel) = release {
                    if !infohash_ok(&rel.infohash) {
                        return None;
                    }
                    rel.infohash.make_ascii_lowercase();
                    rel.title = rel.title.map(|s| s.chars().take(300).collect());
                    rel.quality = rel.quality.map(|s| s.chars().take(64).collect());
                    rel.voice = rel.voice.map(|s| s.chars().take(128).collect());
                    room.release = rel;
                    room.file_index = None;
                }
                room.anchor_pos = episode_resume_pos.filter(|p| p.is_finite() && *p >= 0.0).unwrap_or(0.0);
                room.anchor_at_ms = now_ms;
                room.duration_sec = None;
                room.paused = false;
                for m in &mut room.members {
                    m.ready = false;
                    m.buffering = true;
                    m.waiting_since_ms = Some(now_ms);
                }
                room.recompute_waiting(now_ms);
                room.revision += 1;
                let state = room.snapshot(now_ms, Some(client_id.to_owned()), SyncEventKind::Episode);
                room.broadcast(&state);
                None
            }
            SyncClientMsg::Release { mut release } => {
                if !infohash_ok(&release.infohash) {
                    return None;
                }
                release.infohash.make_ascii_lowercase();
                release.title = release.title.map(|s| s.chars().take(300).collect());
                release.quality = release.quality.map(|s| s.chars().take(64).collect());
                release.voice = release.voice.map(|s| s.chars().take(128).collect());
                room.last_device = device;
                room.anchor_pos = room.current_pos(now_ms);
                room.anchor_at_ms = now_ms;
                room.release = release;
                room.file_index = None;
                room.duration_sec = None;
                if room.members.len() > 1 {
                    for m in &mut room.members {
                        m.ready = false;
                        m.buffering = true;
                        m.waiting_since_ms = Some(now_ms);
                    }
                }
                room.recompute_waiting(now_ms);
                room.revision += 1;
                let state = room.snapshot(now_ms, Some(client_id.to_owned()), SyncEventKind::Release);
                room.broadcast(&state);
                None
            }
            SyncClientMsg::Audio {
                audio_track,
                position_sec,
            } => {
                room.last_device = device;
                room.anchor_pos = sanitize_pos(position_sec, room.duration_sec);
                room.anchor_at_ms = now_ms;
                room.audio_track = audio_track.map(|s| s.chars().take(200).collect());
                if room.members.len() > 1 {
                    for m in &mut room.members {
                        m.ready = false;
                        m.buffering = true;
                        m.waiting_since_ms = Some(now_ms);
                    }
                }
                room.recompute_waiting(now_ms);
                room.revision += 1;
                let state = room.snapshot(now_ms, Some(client_id.to_owned()), SyncEventKind::Audio);
                room.broadcast(&state);
                None
            }
            SyncClientMsg::Sound { client_id: target_id } => {
                if !room.members.iter().any(|m| m.client_id == target_id) {
                    return None;
                }
                if room.sound_client_id.as_deref() == Some(target_id.as_str()) {
                    return None;
                }
                room.last_device = device;
                room.sound_client_id = Some(target_id);
                room.revision += 1;
                let state = room.snapshot(now_ms, Some(client_id.to_owned()), SyncEventKind::Sound);
                room.broadcast(&state);
                None
            }
            SyncClientMsg::Report {
                position_sec,
                duration_sec,
                paused,
                rate,
            } => {
                if let Some(d) = sanitize_duration(duration_sec) {
                    room.duration_sec = Some(d);
                }
                // Play/pause state transitions come explicitly from `Play`/`Pause`:
                // a periodic telemetry `Report` (e.g. while loading or seeking) must never pause the room.
                if room.members.len() == 1 && !room.paused && !room.waiting && !paused {
                    room.anchor_pos = sanitize_pos(position_sec, room.duration_sec);
                    room.anchor_at_ms = now_ms;
                    room.rate = sanitize_rate(rate);
                }
                if !room.paused && !room.waiting && now_ms - room.last_saved_ms >= AUTO_SAVE_INTERVAL_MS {
                    return room.make_save_job(now_ms, None, false);
                }
                None
            }
            SyncClientMsg::Leave {
                position_sec,
                duration_sec,
                finished,
            } => {
                if let Some(d) = sanitize_duration(duration_sec) {
                    room.duration_sec = Some(d);
                }
                let pos_override = if room.members.len() == 1 && position_sec.is_finite() && position_sec >= 0.0 {
                    Some(position_sec)
                } else {
                    None
                };
                let job = room.make_save_job(now_ms, pos_override, finished);
                drop(room);
                self.remove_member(key, client_id, Some(conn_id), now_ms);
                job
            }
        }
    }

    pub fn leave_http(&self, user_id: i64, req: &SyncLeaveRequest, now_ms: i64) -> Option<SaveJob> {
        let key = RoomKey {
            user_id,
            kind: req.kind,
            tmdb_id: req.tmdb_id,
        };
        let room_arc = {
            let rooms = self.rooms.lock().expect("sync rooms lock");
            rooms.get(&key).cloned()
        };
        let job = if let Some(room_arc) = room_arc {
            let mut room = room_arc.lock().expect("sync room lock");
            if let Some(d) = sanitize_duration(req.duration_sec) {
                room.duration_sec = Some(d);
            }
            let pos_override = if room.members.len() <= 1 && req.position_sec.is_finite() && req.position_sec >= 0.0 {
                Some(req.position_sec)
            } else {
                None
            };
            room.make_save_job(now_ms, pos_override, req.finished)
        } else {
            None
        };
        self.remove_member(key, &req.client_id, None, now_ms);
        job
    }

    /// Removes a member when its WebSocket drops or explicit leave arrives.
    /// When `expected_conn_id` is `Some`, only removes if the member still holds that connection.
    pub fn remove_member(
        &self,
        key: RoomKey,
        client_id: &str,
        expected_conn_id: Option<u64>,
        now_ms: i64,
    ) -> Option<SaveJob> {
        let room_arc = {
            let rooms = self.rooms.lock().expect("sync rooms lock");
            rooms.get(&key).cloned()
        }?;
        let mut save_job = None;
        let is_empty = {
            let mut room = room_arc.lock().expect("sync room lock");
            let idx = room.members.iter().position(|m| m.client_id == client_id)?;
            if expected_conn_id.is_some_and(|cid| room.members[idx].conn_id != cid) {
                return None;
            }
            if room.members.len() == 1 {
                save_job = room.make_save_job(now_ms, None, false);
            }
            let was_waiting = room.waiting;
            room.members.remove(idx);
            if room.members.is_empty() {
                room.sound_client_id = None;
                true
            } else {
                room.ensure_sound_owner();
                room.recompute_waiting(now_ms);
                room.revision += 1;
                let event = if was_waiting && !room.waiting {
                    SyncEventKind::Resumed
                } else {
                    SyncEventKind::Members
                };
                let state = room.snapshot(now_ms, None, event);
                room.broadcast(&state);
                false
            }
        };
        if is_empty {
            let mut rooms = self.rooms.lock().expect("sync rooms lock");
            let remove = rooms.get(&key).is_some_and(|current| {
                Arc::ptr_eq(current, &room_arc) && current.lock().expect("sync room lock").members.is_empty()
            });
            if remove {
                rooms.remove(&key);
            }
        }
        save_job
    }

    /// Periodic sweep: drops dead connections, releases stalled buffering barriers,
    /// and flushes periodic DB saves for active playing rooms.
    pub fn sweep(&self, now_ms: i64) -> Vec<SaveJob> {
        let room_list: Vec<(RoomKey, Arc<Mutex<Room>>)> = {
            let rooms = self.rooms.lock().expect("sync rooms lock");
            rooms.iter().map(|(k, v)| (*k, v.clone())).collect()
        };
        let mut saves = Vec::new();
        let mut empty_keys = Vec::new();

        for (key, room_arc) in room_list {
            let mut room = room_arc.lock().expect("sync room lock");
            let before = room.members.len();
            let was_waiting = room.waiting;

            room.members.retain(|m| {
                if !m.connected {
                    now_ms - m.joined_at_ms <= JOIN_HANDSHAKE_TIMEOUT_MS
                } else {
                    now_ms - m.last_seen_ms <= HEARTBEAT_TIMEOUT_MS
                }
            });

            if room.members.is_empty() {
                if before > 0 {
                    saves.extend(room.make_save_job(now_ms, None, false));
                }
                empty_keys.push((key, room_arc.clone()));
                continue;
            }

            let mut barrier_released = false;
            for m in &mut room.members {
                if m.waiting_since_ms
                    .is_some_and(|since| now_ms - since >= BARRIER_TIMEOUT_MS)
                {
                    m.ready = true;
                    m.buffering = false;
                    m.waiting_since_ms = None;
                    barrier_released = true;
                }
            }

            if room.members.len() != before || barrier_released {
                room.ensure_sound_owner();
                room.recompute_waiting(now_ms);
                room.revision += 1;
                let event = if was_waiting && !room.waiting {
                    SyncEventKind::Resumed
                } else {
                    SyncEventKind::Members
                };
                let state = room.snapshot(now_ms, None, event);
                room.broadcast(&state);
            }

            if !room.paused && !room.waiting && now_ms - room.last_saved_ms >= AUTO_SAVE_INTERVAL_MS {
                saves.extend(room.make_save_job(now_ms, None, false));
            }
        }

        if !empty_keys.is_empty() {
            let mut rooms = self.rooms.lock().expect("sync rooms lock");
            for (key, arc) in empty_keys {
                let remove = rooms.get(&key).is_some_and(|cur| {
                    Arc::ptr_eq(cur, &arc) && cur.lock().expect("sync room lock").members.is_empty()
                });
                if remove {
                    rooms.remove(&key);
                }
            }
        }

        saves
    }
}

pub fn spawn_reaper(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            let jobs = state.sync.sweep(SyncHub::now_ms());
            for job in jobs {
                let st = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = persist_progress(&st, job.user_id, job.update).await {
                        tracing::debug!(error = ?e, "sync background progress save failed");
                    }
                });
            }
        }
    });
}

pub async fn join(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(mut req): Json<SyncJoinRequest>,
) -> Result<Json<SyncJoinResponse>, AppError> {
    if req.tmdb_id <= 0 || req.season < 0 || req.episode < 0 || req.client_id.trim().is_empty() {
        return Err(AppError::bad_request("invalid sync join parameters"));
    }
    if req.client_id.len() > 128 {
        return Err(AppError::bad_request("client_id too long"));
    }
    if !infohash_ok(&req.release.infohash) {
        return Err(AppError::bad_request("invalid release infohash"));
    }
    req.release.infohash.make_ascii_lowercase();
    req.release.kind = req.kind;
    req.release.tmdb_id = req.tmdb_id;

    let saved_progress = db::title_progress(&state.pool, user.0, req.kind, req.tmdb_id).await?;
    let title_state = userdata::get_state(&state.pool, user.0, req.kind, req.tmdb_id).await?;
    let now = Utc::now();
    let (sync_state, joined_existing) = state.sync.join_in_memory(
        user.0,
        &req,
        &saved_progress,
        title_state.audio_track,
        now.timestamp_millis(),
    );

    let ticket = issue_ticket(
        &state.config.session_secret,
        &SyncTicketClaims {
            uid: user.0,
            kind: req.kind,
            tmdb_id: req.tmdb_id,
            cid: req.client_id,
            exp: (now + chrono::Duration::seconds(TICKET_TTL_SECS)).timestamp(),
        },
    );

    Ok(Json(SyncJoinResponse {
        ticket,
        state: sync_state,
        joined_existing,
    }))
}

pub async fn leave(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<SyncLeaveRequest>,
) -> Result<axum::http::StatusCode, AppError> {
    if req.tmdb_id <= 0 || req.client_id.trim().is_empty() {
        return Err(AppError::bad_request("invalid sync leave parameters"));
    }
    if let Some(job) = state.sync.leave_http(user.0, &req, SyncHub::now_ms()) {
        persist_progress(&state, job.user_id, job.update).await?;
    }
    state
        .media
        .pause_cache
        .on_player_leave(user.0, req.kind, req.tmdb_id, req.position_sec);
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct WsQuery {
    ticket: String,
}

pub async fn ws(
    State(state): State<AppState>,
    Query(q): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, AppError> {
    let claims = verify_ticket(&state.config.session_secret, &q.ticket, Utc::now())
        .ok_or_else(|| AppError::unauthorized("invalid or expired sync ticket"))?;
    Ok(ws
        .max_message_size(WS_MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(state, claims, socket)))
}

async fn handle_socket(state: AppState, claims: SyncTicketClaims, socket: WebSocket) {
    let (tx, mut rx) = mpsc::channel::<SyncServerMsg>(WS_OUTBOUND_CAPACITY);
    let Some((key, conn_id, initial)) = state.sync.attach_ws(&claims, tx, SyncHub::now_ms()) else {
        return;
    };

    let (mut ws_tx, mut ws_rx) = socket.split();
    if let Ok(json) = serde_json::to_string(&SyncServerMsg::State(Box::new(initial))) {
        if ws_tx.send(Message::Text(json.into())).await.is_err() {
            state
                .sync
                .remove_member(key, &claims.cid, Some(conn_id), SyncHub::now_ms());
            return;
        }
    }

    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let Ok(json) = serde_json::to_string(&msg) else {
                continue;
            };
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(frame)) = ws_rx.next().await {
        let text = match frame {
            Message::Text(t) => t,
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => continue,
        };
        let Ok(msg) = serde_json::from_str::<SyncClientMsg>(&text) else {
            continue;
        };

        let episode_resume = match &msg {
            SyncClientMsg::Episode { season, episode, .. } => {
                db::title_progress(&state.pool, claims.uid, key.kind, key.tmdb_id)
                    .await
                    .ok()
                    .and_then(|list| {
                        list.into_iter()
                            .find(|p| p.season == *season && p.episode == *episode && !p.finished)
                            .map(|p| p.position_sec)
                    })
            }
            _ => None,
        };

        let is_leave = matches!(msg, SyncClientMsg::Leave { .. });
        let job = state
            .sync
            .handle_ws_msg(key, &claims.cid, conn_id, msg, episode_resume, SyncHub::now_ms());
        if let Some(job) = job {
            if is_leave {
                if let Err(e) = persist_progress(&state, job.user_id, job.update).await {
                    tracing::debug!(error = ?e, "sync progress save failed");
                }
            } else {
                let st = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = persist_progress(&st, job.user_id, job.update).await {
                        tracing::debug!(error = ?e, "sync progress save failed");
                    }
                });
            }
        }
        if is_leave {
            break;
        }
    }

    if let Some(job) = state
        .sync
        .remove_member(key, &claims.cid, Some(conn_id), SyncHub::now_ms())
    {
        if let Err(e) = persist_progress(&state, job.user_id, job.update).await {
            tracing::debug!(error = ?e, "sync disconnect progress save failed");
        }
    }
    send_task.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(ch: char) -> ReleaseChoiceUpdate {
        ReleaseChoiceUpdate {
            kind: MediaKind::Movie,
            tmdb_id: 603,
            season: 0,
            episode: 0,
            infohash: ch.to_string().repeat(40),
            title: Some(format!("Release {ch}")),
            quality: Some("1080p".into()),
            quality_key: Some("1080p".into()),
            voice: Some("Дубляж".into()),
        }
    }

    fn join_req(cid: &str, device: &str) -> SyncJoinRequest {
        SyncJoinRequest {
            client_id: cid.into(),
            device: device.into(),
            kind: MediaKind::Movie,
            tmdb_id: 603,
            season: 0,
            episode: 0,
            release: rel('a'),
            explicit_episode: false,
            explicit_release: false,
            reconnect_pos: None,
        }
    }

    #[test]
    fn sync_messages_round_trip() {
        let client_msgs = vec![
            SyncClientMsg::Ready {
                position_sec: 12.5,
                duration_sec: Some(1200.0),
                file_index: Some(2),
            },
            SyncClientMsg::Buffering { position_sec: 45.0 },
            SyncClientMsg::Play { position_sec: 45.0 },
            SyncClientMsg::Pause { position_sec: 46.2 },
            SyncClientMsg::Seek { position_sec: 300.0 },
            SyncClientMsg::Rate {
                rate: 1.5,
                position_sec: 301.0,
            },
            SyncClientMsg::Episode {
                season: 2,
                episode: 3,
                release: None,
            },
            SyncClientMsg::Audio {
                audio_track: Some("LostFilm · Русский".into()),
                position_sec: 305.0,
            },
            SyncClientMsg::Sound { client_id: "c1".into() },
            SyncClientMsg::Report {
                position_sec: 310.0,
                duration_sec: Some(1200.0),
                paused: false,
                rate: 1.5,
            },
            SyncClientMsg::Ping {
                client_ms: 1_700_000_000_123,
            },
            SyncClientMsg::Leave {
                position_sec: 315.0,
                duration_sec: Some(1200.0),
                finished: false,
            },
        ];
        for m in client_msgs {
            let json = serde_json::to_string(&m).expect("serialize client msg");
            let decoded: SyncClientMsg = serde_json::from_str(&json).expect("deserialize client msg");
            assert_eq!(m, decoded);
        }

        let server_msgs = vec![SyncServerMsg::PreloadReady {
            kind: MediaKind::Movie,
            tmdb_id: 603,
            season: 0,
            infohash: "a".repeat(40),
        }];
        for m in &server_msgs {
            let json = serde_json::to_string(m).expect("serialize server msg");
            let decoded: SyncServerMsg = serde_json::from_str(&json).expect("deserialize server msg");
            assert_eq!(&decoded, m);
        }

        let server_msg = SyncServerMsg::State(Box::new(SyncState {
            revision: 7,
            kind: MediaKind::Movie,
            tmdb_id: 603,
            season: 0,
            episode: 0,
            release: rel('a'),
            position_sec: 120.5,
            duration_sec: Some(8160.0),
            paused: false,
            waiting: true,
            waiting_for: vec!["Телефон".into()],
            rate: 1.25,
            audio_track: Some("Дубляж".into()),
            sound_client_id: Some("c1".into()),
            server_now_ms: 1_700_000_000_500,
            updated_by: Some("c1".into()),
            event: SyncEventKind::Seek,
            members: vec![SyncMemberInfo {
                client_id: "c1".into(),
                device: "ПК".into(),
                ready: true,
                buffering: false,
            }],
        }));
        let json = serde_json::to_string(&server_msg).expect("serialize server msg");
        let decoded: SyncServerMsg = serde_json::from_str(&json).expect("deserialize server msg");
        assert_eq!(server_msg, decoded);
    }

    #[test]
    fn ticket_round_trip_and_expiry() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let now = Utc::now();
        let claims = SyncTicketClaims {
            uid: 7,
            kind: MediaKind::Movie,
            tmdb_id: 603,
            cid: "dev1".into(),
            exp: (now + chrono::Duration::seconds(60)).timestamp(),
        };
        let t = issue_ticket(secret, &claims);
        assert_eq!(verify_ticket(secret, &t, now), Some(claims));
        assert_eq!(verify_ticket(secret, &t, now + chrono::Duration::seconds(120)), None);
        // Session token cannot pass as sync ticket.
        let s = crate::session::issue(secret, 7, now + chrono::Duration::hours(1));
        assert_eq!(verify_ticket(secret, &s, now), None);
    }

    #[test]
    fn multi_device_barrier_sync_and_leave() {
        let hub = SyncHub::new();
        let mut t = 1_000_000_i64;

        // Device A joins alone -> no barrier, starts from saved progress 120s.
        let saved = vec![Progress {
            kind: MediaKind::Movie,
            tmdb_id: 603,
            season: 0,
            episode: 0,
            position_sec: 120.0,
            duration_sec: 5000.0,
            finished: false,
            release: None,
            updated_at: Utc::now(),
        }];
        let (s1, joined) = hub.join_in_memory(1, &join_req("a", "ПК"), &saved, None, t);
        assert!(!joined);
        assert!(!s1.waiting);
        assert_eq!(s1.position_sec, 120.0);

        let (tx_a, mut rx_a) = mpsc::channel(WS_OUTBOUND_CAPACITY);
        let claims_a = SyncTicketClaims {
            uid: 1,
            kind: MediaKind::Movie,
            tmdb_id: 603,
            cid: "a".into(),
            exp: 9999999999,
        };
        let (key, conn_a, _) = hub.attach_ws(&claims_a, tx_a, t).unwrap();
        hub.handle_ws_msg(
            key,
            "a",
            conn_a,
            SyncClientMsg::Ready {
                position_sec: 120.0,
                duration_sec: Some(5000.0),
                file_index: Some(0),
            },
            None,
            t,
        );
        while rx_a.try_recv().is_ok() {}

        // Advance 10 seconds -> Device A is at 130.0s.
        t += 10_000;

        // Device B joins -> room enters waiting barrier for B, frozen at ~130.0s.
        let mut req_b = join_req("b", "Телефон");
        req_b.release = rel('b'); // non-explicit release should adopt A's release 'a'!
        let (s2, joined2) = hub.join_in_memory(1, &req_b, &saved, None, t);
        assert!(joined2);
        assert!(s2.waiting);
        assert_eq!(s2.waiting_for, vec!["Телефон"]);
        assert_eq!(s2.release.infohash, rel('a').infohash);
        assert!((s2.position_sec - 130.0).abs() < 0.01);

        // Device A received the waiting broadcast.
        let msg_a = rx_a.try_recv().unwrap();
        match msg_a {
            SyncServerMsg::State(st) => {
                assert!(st.waiting);
                assert_eq!(st.members.len(), 2);
            }
            _ => panic!("expected state"),
        }

        // Stale HTTP progress at 10.0s must be rejected while room is at 130.0s.
        assert!(!hub.should_accept_http_progress(
            1,
            &ProgressUpdate {
                kind: MediaKind::Movie,
                tmdb_id: 603,
                season: 0,
                episode: 0,
                position_sec: 10.0,
                duration_sec: 5000.0,
                release: None,
                device: None,
            }
        ));

        let (tx_b, _rx_b) = mpsc::channel(WS_OUTBOUND_CAPACITY);
        let claims_b = SyncTicketClaims {
            uid: 1,
            kind: MediaKind::Movie,
            tmdb_id: 603,
            cid: "b".into(),
            exp: 9999999999,
        };
        let (_, conn_b, _) = hub.attach_ws(&claims_b, tx_b, t).unwrap();

        // 4 seconds pass while B buffers -> position MUST stay frozen at 130.0s!
        t += 4_000;
        hub.handle_ws_msg(
            key,
            "b",
            conn_b,
            SyncClientMsg::Ready {
                position_sec: 130.0,
                duration_sec: Some(5000.0),
                file_index: Some(0),
            },
            None,
            t,
        );

        let resumed = rx_a.try_recv().unwrap();
        match resumed {
            SyncServerMsg::State(st) => {
                assert!(!st.waiting);
                assert_eq!(st.event, SyncEventKind::Resumed);
                assert!((st.position_sec - 130.0).abs() < 0.01);
            }
            _ => panic!("expected resumed state"),
        }

        // Speed change + 2s playback + pause.
        t += 2_000; // now at 132.0s
        hub.handle_ws_msg(
            key,
            "b",
            conn_b,
            SyncClientMsg::Rate {
                rate: 1.5,
                position_sec: 132.0,
            },
            None,
            t,
        );
        let rate_st = rx_a.try_recv().unwrap();
        match rate_st {
            SyncServerMsg::State(st) => {
                assert_eq!(st.rate, 1.5);
                assert_eq!(st.event, SyncEventKind::Rate);
            }
            _ => panic!("expected rate"),
        }

        // Device B leaves -> Device A continues alone without pausing.
        hub.remove_member(key, "b", Some(conn_b), t);
        let left_st = rx_a.try_recv().unwrap();
        match left_st {
            SyncServerMsg::State(st) => {
                assert_eq!(st.members.len(), 1);
                assert!(!st.paused);
                assert!(!st.waiting);
            }
            _ => panic!("expected member leave"),
        }
    }

    #[test]
    fn barrier_timeout_unblocks_healthy_peer() {
        let hub = SyncHub::new();
        let mut t = 2_000_000_i64;
        hub.join_in_memory(1, &join_req("a", "ПК"), &[], None, t);
        let (tx_a, mut rx_a) = mpsc::channel(WS_OUTBOUND_CAPACITY);
        let claims_a = SyncTicketClaims {
            uid: 1,
            kind: MediaKind::Movie,
            tmdb_id: 603,
            cid: "a".into(),
            exp: 9999999999,
        };
        let (key, conn_a, _) = hub.attach_ws(&claims_a, tx_a, t).unwrap();
        hub.handle_ws_msg(
            key,
            "a",
            conn_a,
            SyncClientMsg::Ready {
                position_sec: 0.0,
                duration_sec: Some(3600.0),
                file_index: None,
            },
            None,
            t,
        );

        hub.join_in_memory(1, &join_req("b", "Телефон"), &[], None, t);
        let (tx_b, _) = mpsc::channel(WS_OUTBOUND_CAPACITY);
        let claims_b = SyncTicketClaims {
            uid: 1,
            kind: MediaKind::Movie,
            tmdb_id: 603,
            cid: "b".into(),
            exp: 9999999999,
        };
        let (_, conn_b, _) = hub.attach_ws(&claims_b, tx_b, t).unwrap();
        while rx_a.try_recv().is_ok() {}

        // Keep B's heartbeat alive via Ping, but never send Ready.
        t += 15_000;
        hub.handle_ws_msg(key, "a", conn_a, SyncClientMsg::Ping { client_ms: t }, None, t);
        hub.handle_ws_msg(key, "b", conn_b, SyncClientMsg::Ping { client_ms: t }, None, t);
        let _ = rx_a.try_recv(); // Pong

        t += 16_000;
        hub.handle_ws_msg(key, "a", conn_a, SyncClientMsg::Ping { client_ms: t }, None, t);
        hub.handle_ws_msg(key, "b", conn_b, SyncClientMsg::Ping { client_ms: t }, None, t);
        let _ = rx_a.try_recv(); // Pong

        // Sweep past BARRIER_TIMEOUT_MS -> barrier releases!
        hub.sweep(t);
        let msg = rx_a.try_recv().unwrap();
        match msg {
            SyncServerMsg::State(st) => {
                assert!(!st.waiting);
                assert_eq!(st.event, SyncEventKind::Resumed);
            }
            _ => panic!("expected barrier timeout resume"),
        }
    }

    #[test]
    fn report_does_not_pause_solo_room() {
        let hub = SyncHub::new();
        let t = 3_000_000_i64;
        let (s1, _) = hub.join_in_memory(1, &join_req("a", "ПК"), &[], None, t);
        assert!(!s1.paused);

        let (tx_a, _rx_a) = mpsc::channel(WS_OUTBOUND_CAPACITY);
        let claims_a = SyncTicketClaims {
            uid: 1,
            kind: MediaKind::Movie,
            tmdb_id: 603,
            cid: "a".into(),
            exp: 9_999_999_999,
        };
        let (key, conn_a, _) = hub.attach_ws(&claims_a, tx_a, t).unwrap();

        // A transient `paused: true` telemetry report (e.g. while loading/seeking)
        // must not flip `room.paused` or reset anchor_pos to 0.
        hub.handle_ws_msg(
            key,
            "a",
            conn_a,
            SyncClientMsg::Seek { position_sec: 1942.0 },
            None,
            t + 500,
        );
        hub.handle_ws_msg(
            key,
            "a",
            conn_a,
            SyncClientMsg::Report {
                position_sec: 0.0,
                duration_sec: Some(7200.0),
                paused: true,
                rate: 1.0,
            },
            None,
            t + 1_000,
        );
        hub.handle_ws_msg(
            key,
            "a",
            conn_a,
            SyncClientMsg::Ready {
                position_sec: 1942.0,
                duration_sec: Some(7200.0),
                file_index: Some(0),
            },
            None,
            t + 1_500,
        );

        let (_, rejoined) = hub.join_in_memory(1, &join_req("b", "Телефон"), &[], None, t + 1_600);
        assert!(rejoined);
        let state = hub.join_in_memory(1, &join_req("b", "Телефон"), &[], None, t + 1_600).0;
        assert!(!state.paused);
        assert!((state.position_sec - 1942.0).abs() < 1.0);
    }

    #[test]
    fn multi_device_sound_ownership_and_handoff() {
        let hub = SyncHub::new();
        let mut t = 4_000_000_i64;

        // First device owns sound by default.
        let (s1, _) = hub.join_in_memory(1, &join_req("a", "ПК"), &[], None, t);
        assert_eq!(s1.sound_client_id.as_deref(), Some("a"));

        let (tx_a, mut rx_a) = mpsc::channel(WS_OUTBOUND_CAPACITY);
        let claims_a = SyncTicketClaims {
            uid: 1,
            kind: MediaKind::Movie,
            tmdb_id: 603,
            cid: "a".into(),
            exp: 9_999_999_999,
        };
        let (key, conn_a, _) = hub.attach_ws(&claims_a, tx_a, t).unwrap();

        // Second device joins -> existing owner ("a") retains sound.
        let (s2, joined) = hub.join_in_memory(1, &join_req("b", "Телефон"), &[], None, t + 100);
        assert!(joined);
        assert_eq!(s2.sound_client_id.as_deref(), Some("a"));

        let (tx_b, mut rx_b) = mpsc::channel(WS_OUTBOUND_CAPACITY);
        let claims_b = SyncTicketClaims {
            uid: 1,
            kind: MediaKind::Movie,
            tmdb_id: 603,
            cid: "b".into(),
            exp: 9_999_999_999,
        };
        let (_, conn_b, _) = hub.attach_ws(&claims_b, tx_b, t + 100).unwrap();
        while rx_a.try_recv().is_ok() {}
        while rx_b.try_recv().is_ok() {}

        // Invalid target is ignored.
        hub.handle_ws_msg(
            key,
            "a",
            conn_a,
            SyncClientMsg::Sound {
                client_id: "ghost".into(),
            },
            None,
            t + 200,
        );
        assert!(rx_a.try_recv().is_err());

        // Switching sound to "b" broadcasts SyncEventKind::Sound to both devices.
        hub.handle_ws_msg(
            key,
            "a",
            conn_a,
            SyncClientMsg::Sound { client_id: "b".into() },
            None,
            t + 300,
        );
        for rx in [&mut rx_a, &mut rx_b] {
            match rx.try_recv().unwrap() {
                SyncServerMsg::State(st) => {
                    assert_eq!(st.event, SyncEventKind::Sound);
                    assert_eq!(st.sound_client_id.as_deref(), Some("b"));
                    assert_eq!(st.updated_by.as_deref(), Some("a"));
                }
                _ => panic!("expected state"),
            }
        }

        // Redundant switch to the same owner does not emit an extra broadcast.
        hub.handle_ws_msg(
            key,
            "b",
            conn_b,
            SyncClientMsg::Sound { client_id: "b".into() },
            None,
            t + 400,
        );
        assert!(rx_a.try_recv().is_err());

        // Sound owner leaves -> sound automatically hands off to remaining member ("a").
        hub.remove_member(key, "b", Some(conn_b), t + 500);
        match rx_a.try_recv().unwrap() {
            SyncServerMsg::State(st) => {
                assert_eq!(st.members.len(), 1);
                assert_eq!(st.sound_client_id.as_deref(), Some("a"));
            }
            _ => panic!("expected member leave handoff"),
        }

        // Rejoin "b", switch sound to "b", then let "b" time out in sweep() -> handoff back to "a".
        t += 1_000;
        hub.join_in_memory(1, &join_req("b", "Телефон"), &[], None, t);
        let (_, conn_b2, _) = hub
            .attach_ws(&claims_b, mpsc::channel(WS_OUTBOUND_CAPACITY).0, t)
            .unwrap();
        hub.handle_ws_msg(
            key,
            "a",
            conn_a,
            SyncClientMsg::Sound { client_id: "b".into() },
            None,
            t + 100,
        );
        while rx_a.try_recv().is_ok() {}
        let _ = conn_b2;
        t += HEARTBEAT_TIMEOUT_MS + 1_000;
        hub.handle_ws_msg(key, "a", conn_a, SyncClientMsg::Ping { client_ms: t }, None, t);
        let _ = rx_a.try_recv(); // Pong
        hub.sweep(t);
        match rx_a.try_recv().unwrap() {
            SyncServerMsg::State(st) => {
                assert_eq!(st.members.len(), 1);
                assert_eq!(st.sound_client_id.as_deref(), Some("a"));
            }
            _ => panic!("expected sweep handoff"),
        }
    }

    #[test]
    fn notifies_matching_players_when_preload_finishes() {
        let hub = SyncHub::new();
        let t = 5_000_000_i64;
        let hash_a = "a".repeat(40);
        hub.join_in_memory(1, &join_req("a", "ПК"), &[], None, t);
        let (tx_a, mut rx_a) = mpsc::channel(WS_OUTBOUND_CAPACITY);
        let claims_a = SyncTicketClaims {
            uid: 1,
            kind: MediaKind::Movie,
            tmdb_id: 603,
            cid: "a".into(),
            exp: 9_999_999_999,
        };
        let (key, conn_a, _) = hub.attach_ws(&claims_a, tx_a, t).unwrap();
        assert!(!hub.has_playing_hash_at(&hash_a, t));
        hub.handle_ws_msg(
            key,
            "a",
            conn_a,
            SyncClientMsg::Ready {
                position_sec: 10.0,
                duration_sec: Some(5000.0),
                file_index: Some(1),
            },
            None,
            t + 100,
        );
        assert!(hub.has_playing_hash_at(&hash_a, t + 100));
        while rx_a.try_recv().is_ok() {}

        hub.notify_preload_ready_at(&[1], MediaKind::Movie, 603, 0, &hash_a, t + 100);
        match rx_a.try_recv().unwrap() {
            SyncServerMsg::PreloadReady {
                kind,
                tmdb_id,
                season,
                infohash,
            } => {
                assert_eq!(kind, MediaKind::Movie);
                assert_eq!(tmdb_id, 603);
                assert_eq!(season, 0);
                assert_eq!(infohash, hash_a);
            }
            other => panic!("expected PreloadReady, got {other:?}"),
        }
    }

    #[test]
    fn saves_position_without_duration() {
        let hub = SyncHub::new();
        let t = 5_000_000_i64;
        let mut req = join_req("a", "ПК");
        req.kind = MediaKind::Tv;
        req.tmdb_id = 1622;
        req.season = 1;
        req.episode = 1;
        hub.join_in_memory(1, &req, &[], None, t);
        let (tx_a, _rx_a) = mpsc::channel(WS_OUTBOUND_CAPACITY);
        let claims_a = SyncTicketClaims {
            uid: 1,
            kind: MediaKind::Tv,
            tmdb_id: 1622,
            cid: "a".into(),
            exp: 9_999_999_999,
        };
        let (key, conn_a, _) = hub.attach_ws(&claims_a, tx_a, t).unwrap();
        hub.handle_ws_msg(
            key,
            "a",
            conn_a,
            SyncClientMsg::Ready {
                position_sec: 0.0,
                duration_sec: None,
                file_index: Some(1),
            },
            None,
            t,
        );
        // At 0s with unknown duration, nothing meaningful to persist yet.
        assert!(hub
            .leave_http(
                1,
                &SyncLeaveRequest {
                    client_id: "a".into(),
                    kind: MediaKind::Tv,
                    tmdb_id: 1622,
                    position_sec: 0.2,
                    duration_sec: None,
                    finished: false,
                },
                t,
            )
            .is_none());

        // Rejoin and watch past 1s with unknown duration -> timecode must be saved.
        hub.join_in_memory(1, &req, &[], None, t);
        let (tx_a2, _rx_a2) = mpsc::channel(WS_OUTBOUND_CAPACITY);
        let (_, conn_a2, _) = hub.attach_ws(&claims_a, tx_a2, t).unwrap();
        hub.handle_ws_msg(
            key,
            "a",
            conn_a2,
            SyncClientMsg::Ready {
                position_sec: 125.0,
                duration_sec: None,
                file_index: Some(1),
            },
            None,
            t,
        );
        let job = hub
            .leave_http(
                1,
                &SyncLeaveRequest {
                    client_id: "a".into(),
                    kind: MediaKind::Tv,
                    tmdb_id: 1622,
                    position_sec: 125.0,
                    duration_sec: None,
                    finished: false,
                },
                t + 5_000,
            )
            .expect("expected save job when position >= 1s even without duration");
        assert!((job.update.position_sec - 125.0).abs() < 0.01);
        assert_eq!(job.update.duration_sec, 0.0);
        assert_eq!(job.update.season, 1);
        assert_eq!(job.update.episode, 1);
    }
}
