//! SSD pause-resume cache for torrent streams (both Direct and transcoded).
//!
//! During active playback nothing is written to disk. Once a user pauses or
//! closes the player (and stays idle for a short grace period), the server grabs
//! the file's header/tail metadata and a ~40s ahead window around the pause
//! position while TorrServer still holds the torrent in RAM, and saves them to
//! SSD under a strict size budget (default 5 GiB) and TTL (default 72 hours).
//!
//! When the user returns later, `POST /api/play` responds immediately from the
//! cached metadata while waking TorrServer in the background, and both Direct
//! clients and `ffmpeg` receive cached bytes at SSD speed before seamlessly
//! continuing from TorrServer.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    extract::{Path as AxumPath, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use chrono::Utc;
use futures_util::StreamExt;
use kino_core::{MediaKind, FINISHED_RATIO};
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    process::Command,
    sync::{mpsc, Notify, OnceCell},
};

use super::{ffmpeg::Probe, files::FileEntry, torrserver::TorrServer, Source};

const HEAD_BYTES: u64 = 4 * 1024 * 1024;
const TAIL_BYTES: u64 = 2 * 1024 * 1024;
const BACK_MARGIN_BYTES: u64 = 3 * 1024 * 1024;
const MIN_WINDOW_BYTES: u64 = 32 * 1024 * 1024;
const MAX_WINDOW_BYTES: u64 = 128 * 1024 * 1024;
const DEFAULT_WINDOW_BYTES: u64 = 40 * 1024 * 1024;
const TARGET_WINDOW_SECS: f64 = 40.0;
const WARMUP_AHEAD_BYTES: u64 = 32 * 1024 * 1024;
const MIN_PAUSE_POSITION_SEC: f64 = 15.0;
const RESUME_MATCH_TOLERANCE_SEC: f64 = 120.0;
const CAPTURE_DEBOUNCE: Duration = Duration::from_secs(25);
const PROGRESS_ACTIVE_WINDOW: Duration = Duration::from_secs(15);
const PROBE_CACHE_TTL: Duration = Duration::from_secs(12 * 3600);
const PROBE_CACHE_MAX: usize = 128;
const READ_CHUNK_SIZE: usize = 64 * 1024;
const STREAM_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub user_id: i64,
    pub infohash: String,
    pub file_index: i32,
}

impl CacheKey {
    pub fn new(user_id: i64, infohash: &str, file_index: i32) -> Self {
        Self {
            user_id,
            infohash: infohash.to_ascii_lowercase(),
            file_index,
        }
    }

    fn dir_name(&self) -> Option<String> {
        if self.infohash.is_empty() || !self.infohash.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        Some(format!("{}_{}_{}", self.user_id, self.infohash, self.file_index))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSlice {
    offset: u64,
    len: u64,
    file_name: String,
}

impl CachedSlice {
    fn end(&self) -> u64 {
        self.offset.saturating_add(self.len)
    }

    fn contains(&self, pos: u64) -> bool {
        pos >= self.offset && pos < self.end()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedEntryMeta {
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    infohash: String,
    file_index: i32,
    file: FileEntry,
    probe: Probe,
    content_type: String,
    position_sec: f64,
    created_at_unix: i64,
    last_accessed_unix: i64,
    slices: Vec<CachedSlice>,
}

impl CachedEntryMeta {
    fn key(&self) -> CacheKey {
        CacheKey::new(self.user_id, &self.infohash, self.file_index)
    }

    fn total_bytes(&self) -> u64 {
        self.slices.iter().map(|s| s.len).sum()
    }

    fn resume_window_end(&self) -> u64 {
        self.slices
            .iter()
            .filter(|s| s.file_name == "window.bin")
            .map(CachedSlice::end)
            .max()
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone)]
pub struct PauseCacheHit {
    pub file: FileEntry,
    pub probe: Probe,
    pub resume_window_end: u64,
}

#[derive(Clone)]
struct PlayContext {
    key: CacheKey,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    source: Source,
    file: FileEntry,
    probe: Probe,
    content_type: String,
    open_streams: usize,
    last_byte_offset: u64,
    last_position_sec: f64,
    last_duration_sec: f64,
    last_progress_moved_at: Option<Instant>,
    transcode_stalled: bool,
    capture_gen: u64,
    last_captured_pos: Option<f64>,
    torrserver: TorrServer,
    ffmpeg: String,
}

struct EnsureState {
    done: AtomicBool,
    ok: AtomicBool,
    started_at: Instant,
    notify: Notify,
}

impl EnsureState {
    fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            ok: AtomicBool::new(false),
            started_at: Instant::now(),
            notify: Notify::new(),
        }
    }

    fn finish(&self, ok: bool) {
        self.ok.store(ok, Ordering::Release);
        self.done.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self, timeout: Duration) -> bool {
        if self.done.load(Ordering::Acquire) {
            return self.ok.load(Ordering::Acquire);
        }
        let _ = tokio::time::timeout(timeout, async {
            loop {
                let notified = self.notify.notified();
                if self.done.load(Ordering::Acquire) {
                    break;
                }
                notified.await;
            }
        })
        .await;
        self.done.load(Ordering::Acquire) && self.ok.load(Ordering::Acquire)
    }
}

struct ProbeSession {
    ts: TorrServer,
    stream_url: String,
    file_len: u64,
    head: Vec<u8>,
    tail_offset: u64,
    tail: Vec<u8>,
    recorded_seek: Mutex<Option<u64>>,
    extra_tail: Mutex<Option<(u64, Vec<u8>)>>,
}

struct RecentProbe {
    probe: Probe,
    updated_at: Instant,
}

struct Inner {
    root: Option<PathBuf>,
    max_bytes: u64,
    ttl_secs: i64,
    secret: String,
    entries: Mutex<HashMap<CacheKey, CachedEntryMeta>>,
    contexts: Mutex<HashMap<CacheKey, PlayContext>>,
    recent_probes: Mutex<HashMap<(String, i32), RecentProbe>>,
    capturing: Mutex<HashSet<CacheKey>>,
    bg_ensures: Mutex<HashMap<String, Arc<EnsureState>>>,
    probes: Mutex<HashMap<String, Arc<ProbeSession>>>,
    loopback_port: OnceCell<u16>,
    probe_seq: AtomicU64,
}

#[derive(Clone)]
pub struct PauseCache {
    inner: Arc<Inner>,
}

impl PauseCache {
    pub fn new(root: Option<PathBuf>, max_mb: u64, ttl_hours: u64) -> Self {
        let secret: String = rand::rng()
            .sample_iter(&Alphanumeric)
            .take(32)
            .map(char::from)
            .collect();
        let max_bytes = max_mb.saturating_mul(1024 * 1024);
        let ttl_secs = (ttl_hours.saturating_mul(3600)) as i64;
        let mut entries = HashMap::new();
        if let Some(dir) = &root {
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::warn!(error = %e, path = %dir.display(), "failed to create pause cache dir");
            } else {
                entries = load_existing_entries(dir);
            }
        }
        let cache = Self {
            inner: Arc::new(Inner {
                root,
                max_bytes,
                ttl_secs,
                secret,
                entries: Mutex::new(entries),
                contexts: Mutex::new(HashMap::new()),
                recent_probes: Mutex::new(HashMap::new()),
                capturing: Mutex::new(HashSet::new()),
                bg_ensures: Mutex::new(HashMap::new()),
                probes: Mutex::new(HashMap::new()),
                loopback_port: OnceCell::new(),
                probe_seq: AtomicU64::new(1),
            }),
        };
        if cache.is_enabled() && tokio::runtime::Handle::try_current().is_ok() {
            let c = cache.clone();
            tokio::spawn(async move {
                c.prune().await;
            });
        }
        cache
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.root.is_some()
    }

    /// Ensures the internal loopback HTTP server for `ffmpeg` and seek-probing is running.
    pub async fn loopback_port(&self, ts: TorrServer) -> anyhow::Result<u16> {
        let cache = self.clone();
        self.inner
            .loopback_port
            .get_or_try_init(|| async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
                let port = listener.local_addr()?.port();
                let app = Router::new()
                    .route("/internal/{secret}/{uid}/{hash}/{index}", get(internal_stream))
                    .route("/probe/{secret}/{probe_id}", get(probe_stream))
                    .with_state(LoopbackState { cache, ts });
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener, app).await {
                        tracing::warn!(error = %e, "pause cache loopback server stopped");
                    }
                });
                Ok(port)
            })
            .await
            .copied()
    }

    /// Builds an internal localhost URL that `ffmpeg` reads through our cache + proxy.
    pub async fn internal_stream_url(
        &self,
        ts: &TorrServer,
        user_id: i64,
        hash: &str,
        file_index: i32,
    ) -> Option<String> {
        let port = self.loopback_port(ts.clone()).await.ok()?;
        Some(format!(
            "http://127.0.0.1:{port}/internal/{}/{user_id}/{}/{file_index}",
            self.inner.secret,
            hash.to_ascii_lowercase()
        ))
    }

    /// Looks up a valid, non-expired pause cache matching this playback start.
    pub fn lookup_for_play(
        &self,
        user_id: i64,
        hash: &str,
        season: i32,
        episode: i32,
        start_at_sec: i32,
    ) -> Option<PauseCacheHit> {
        if !self.is_enabled() {
            return None;
        }
        let hash = hash.to_ascii_lowercase();
        let now = Utc::now().timestamp();
        let mut guard = self.inner.entries.lock().unwrap_or_else(|e| e.into_inner());
        let matched_key = guard
            .iter()
            .find(|(k, m)| {
                k.user_id == user_id
                    && k.infohash == hash
                    && m.season == season
                    && m.episode == episode
                    && now - m.last_accessed_unix <= self.inner.ttl_secs
                    && (f64::from(start_at_sec) - m.position_sec).abs() <= RESUME_MATCH_TOLERANCE_SEC
                    && !m.slices.is_empty()
            })
            .map(|(k, _)| k.clone())?;
        let entry = guard.get_mut(&matched_key)?;
        entry.last_accessed_unix = now;
        let hit = PauseCacheHit {
            file: entry.file.clone(),
            probe: entry.probe.clone(),
            resume_window_end: entry.resume_window_end(),
        };
        let meta_clone = entry.clone();
        drop(guard);
        if let Some(dir) = self.entry_dir(&matched_key) {
            tokio::spawn(async move {
                let _ = write_meta(&dir, &meta_clone).await;
            });
        }
        Some(hit)
    }

    /// Remembers a successful `ffprobe` result in RAM so switching to another
    /// device skips re-probing immediately (even before a 25s pause cache exists).
    pub fn remember_probe(&self, hash: &str, file_index: i32, probe: &Probe) {
        if probe.container.is_empty() && probe.duration.is_none() && probe.audio.is_empty() {
            return;
        }
        let hash_lc = hash.to_ascii_lowercase();
        {
            let mut guard = self.inner.recent_probes.lock().unwrap_or_else(|e| e.into_inner());
            if guard.len() >= PROBE_CACHE_MAX && !guard.contains_key(&(hash_lc.clone(), file_index)) {
                if let Some(oldest) = guard.iter().min_by_key(|(_, v)| v.updated_at).map(|(k, _)| k.clone()) {
                    guard.remove(&oldest);
                }
            }
            guard.insert(
                (hash_lc.clone(), file_index),
                RecentProbe {
                    probe: probe.clone(),
                    updated_at: Instant::now(),
                },
            );
        }
        let mut ctxs = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
        for (k, ctx) in ctxs.iter_mut() {
            if k.infohash == hash_lc && k.file_index == file_index {
                ctx.probe = probe.clone();
                if let Some(d) = probe.duration.filter(|d| *d > 0.0) {
                    ctx.last_duration_sec = d;
                }
            }
        }
    }

    /// Returns a recently probed file's metadata from RAM (`< 12h`), avoiding
    /// a redundant `ffprobe` pass when another device opens the same release.
    pub fn lookup_probe(&self, hash: &str, file_index: i32) -> Option<Probe> {
        let key = (hash.to_ascii_lowercase(), file_index);
        let mut guard = self.inner.recent_probes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = guard.get(&key) {
            if entry.updated_at.elapsed() <= PROBE_CACHE_TTL {
                return Some(entry.probe.clone());
            }
            guard.remove(&key);
        }
        None
    }

    /// Spawns background torrent connection + range preload right after `resume_window_end`.
    pub fn trigger_background_warmup(
        &self,
        ts: TorrServer,
        src: Source,
        file_index: i32,
        resume_window_end: u64,
        file_len: u64,
    ) {
        let Some(hash) = src.infohash.clone().filter(|h| !h.is_empty()) else {
            return;
        };
        let hash_lc = hash.to_ascii_lowercase();
        let ensure = {
            let mut map = self.inner.bg_ensures.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = map.get(&hash_lc) {
                if existing.started_at.elapsed() < Duration::from_secs(45) {
                    return;
                }
            }
            let st = Arc::new(EnsureState::new());
            map.insert(hash_lc.clone(), Arc::clone(&st));
            st
        };
        tokio::spawn(async move {
            let ok = ts.add(&src).await.map(|l| !l.is_empty()).unwrap_or(false);
            ensure.finish(ok);
            if ok && resume_window_end < file_len {
                let len = WARMUP_AHEAD_BYTES.min(file_len - resume_window_end);
                ts.warm_up_range(&hash_lc, file_index, resume_window_end, len).await;
            }
        });
    }

    async fn await_torrserver_ready(&self, ts: &TorrServer, key: &CacheKey) {
        let maybe_ensure = {
            let map = self.inner.bg_ensures.lock().unwrap_or_else(|e| e.into_inner());
            map.get(&key.infohash).cloned()
        };
        if let Some(st) = maybe_ensure {
            if st.wait(Duration::from_secs(35)).await {
                return;
            }
        }
        let maybe_src = {
            let map = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
            map.get(key).map(|c| c.source.clone())
        };
        if let Some(src) = maybe_src {
            let _ = ts.add(&src).await;
        }
    }

    /// Registers an active playback session so pause/idle capture knows its metadata and source.
    #[allow(clippy::too_many_arguments)]
    pub fn register_play(
        &self,
        user_id: i64,
        kind: MediaKind,
        tmdb_id: i64,
        season: i32,
        episode: i32,
        source: Source,
        file: FileEntry,
        probe: Probe,
        start_at_sec: f64,
        ts: TorrServer,
        ffmpeg: String,
    ) {
        if !self.is_enabled() {
            return;
        }
        let Some(hash) = source.infohash.as_deref() else {
            return;
        };
        let key = CacheKey::new(user_id, hash, file.index);
        let content_type = guess_content_type(&file.path).to_owned();
        {
            let mut guard = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
            let prev = guard.get(&key);
            let open_streams = prev.map_or(0, |p| p.open_streams);
            let last_byte_offset = prev.map_or(0, |p| p.last_byte_offset);
            let capture_gen = prev.map_or(0, |p| p.capture_gen.wrapping_add(1));
            let last_captured_pos = prev.and_then(|p| p.last_captured_pos);
            guard.insert(
                key.clone(),
                PlayContext {
                    key: key.clone(),
                    kind,
                    tmdb_id,
                    season,
                    episode,
                    source,
                    file,
                    probe: probe.clone(),
                    content_type,
                    open_streams,
                    last_byte_offset,
                    last_position_sec: start_at_sec,
                    last_duration_sec: probe.duration.unwrap_or(0.0),
                    last_progress_moved_at: None,
                    transcode_stalled: false,
                    capture_gen,
                    last_captured_pos,
                    torrserver: ts,
                    ffmpeg,
                },
            );
        }

        // Evict any earlier episode of the same TV series for this user so watched episodes don't linger.
        if kind == MediaKind::Tv && (season > 0 || episode > 0) {
            let cache = self.clone();
            tokio::spawn(async move {
                cache
                    .evict_where(|m| {
                        m.user_id == user_id
                            && m.kind == MediaKind::Tv
                            && m.tmdb_id == tmdb_id
                            && (m.season, m.episode) < (season, episode)
                    })
                    .await;
            });
        }
    }

    /// Updates playback position from `PUT /api/progress` and filters out finished (>= 90%) titles.
    #[allow(clippy::too_many_arguments)]
    pub fn on_progress(
        &self,
        user_id: i64,
        kind: MediaKind,
        tmdb_id: i64,
        season: i32,
        episode: i32,
        infohash: Option<&str>,
        position_sec: f64,
        duration_sec: f64,
        finished: bool,
    ) {
        if !self.is_enabled() {
            return;
        }
        let is_done = finished || (duration_sec > 0.0 && position_sec >= duration_sec * FINISHED_RATIO);
        if is_done {
            {
                let mut guard = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
                for ctx in guard.values_mut() {
                    if ctx.key.user_id == user_id
                        && ctx.kind == kind
                        && ctx.tmdb_id == tmdb_id
                        && ctx.season == season
                        && ctx.episode == episode
                    {
                        ctx.capture_gen = ctx.capture_gen.wrapping_add(1);
                        ctx.last_position_sec = position_sec;
                        if duration_sec > 0.0 {
                            ctx.last_duration_sec = duration_sec;
                        }
                    }
                }
            }
            let cache = self.clone();
            tokio::spawn(async move {
                cache
                    .evict_where(|m| {
                        m.user_id == user_id
                            && m.kind == kind
                            && m.tmdb_id == tmdb_id
                            && m.season == season
                            && m.episode == episode
                    })
                    .await;
            });
            return;
        }

        let mut guard = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        for ctx in guard.values_mut() {
            let hash_matches = infohash.is_none_or(|h| ctx.key.infohash.eq_ignore_ascii_case(h));
            if ctx.key.user_id == user_id
                && ctx.kind == kind
                && ctx.tmdb_id == tmdb_id
                && ctx.season == season
                && ctx.episode == episode
                && hash_matches
            {
                if (position_sec - ctx.last_position_sec).abs() > 1.5 {
                    ctx.last_progress_moved_at = Some(now);
                }
                ctx.last_position_sec = position_sec;
                if duration_sec > 0.0 {
                    ctx.last_duration_sec = duration_sec;
                }
            }
        }
    }

    /// Evicts all pause-cache entries for a user's title (e.g. when marked Watched or Dropped).
    pub async fn evict_title(&self, user_id: i64, kind: MediaKind, tmdb_id: i64) {
        self.evict_where(|m| m.user_id == user_id && m.kind == kind && m.tmdb_id == tmdb_id)
            .await;
    }

    /// Evicts pause-cache entries for a specific episode (e.g. marked watched).
    pub async fn evict_episode(&self, user_id: i64, kind: MediaKind, tmdb_id: i64, season: i32, episode: i32) {
        self.evict_where(|m| {
            m.user_id == user_id && m.kind == kind && m.tmdb_id == tmdb_id && m.season == season && m.episode == episode
        })
        .await;
    }

    async fn evict_where<F>(&self, pred: F)
    where
        F: Fn(&CachedEntryMeta) -> bool,
    {
        if !self.is_enabled() {
            return;
        }
        let to_remove: Vec<CacheKey> = {
            let mut guard = self.inner.entries.lock().unwrap_or_else(|e| e.into_inner());
            let keys: Vec<CacheKey> = guard.iter().filter(|(_, m)| pred(m)).map(|(k, _)| k.clone()).collect();
            for k in &keys {
                guard.remove(k);
            }
            keys
        };
        for k in to_remove {
            if let Some(dir) = self.entry_dir(&k) {
                let _ = fs::remove_dir_all(dir).await;
            }
        }
    }

    /// Called when an `ffmpeg` transcoded stream stalls on client read (user paused in browser).
    pub fn on_transcode_stalled(&self, key: &CacheKey) {
        if !self.is_enabled() {
            return;
        }
        let gen = {
            let mut guard = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
            let Some(ctx) = guard.get_mut(key) else {
                return;
            };
            ctx.transcode_stalled = true;
            ctx.capture_gen = ctx.capture_gen.wrapping_add(1);
            ctx.capture_gen
        };
        let cache = self.clone();
        let key = key.clone();
        tokio::spawn(async move {
            cache.capture_if_eligible(key, gen).await;
        });
    }

    fn on_stream_open(&self, key: &CacheKey) {
        let mut guard = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ctx) = guard.get_mut(key) {
            ctx.open_streams += 1;
            ctx.transcode_stalled = false;
            ctx.capture_gen = ctx.capture_gen.wrapping_add(1);
        }
    }

    fn on_stream_bytes(&self, key: &CacheKey, offset_end: u64) {
        let mut guard = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ctx) = guard.get_mut(key) {
            if offset_end > ctx.last_byte_offset {
                ctx.last_byte_offset = offset_end;
            }
        }
    }

    fn on_stream_close(&self, key: &CacheKey) {
        let gen = {
            let mut guard = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
            let Some(ctx) = guard.get_mut(key) else {
                return;
            };
            ctx.open_streams = ctx.open_streams.saturating_sub(1);
            if ctx.open_streams > 0 {
                return;
            }
            ctx.capture_gen = ctx.capture_gen.wrapping_add(1);
            ctx.capture_gen
        };
        let cache = self.clone();
        let key = key.clone();
        tokio::spawn(async move {
            cache.debounced_capture(key, gen).await;
        });
    }

    async fn debounced_capture(&self, key: CacheKey, gen: u64) {
        tokio::time::sleep(CAPTURE_DEBOUNCE).await;
        // Wait while the client is still advancing through its own RAM buffer without HTTP reads.
        for _ in 0..3 {
            let should_wait = {
                let guard = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
                let Some(ctx) = guard.get(&key) else {
                    return;
                };
                if ctx.capture_gen != gen || ctx.open_streams > 0 {
                    return;
                }
                ctx.last_progress_moved_at
                    .is_some_and(|t| t.elapsed() < PROGRESS_ACTIVE_WINDOW)
            };
            if !should_wait {
                break;
            }
            tokio::time::sleep(Duration::from_secs(20)).await;
        }
        self.capture_if_eligible(key, gen).await;
    }

    async fn capture_if_eligible(&self, key: CacheKey, gen: u64) {
        let ctx = {
            let guard = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
            let Some(ctx) = guard.get(&key) else {
                return;
            };
            if ctx.capture_gen != gen {
                return;
            }
            if ctx.open_streams > 0 && !ctx.transcode_stalled {
                return;
            }
            if ctx.last_position_sec < MIN_PAUSE_POSITION_SEC {
                return;
            }
            if ctx.last_duration_sec > 0.0 && ctx.last_position_sec >= ctx.last_duration_sec * FINISHED_RATIO {
                return;
            }
            if ctx
                .last_captured_pos
                .is_some_and(|p| (p - ctx.last_position_sec).abs() < 5.0)
            {
                return;
            }
            ctx.clone()
        };

        {
            let mut cap = self.inner.capturing.lock().unwrap_or_else(|e| e.into_inner());
            if !cap.insert(key.clone()) {
                return;
            }
        }
        let res = self.perform_capture(&ctx).await;
        {
            let mut cap = self.inner.capturing.lock().unwrap_or_else(|e| e.into_inner());
            cap.remove(&key);
        }
        if let Err(e) = res {
            tracing::debug!(error = %e, hash = %key.infohash, "pause cache capture skipped");
        } else {
            let mut guard = self.inner.contexts.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = guard.get_mut(&key) {
                c.last_captured_pos = Some(ctx.last_position_sec);
            }
        }
    }

    async fn perform_capture(&self, ctx: &PlayContext) -> anyhow::Result<()> {
        let Some(root) = &self.inner.root else {
            return Ok(());
        };
        let Some(dir_name) = ctx.key.dir_name() else {
            return Ok(());
        };
        let final_dir = root.join(&dir_name);
        let staging_dir = root.join(format!(
            ".staging_{dir_name}_{}",
            self.inner.probe_seq.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&staging_dir).await?;
        let res = self.write_staging_entry(ctx, &staging_dir).await;
        match res {
            Ok(meta) => {
                if fs::try_exists(&final_dir).await.unwrap_or(false) {
                    let _ = fs::remove_dir_all(&final_dir).await;
                }
                fs::rename(&staging_dir, &final_dir).await?;
                {
                    let mut guard = self.inner.entries.lock().unwrap_or_else(|e| e.into_inner());
                    guard.insert(ctx.key.clone(), meta.clone());
                }
                tracing::info!(
                    hash = %ctx.key.infohash,
                    pos_sec = ctx.last_position_sec as u64,
                    bytes = meta.total_bytes(),
                    "saved pause cache entry"
                );
                self.prune().await;
                Ok(())
            }
            Err(e) => {
                let _ = fs::remove_dir_all(&staging_dir).await;
                Err(e)
            }
        }
    }

    async fn write_staging_entry(&self, ctx: &PlayContext, staging_dir: &Path) -> anyhow::Result<CachedEntryMeta> {
        let ts = &ctx.torrserver;
        let stream_url = ts.stream_url(&ctx.key.infohash, ctx.key.file_index);

        // Fetch file header (first 4 MiB); also learns total length if FileEntry.length was 0.
        let head_req_end = if ctx.file.length > 0 {
            HEAD_BYTES.min(ctx.file.length) - 1
        } else {
            HEAD_BYTES - 1
        };
        let (head, total_len, detected_ct) = fetch_bytes_range(ts, &stream_url, 0, head_req_end).await?;
        let file_len = if total_len > 0 { total_len } else { ctx.file.length };
        anyhow::ensure!(file_len > 0 && !head.is_empty(), "empty torrent file");

        let mut slices = Vec::new();
        fs::write(staging_dir.join("head.bin"), &head).await?;
        slices.push(CachedSlice {
            offset: 0,
            len: head.len() as u64,
            file_name: "head.bin".into(),
        });

        let mut tail = Vec::new();
        let mut tail_offset = file_len;
        if file_len > head.len() as u64 {
            tail_offset = file_len.saturating_sub(TAIL_BYTES).max(head.len() as u64);
            if tail_offset < file_len {
                if let Ok((t, _, _)) = fetch_bytes_range(ts, &stream_url, tail_offset, file_len - 1).await {
                    tail = t;
                }
            }
        }

        let (anchor_offset, extra_tail) = self
            .resolve_seek_anchor(ctx, &stream_url, file_len, head.clone(), tail_offset, tail.clone())
            .await;

        if let Some((ex_off, ex_bytes)) = extra_tail {
            if !ex_bytes.is_empty() {
                fs::write(staging_dir.join("tail_meta.bin"), &ex_bytes).await?;
                slices.push(CachedSlice {
                    offset: ex_off,
                    len: ex_bytes.len() as u64,
                    file_name: "tail_meta.bin".into(),
                });
            }
        }
        if !tail.is_empty() {
            fs::write(staging_dir.join("tail.bin"), &tail).await?;
            slices.push(CachedSlice {
                offset: tail_offset,
                len: tail.len() as u64,
                file_name: "tail.bin".into(),
            });
        }

        let bps = if ctx.last_duration_sec > 0.0 {
            file_len as f64 / ctx.last_duration_sec
        } else {
            DEFAULT_WINDOW_BYTES as f64 / TARGET_WINDOW_SECS
        };
        let forward_bytes = (bps * TARGET_WINDOW_SECS).clamp(MIN_WINDOW_BYTES as f64, MAX_WINDOW_BYTES as f64) as u64;
        let win_start = anchor_offset
            .saturating_sub(BACK_MARGIN_BYTES)
            .min(file_len.saturating_sub(1));
        let win_end = anchor_offset.saturating_add(forward_bytes).min(file_len);
        if win_end > win_start && win_end > head.len() as u64 {
            let actual_start = win_start.max(head.len() as u64);
            if win_end > actual_start {
                let written = stream_range_to_file(
                    ts,
                    &stream_url,
                    actual_start,
                    win_end - 1,
                    &staging_dir.join("window.bin"),
                )
                .await?;
                if written > 0 {
                    slices.push(CachedSlice {
                        offset: actual_start,
                        len: written,
                        file_name: "window.bin".into(),
                    });
                }
            }
        }

        let mut file = ctx.file.clone();
        file.length = file_len;
        let now = Utc::now().timestamp();
        let meta = CachedEntryMeta {
            user_id: ctx.key.user_id,
            kind: ctx.kind,
            tmdb_id: ctx.tmdb_id,
            season: ctx.season,
            episode: ctx.episode,
            infohash: ctx.key.infohash.clone(),
            file_index: ctx.key.file_index,
            file,
            probe: ctx.probe.clone(),
            content_type: detected_ct.unwrap_or_else(|| ctx.content_type.clone()),
            position_sec: ctx.last_position_sec,
            created_at_unix: now,
            last_accessed_unix: now,
            slices,
        };
        write_meta(staging_dir, &meta).await?;
        Ok(meta)
    }

    async fn resolve_seek_anchor(
        &self,
        ctx: &PlayContext,
        stream_url: &str,
        file_len: u64,
        head: Vec<u8>,
        tail_offset: u64,
        tail: Vec<u8>,
    ) -> (u64, Option<(u64, Vec<u8>)>) {
        let est_offset = if ctx.last_duration_sec > 0.0 {
            ((ctx.last_position_sec / ctx.last_duration_sec).clamp(0.0, 0.99) * file_len as f64) as u64
        } else {
            0
        };
        let fallback = if ctx.last_byte_offset > 0
            && (est_offset == 0 || ctx.last_byte_offset.abs_diff(est_offset) <= 96 * 1024 * 1024)
        {
            ctx.last_byte_offset.saturating_sub(8 * 1024 * 1024)
        } else {
            est_offset
        };

        let Ok(port) = self.loopback_port(ctx.torrserver.clone()).await else {
            return (fallback, None);
        };
        let probe_id = self.inner.probe_seq.fetch_add(1, Ordering::Relaxed).to_string();
        let session = Arc::new(ProbeSession {
            ts: ctx.torrserver.clone(),
            stream_url: stream_url.to_owned(),
            file_len,
            head,
            tail_offset,
            tail,
            recorded_seek: Mutex::new(None),
            extra_tail: Mutex::new(None),
        });
        self.inner
            .probes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(probe_id.clone(), Arc::clone(&session));

        let url = format!("http://127.0.0.1:{port}/probe/{}/{probe_id}", self.inner.secret);
        let seek_sec = (ctx.last_position_sec.floor() as u64).max(1);
        let args = [
            "-hide_banner".to_owned(),
            "-loglevel".into(),
            "error".into(),
            "-rw_timeout".into(),
            "8000000".into(),
            "-noaccurate_seek".into(),
            "-ss".into(),
            seek_sec.to_string(),
            "-i".into(),
            url,
            "-map".into(),
            "0:v:0".into(),
            "-c".into(),
            "copy".into(),
            "-frames:v".into(),
            "1".into(),
            "-f".into(),
            "null".into(),
            "-".into(),
        ];
        let _ = tokio::time::timeout(
            Duration::from_secs(4),
            Command::new(&ctx.ffmpeg).args(&args).kill_on_drop(true).output(),
        )
        .await;

        self.inner
            .probes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&probe_id);

        let recorded = *session.recorded_seek.lock().unwrap_or_else(|e| e.into_inner());
        let extra = session.extra_tail.lock().unwrap_or_else(|e| e.into_inner()).take();
        (recorded.unwrap_or(fallback), extra)
    }

    /// Removes expired (> TTL) entries and trims oldest entries until total size <= `max_bytes`.
    pub async fn prune(&self) {
        let Some(root) = &self.inner.root else {
            return;
        };
        let now = Utc::now().timestamp();
        let mut removed_keys = Vec::new();
        {
            let mut guard = self.inner.entries.lock().unwrap_or_else(|e| e.into_inner());
            let expired: Vec<CacheKey> = guard
                .iter()
                .filter(|(_, m)| now - m.last_accessed_unix > self.inner.ttl_secs)
                .map(|(k, _)| k.clone())
                .collect();
            for k in expired {
                guard.remove(&k);
                removed_keys.push(k);
            }
            let mut total: u64 = guard.values().map(CachedEntryMeta::total_bytes).sum();
            if total > self.inner.max_bytes {
                let mut by_age: Vec<(CacheKey, i64, u64)> = guard
                    .iter()
                    .map(|(k, m)| (k.clone(), m.last_accessed_unix, m.total_bytes()))
                    .collect();
                by_age.sort_by_key(|(_, ts, _)| *ts);
                for (k, _, sz) in by_age {
                    if total <= self.inner.max_bytes {
                        break;
                    }
                    guard.remove(&k);
                    removed_keys.push(k);
                    total = total.saturating_sub(sz);
                }
            }
        }
        for k in removed_keys {
            if let Some(dir) = self.entry_dir(&k) {
                let _ = fs::remove_dir_all(dir).await;
            }
        }
        if let Ok(mut rd) = fs::read_dir(root).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(".staging_") {
                    let is_stale = entry
                        .metadata()
                        .await
                        .and_then(|m| m.modified())
                        .is_ok_and(|t| t.elapsed().unwrap_or_default() > Duration::from_secs(600));
                    if is_stale {
                        let _ = fs::remove_dir_all(entry.path()).await;
                    }
                }
            }
        }
    }

    fn entry_dir(&self, key: &CacheKey) -> Option<PathBuf> {
        let root = self.inner.root.as_ref()?;
        let name = key.dir_name()?;
        Some(root.join(name))
    }

    /// Serves a torrent stream request (`GET` or `HEAD`) from the pause cache when covered,
    /// seamlessly splicing into TorrServer at slice boundaries and proxying cache misses.
    pub async fn serve_stream(
        &self,
        ts: &TorrServer,
        user_id: i64,
        hash: &str,
        file_index: i32,
        method: &Method,
        headers: &HeaderMap,
    ) -> Response {
        let key = CacheKey::new(user_id, hash, file_index);
        let upstream_url = ts.stream_url(&key.infohash, file_index);

        let entry = if self.is_enabled() {
            self.inner
                .entries
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .cloned()
        } else {
            None
        };

        if *method == Method::HEAD {
            if let Some(meta) = entry.as_ref().filter(|m| m.file.length > 0) {
                let mut resp = Response::builder().status(StatusCode::OK);
                if let Ok(ct) = HeaderValue::from_str(&meta.content_type) {
                    resp = resp.header(header::CONTENT_TYPE, ct);
                }
                resp = resp
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_LENGTH, meta.file.length.to_string());
                return resp.body(Body::empty()).unwrap_or_else(|_| bad_gateway());
            }
        }

        let is_get = *method == Method::GET;
        let guard = is_get.then(|| ActiveStreamGuard::new(self.clone(), key.clone()));

        if let (true, Some(meta)) = (is_get, entry) {
            if meta.file.length > 0 {
                let range_hdr = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
                let (has_range, start, end) = match parse_range(range_hdr, meta.file.length) {
                    Ok(v) => v,
                    Err(()) => {
                        return (
                            StatusCode::RANGE_NOT_SATISFIABLE,
                            [(header::CONTENT_RANGE, format!("bytes */{}", meta.file.length))],
                        )
                            .into_response();
                    }
                };
                if let Some(slice) = meta
                    .slices
                    .iter()
                    .filter(|s| s.contains(start))
                    .max_by_key(|s| s.end())
                    .cloned()
                {
                    if let Some(dir) = self.entry_dir(&key) {
                        let slice_path = dir.join(&slice.file_name);
                        if let Ok(mut file) = fs::File::open(&slice_path).await {
                            let file_off = start - slice.offset;
                            if file.seek(std::io::SeekFrom::Start(file_off)).await.is_ok() {
                                let cache_end_incl = (slice.end() - 1).min(end);
                                let cached_len = cache_end_incl - start + 1;
                                let total_resp_len = end - start + 1;
                                self.on_stream_bytes(&key, cache_end_incl + 1);

                                let mut resp = Response::builder().status(if has_range {
                                    StatusCode::PARTIAL_CONTENT
                                } else {
                                    StatusCode::OK
                                });
                                if let Ok(ct) = HeaderValue::from_str(&meta.content_type) {
                                    resp = resp.header(header::CONTENT_TYPE, ct);
                                }
                                resp = resp
                                    .header(header::ACCEPT_RANGES, "bytes")
                                    .header(header::CONTENT_LENGTH, total_resp_len.to_string());
                                if has_range {
                                    resp = resp.header(
                                        header::CONTENT_RANGE,
                                        format!("bytes {start}-{end}/{}", meta.file.length),
                                    );
                                }

                                let needs_upstream = cache_end_incl < end;
                                let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(8);
                                let cache_clone = self.clone();
                                let ts_clone = ts.clone();
                                let key_clone = key.clone();
                                let url_clone = upstream_url.clone();
                                tokio::spawn(async move {
                                    let _keep_guard = guard;
                                    let mut rem = cached_len;
                                    let mut buf = vec![0u8; READ_CHUNK_SIZE];
                                    while rem > 0 {
                                        let want = (rem as usize).min(buf.len());
                                        match file.read(&mut buf[..want]).await {
                                            Ok(0) => break,
                                            Ok(n) => {
                                                rem -= n as u64;
                                                let chunk = axum::body::Bytes::copy_from_slice(&buf[..n]);
                                                if tx.send(Ok(chunk)).await.is_err() {
                                                    return;
                                                }
                                            }
                                            Err(e) => {
                                                let _ = tx.send(Err(e)).await;
                                                return;
                                            }
                                        }
                                    }
                                    if !needs_upstream || rem > 0 {
                                        return;
                                    }
                                    // Stream continuation seamlessly from TorrServer starting right after slice_end.
                                    cache_clone.await_torrserver_ready(&ts_clone, &key_clone).await;
                                    let next_start = cache_end_incl + 1;
                                    let req = ts_clone
                                        .get(&url_clone)
                                        .header(header::RANGE, format!("bytes={next_start}-{end}"));
                                    let Ok(Ok(upstream)) = tokio::time::timeout(STREAM_TIMEOUT, req.send()).await
                                    else {
                                        return;
                                    };
                                    if !upstream.status().is_success() {
                                        return;
                                    }
                                    let mut up_stream = upstream.bytes_stream();
                                    let mut sent_up = 0u64;
                                    let expected_up = end - cache_end_incl;
                                    while let Some(item) = tokio::select! {
                                        biased;
                                        _ = tx.closed() => None,
                                        it = up_stream.next() => it,
                                    } {
                                        match item {
                                            Ok(mut bytes) => {
                                                if sent_up >= expected_up {
                                                    break;
                                                }
                                                let left = (expected_up - sent_up) as usize;
                                                if bytes.len() > left {
                                                    bytes = bytes.slice(..left);
                                                }
                                                sent_up += bytes.len() as u64;
                                                cache_clone.on_stream_bytes(&key_clone, next_start + sent_up);
                                                if tx.send(Ok(bytes)).await.is_err() {
                                                    return;
                                                }
                                                if sent_up >= expected_up {
                                                    break;
                                                }
                                            }
                                            Err(e) => {
                                                let _ = tx.send(Err(std::io::Error::other(e))).await;
                                                return;
                                            }
                                        }
                                    }
                                });

                                let body_stream = futures_util::stream::unfold(rx, |mut rx| async move {
                                    rx.recv().await.map(|item| (item, rx))
                                });
                                return resp
                                    .body(Body::from_stream(body_stream))
                                    .unwrap_or_else(|_| bad_gateway());
                            }
                        }
                    }
                }
            }
        }

        // Cache miss: ensure background add completed (if one was started) and proxy to TorrServer.
        if is_get {
            self.await_torrserver_ready(ts, &key).await;
        }
        let mut r = if *method == Method::HEAD {
            ts.head(&upstream_url)
        } else {
            ts.get(&upstream_url)
        };
        if let Some(range) = headers.get(header::RANGE) {
            r = r.header(header::RANGE, range.as_bytes());
        }
        let upstream = match tokio::time::timeout(STREAM_TIMEOUT, r.send()).await {
            Ok(Ok(u)) => u,
            _ => return bad_gateway(),
        };
        let status = StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let mut resp = Response::builder().status(status);
        for name in [
            header::CONTENT_TYPE,
            header::CONTENT_LENGTH,
            header::CONTENT_RANGE,
            header::ACCEPT_RANGES,
        ] {
            if let Some(v) = upstream.headers().get(name.as_str()) {
                if let Ok(v) = HeaderValue::from_bytes(v.as_bytes()) {
                    resp = resp.header(name, v);
                }
            }
        }
        if !is_get {
            return resp
                .body(Body::from_stream(upstream.bytes_stream()))
                .unwrap_or_else(|_| bad_gateway());
        }
        let start_offset = upstream
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range_start)
            .or_else(|| {
                headers
                    .get(header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.strip_prefix("bytes="))
                    .and_then(|s| s.split('-').next())
                    .and_then(|s| s.parse::<u64>().ok())
            })
            .unwrap_or(0);
        self.on_stream_bytes(&key, start_offset);
        let cache_clone = self.clone();
        let mut up_stream = upstream.bytes_stream();
        let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(8);
        tokio::spawn(async move {
            let _keep_guard = guard;
            let mut cur = start_offset;
            while let Some(item) = tokio::select! {
                biased;
                _ = tx.closed() => None,
                it = up_stream.next() => it,
            } {
                match item {
                    Ok(bytes) => {
                        cur = cur.saturating_add(bytes.len() as u64);
                        cache_clone.on_stream_bytes(&key, cur);
                        if tx.send(Ok(bytes)).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(std::io::Error::other(e))).await;
                        return;
                    }
                }
            }
        });
        let body_stream =
            futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) });
        resp.body(Body::from_stream(body_stream))
            .unwrap_or_else(|_| bad_gateway())
    }
}

struct ActiveStreamGuard {
    cache: PauseCache,
    key: CacheKey,
}

impl ActiveStreamGuard {
    fn new(cache: PauseCache, key: CacheKey) -> Self {
        cache.on_stream_open(&key);
        Self { cache, key }
    }
}

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        self.cache.on_stream_close(&self.key);
    }
}

#[derive(Clone)]
struct LoopbackState {
    cache: PauseCache,
    ts: TorrServer,
}

async fn internal_stream(
    State(st): State<LoopbackState>,
    AxumPath((secret, uid, hash, index)): AxumPath<(String, i64, String, i32)>,
    req: Request,
) -> Response {
    if secret != st.cache.inner.secret {
        return StatusCode::FORBIDDEN.into_response();
    }
    st.cache
        .serve_stream(&st.ts, uid, &hash, index, req.method(), req.headers())
        .await
}

async fn probe_stream(
    State(st): State<LoopbackState>,
    AxumPath((secret, probe_id)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if secret != st.cache.inner.secret {
        return StatusCode::FORBIDDEN.into_response();
    }
    let session = {
        st.cache
            .inner
            .probes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&probe_id)
            .cloned()
    };
    let Some(session) = session else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let range_hdr = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let Ok((has_range, start, mut end)) = parse_range(range_hdr, session.file_len) else {
        return (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{}", session.file_len))],
        )
            .into_response();
    };

    // Serve from in-memory head if inside head.
    if start < session.head.len() as u64 {
        let end_incl = end.min(session.head.len() as u64 - 1);
        let slice = session.head[start as usize..=end_incl as usize].to_vec();
        return slice_response(has_range, start, end_incl, session.file_len, slice);
    }
    // Serve from in-memory tail if inside tail.
    if !session.tail.is_empty() && start >= session.tail_offset {
        let rel = (start - session.tail_offset) as usize;
        if rel < session.tail.len() {
            let max_rel = (end - session.tail_offset) as usize;
            let end_rel = max_rel.min(session.tail.len() - 1);
            let slice = session.tail[rel..=end_rel].to_vec();
            return slice_response(
                has_range,
                start,
                session.tail_offset + end_rel as u64,
                session.file_len,
                slice,
            );
        }
    }

    // Near-EOF metadata (e.g. MP4 moov or MKV Cues just before last 2 MiB): record & serve up to 4 MiB.
    if start >= session.file_len.saturating_sub(16 * 1024 * 1024) {
        end = end.min(start.saturating_add(4 * 1024 * 1024 - 1));
        let Ok((bytes, _, _)) = fetch_bytes_range(&session.ts, &session.stream_url, start, end).await else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        if !bytes.is_empty() {
            *session.extra_tail.lock().unwrap_or_else(|e| e.into_inner()) = Some((start, bytes.clone()));
        }
        let end_incl = start + bytes.len().saturating_sub(1) as u64;
        return slice_response(has_range, start, end_incl, session.file_len, bytes);
    }

    // Media seek target! Record it and serve a small chunk (up to 1 MiB) so ffmpeg finds the keyframe quickly.
    {
        let mut rec = session.recorded_seek.lock().unwrap_or_else(|e| e.into_inner());
        if rec.is_none() {
            *rec = Some(start);
        }
    }
    end = end.min(start.saturating_add(1024 * 1024 - 1));
    let Ok((bytes, _, _)) = fetch_bytes_range(&session.ts, &session.stream_url, start, end).await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let end_incl = start + bytes.len().saturating_sub(1) as u64;
    slice_response(has_range, start, end_incl, session.file_len, bytes)
}

fn slice_response(has_range: bool, start: u64, end_incl: u64, total: u64, data: Vec<u8>) -> Response {
    let status = if has_range {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let mut resp = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, data.len().to_string());
    if has_range {
        resp = resp.header(header::CONTENT_RANGE, format!("bytes {start}-{end_incl}/{total}"));
    }
    resp.body(Body::from(data))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn bad_gateway() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "Потоковый сервер не отвечает").into_response()
}

fn parse_range(hdr: Option<&str>, total: u64) -> Result<(bool, u64, u64), ()> {
    let Some(raw) = hdr.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok((false, 0, total.saturating_sub(1)));
    };
    let spec = raw.strip_prefix("bytes=").ok_or(())?;
    let first = spec.split(',').next().ok_or(())?.trim();
    let (a, b) = first.split_once('-').ok_or(())?;
    if total == 0 {
        return Err(());
    }
    if a.is_empty() {
        let suffix: u64 = b.parse().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let start = total.saturating_sub(suffix);
        return Ok((true, start, total - 1));
    }
    let start: u64 = a.parse().map_err(|_| ())?;
    if start >= total {
        return Err(());
    }
    let end = if b.is_empty() {
        total - 1
    } else {
        let e: u64 = b.parse().map_err(|_| ())?;
        if e < start {
            return Err(());
        }
        e.min(total - 1)
    };
    Ok((true, start, end))
}

fn parse_content_range_start(hdr: &str) -> Option<u64> {
    let rest = hdr.trim().strip_prefix("bytes ")?;
    let (range, _) = rest.split_once('/')?;
    let (start, _) = range.split_once('-')?;
    start.parse().ok()
}

fn parse_content_range_total(hdr: &str) -> Option<u64> {
    let (_, total) = hdr.trim().split_once('/')?;
    total.parse().ok()
}

async fn fetch_bytes_range(
    ts: &TorrServer,
    url: &str,
    start: u64,
    end_incl: u64,
) -> anyhow::Result<(Vec<u8>, u64, Option<String>)> {
    let resp = tokio::time::timeout(
        Duration::from_secs(20),
        ts.get(url)
            .header(header::RANGE, format!("bytes={start}-{end_incl}"))
            .send(),
    )
    .await??;
    anyhow::ensure!(resp.status().is_success(), "torrserver returned {}", resp.status());
    let total_len = resp
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range_total)
        .or_else(|| {
            if start == 0 {
                resp.headers()
                    .get(header::CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse().ok())
            } else {
                None
            }
        })
        .unwrap_or(0);
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let expected = (end_incl - start + 1) as usize;
    let mut out = Vec::with_capacity(expected.min(8 * 1024 * 1024));
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let need = expected.saturating_sub(out.len());
        if need == 0 {
            break;
        }
        let take = chunk.len().min(need);
        out.extend_from_slice(&chunk[..take]);
        if out.len() >= expected {
            break;
        }
    }
    Ok((out, total_len, content_type))
}

async fn stream_range_to_file(
    ts: &TorrServer,
    url: &str,
    start: u64,
    end_incl: u64,
    path: &Path,
) -> anyhow::Result<u64> {
    let resp = tokio::time::timeout(
        Duration::from_secs(25),
        ts.get(url)
            .header(header::RANGE, format!("bytes={start}-{end_incl}"))
            .send(),
    )
    .await??;
    anyhow::ensure!(resp.status().is_success(), "torrserver returned {}", resp.status());
    let expected = end_incl - start + 1;
    let mut file = fs::File::create(path).await?;
    let mut written = 0u64;
    let mut stream = resp.bytes_stream();
    let deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            chunk = stream.next() => {
                let Some(chunk) = chunk else { break };
                let chunk = chunk?;
                let left = (expected - written) as usize;
                if left == 0 {
                    break;
                }
                let take = chunk.len().min(left);
                file.write_all(&chunk[..take]).await?;
                written += take as u64;
                if written >= expected {
                    break;
                }
            }
        }
    }
    file.flush().await?;
    Ok(written)
}

async fn write_meta(dir: &Path, meta: &CachedEntryMeta) -> anyhow::Result<()> {
    let tmp = dir.join("meta.json.tmp");
    let final_path = dir.join("meta.json");
    let json = serde_json::to_vec(meta)?;
    fs::write(&tmp, json).await?;
    fs::rename(tmp, final_path).await?;
    Ok(())
}

fn load_existing_entries(root: &Path) -> HashMap<CacheKey, CachedEntryMeta> {
    let mut map = HashMap::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return map;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(".staging_") {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        let Ok(bytes) = std::fs::read(path.join("meta.json")) else {
            continue;
        };
        let Ok(meta) = serde_json::from_slice::<CachedEntryMeta>(&bytes) else {
            continue;
        };
        if meta.slices.iter().all(|s| path.join(&s.file_name).is_file()) {
            map.insert(meta.key(), meta);
        }
    }
    map
}

fn guess_content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("avi") => "video/x-msvideo",
        Some("mov") => "video/quicktime",
        Some("ts" | "m2ts") => "video/mp2t",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;
    use http_body_util::BodyExt;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kino_pause_cache_{name}_{}", rand::rng().random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_meta(
        user_id: i64,
        hash: &str,
        season: i32,
        episode: i32,
        pos: f64,
        slices: Vec<CachedSlice>,
    ) -> CachedEntryMeta {
        let now = Utc::now().timestamp();
        CachedEntryMeta {
            user_id,
            kind: if season > 0 { MediaKind::Tv } else { MediaKind::Movie },
            tmdb_id: 42,
            season,
            episode,
            infohash: hash.into(),
            file_index: 1,
            file: FileEntry {
                index: 1,
                path: "movie.mkv".into(),
                length: 1000,
            },
            probe: Probe {
                duration: Some(3600.0),
                container: "matroska,webm".into(),
                video_codec: Some("hevc".into()),
                ..Probe::default()
            },
            content_type: "video/x-matroska".into(),
            position_sec: pos,
            created_at_unix: now,
            last_accessed_unix: now,
            slices,
        }
    }

    #[test]
    fn parses_http_ranges() {
        assert_eq!(parse_range(None, 100), Ok((false, 0, 99)));
        assert_eq!(parse_range(Some("bytes=10-"), 100), Ok((true, 10, 99)));
        assert_eq!(parse_range(Some("bytes=10-25"), 100), Ok((true, 10, 25)));
        assert_eq!(parse_range(Some("bytes=10-500"), 100), Ok((true, 10, 99)));
        assert_eq!(parse_range(Some("bytes=-20"), 100), Ok((true, 80, 99)));
        assert_eq!(parse_range(Some("bytes=100-"), 100), Err(()));
        assert_eq!(parse_range(Some("bytes=20-10"), 100), Err(()));
    }

    #[tokio::test]
    async fn lookup_hits_within_tolerance_and_skips_mismatch() {
        let dir = temp_dir("lookup");
        let cache = PauseCache::new(Some(dir.clone()), 5120, 72);
        let hash = "a".repeat(40);
        let key = CacheKey::new(1, &hash, 1);
        let entry_dir = cache.entry_dir(&key).unwrap();
        fs::create_dir_all(&entry_dir).await.unwrap();
        fs::write(entry_dir.join("window.bin"), vec![1u8; 64]).await.unwrap();
        let meta = sample_meta(
            1,
            &hash,
            0,
            0,
            1200.0,
            vec![CachedSlice {
                offset: 300,
                len: 64,
                file_name: "window.bin".into(),
            }],
        );
        write_meta(&entry_dir, &meta).await.unwrap();
        cache.inner.entries.lock().unwrap().insert(key, meta);

        assert!(cache.lookup_for_play(1, &hash, 0, 0, 1210).is_some());
        assert!(cache.lookup_for_play(1, &hash, 0, 0, 100).is_none());
        assert!(cache.lookup_for_play(2, &hash, 0, 0, 1200).is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn serve_stream_splices_ssd_cache_and_torrserver() {
        let dir = temp_dir("splice");
        let cache = PauseCache::new(Some(dir.clone()), 5120, 72);
        let hash = "b".repeat(40);
        let key = CacheKey::new(1, &hash, 1);

        // Full 100-byte file: bytes 0..40 in SSD cache, bytes 40..100 on mock TorrServer.
        let full_data: Vec<u8> = (0..100u8).collect();
        let full_for_ts = full_data.clone();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ts_url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/stream/video",
            get(move |headers: HeaderMap| {
                let data = full_for_ts.clone();
                async move {
                    let rng = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
                    let (_, s, e) = parse_range(rng, data.len() as u64).unwrap();
                    (
                        StatusCode::PARTIAL_CONTENT,
                        [(header::CONTENT_RANGE, format!("bytes {s}-{e}/{}", data.len()))],
                        data[s as usize..=e as usize].to_vec(),
                    )
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let ts = TorrServer::new(&ts_url, None);

        let entry_dir = cache.entry_dir(&key).unwrap();
        fs::create_dir_all(&entry_dir).await.unwrap();
        fs::write(entry_dir.join("window.bin"), &full_data[20..50])
            .await
            .unwrap();
        let mut meta = sample_meta(
            1,
            &hash,
            0,
            0,
            100.0,
            vec![CachedSlice {
                offset: 20,
                len: 30,
                file_name: "window.bin".into(),
            }],
        );
        meta.file.length = 100;
        write_meta(&entry_dir, &meta).await.unwrap();
        cache.inner.entries.lock().unwrap().insert(key, meta);

        // Open-ended range starting at 25: 25..50 from SSD + 50..100 from TorrServer!
        let mut hdrs = HeaderMap::new();
        hdrs.insert(header::RANGE, HeaderValue::from_static("bytes=25-"));
        let resp = cache.serve_stream(&ts, 1, &hash, 1, &Method::GET, &hdrs).await;
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        let body = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
        assert_eq!(body, full_data[25..100]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn prunes_expired_and_evicts_lru_above_budget() {
        let dir = temp_dir("prune");
        // 1 MiB budget
        let cache = PauseCache::new(Some(dir.clone()), 1, 72);
        let h1 = "1".repeat(40);
        let h2 = "2".repeat(40);
        let h3 = "3".repeat(40);

        for (i, (h, age_secs, sz)) in [
            (h1.clone(), 100, 600_000u64),
            (h2.clone(), 10, 600_000),
            (h3.clone(), 100 * 3600, 100_000),
        ]
        .into_iter()
        .enumerate()
        {
            let k = CacheKey::new(1, &h, 1);
            let ed = cache.entry_dir(&k).unwrap();
            fs::create_dir_all(&ed).await.unwrap();
            fs::write(ed.join("window.bin"), vec![0u8; sz as usize]).await.unwrap();
            let mut m = sample_meta(
                1,
                &h,
                0,
                0,
                100.0 + i as f64,
                vec![CachedSlice {
                    offset: 0,
                    len: sz,
                    file_name: "window.bin".into(),
                }],
            );
            m.last_accessed_unix = Utc::now().timestamp() - age_secs;
            write_meta(&ed, &m).await.unwrap();
            cache.inner.entries.lock().unwrap().insert(k, m);
        }

        cache.prune().await;
        let guard = cache.inner.entries.lock().unwrap();
        // h3 expired (>72h), h1 evicted by LRU so total <= 1 MiB, only h2 remains!
        assert!(!guard.contains_key(&CacheKey::new(1, &h3, 1)));
        assert!(!guard.contains_key(&CacheKey::new(1, &h1, 1)));
        assert!(guard.contains_key(&CacheKey::new(1, &h2, 1)));
        drop(guard);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn progress_at_credits_evicts_cache() {
        let dir = temp_dir("credits");
        let cache = PauseCache::new(Some(dir.clone()), 5120, 72);
        let hash = "c".repeat(40);
        let k = CacheKey::new(1, &hash, 1);
        let ed = cache.entry_dir(&k).unwrap();
        fs::create_dir_all(&ed).await.unwrap();
        fs::write(ed.join("window.bin"), vec![1u8; 32]).await.unwrap();
        let m = sample_meta(
            1,
            &hash,
            0,
            0,
            500.0,
            vec![CachedSlice {
                offset: 0,
                len: 32,
                file_name: "window.bin".into(),
            }],
        );
        write_meta(&ed, &m).await.unwrap();
        cache.inner.entries.lock().unwrap().insert(k.clone(), m);

        cache.on_progress(1, MediaKind::Movie, 42, 0, 0, Some(&hash), 920.0, 1000.0, false);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!cache.inner.entries.lock().unwrap().contains_key(&k));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn internal_loopback_serves_cached_and_spliced_ranges() {
        let dir = temp_dir("loopback");
        let cache = PauseCache::new(Some(dir.clone()), 5120, 72);
        let hash = "d".repeat(40);
        let key = CacheKey::new(7, &hash, 1);
        let full_data: Vec<u8> = (0..120u8).collect();
        let full_for_ts = full_data.clone();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ts_url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/stream/video",
            get(move |headers: HeaderMap| {
                let data = full_for_ts.clone();
                async move {
                    let rng = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
                    let (_, s, e) = parse_range(rng, data.len() as u64).unwrap();
                    (
                        StatusCode::PARTIAL_CONTENT,
                        [(header::CONTENT_RANGE, format!("bytes {s}-{e}/{}", data.len()))],
                        data[s as usize..=e as usize].to_vec(),
                    )
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let ts = TorrServer::new(&ts_url, None);

        let entry_dir = cache.entry_dir(&key).unwrap();
        fs::create_dir_all(&entry_dir).await.unwrap();
        fs::write(entry_dir.join("head.bin"), &full_data[0..20]).await.unwrap();
        fs::write(entry_dir.join("window.bin"), &full_data[40..70])
            .await
            .unwrap();
        let mut meta = sample_meta(
            7,
            &hash,
            0,
            0,
            600.0,
            vec![
                CachedSlice {
                    offset: 0,
                    len: 20,
                    file_name: "head.bin".into(),
                },
                CachedSlice {
                    offset: 40,
                    len: 30,
                    file_name: "window.bin".into(),
                },
            ],
        );
        meta.file.length = full_data.len() as u64;
        write_meta(&entry_dir, &meta).await.unwrap();
        cache.inner.entries.lock().unwrap().insert(key, meta);

        let url = cache.internal_stream_url(&ts, 7, &hash, 1).await.unwrap();
        let client = reqwest::Client::new();
        // Header from SSD
        let head_resp = client
            .get(&url)
            .header(header::RANGE, "bytes=0-15")
            .send()
            .await
            .unwrap();
        assert_eq!(head_resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(head_resp.bytes().await.unwrap().as_ref(), &full_data[0..16]);

        // Resume window from SSD spliced seamlessly into TorrServer continuation
        let resume_resp = client
            .get(&url)
            .header(header::RANGE, "bytes=45-")
            .send()
            .await
            .unwrap();
        assert_eq!(resume_resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(resume_resp.bytes().await.unwrap().as_ref(), &full_data[45..120]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn capture_saves_entry_and_respects_short_and_credits_filters() {
        let dir = temp_dir("capture");
        let cache = PauseCache::new(Some(dir.clone()), 5120, 72);
        let hash = "e".repeat(40);
        let full_data: Vec<u8> = (0..1024usize).map(|i| (i % 251) as u8).collect();
        let full_for_ts = full_data.clone();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ts_url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/stream/video",
            get(move |headers: HeaderMap| {
                let data = full_for_ts.clone();
                async move {
                    let rng = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
                    let (_, s, e) = parse_range(rng, data.len() as u64).unwrap();
                    (
                        StatusCode::PARTIAL_CONTENT,
                        [(header::CONTENT_RANGE, format!("bytes {s}-{e}/{}", data.len()))],
                        data[s as usize..=e as usize].to_vec(),
                    )
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let ts = TorrServer::new(&ts_url, None);

        let src = Source {
            key: hash.clone(),
            infohash: Some(hash.clone()),
            title: "Test Movie".into(),
            link: None,
            magnet: None,
            torrent: None,
        };
        let file = FileEntry {
            index: 1,
            path: "Test.Movie.mkv".into(),
            length: full_data.len() as u64,
        };
        let probe = Probe {
            duration: Some(1000.0),
            container: "matroska,webm".into(),
            video_codec: Some("h264".into()),
            ..Probe::default()
        };

        // 1) Too close to start (< 15s) -> skipped
        cache.register_play(
            9,
            MediaKind::Movie,
            99,
            0,
            0,
            src.clone(),
            file.clone(),
            probe.clone(),
            5.0,
            ts.clone(),
            "kino-test-no-ffmpeg".into(),
        );
        let k = CacheKey::new(9, &hash, 1);
        let gen = cache.inner.contexts.lock().unwrap().get(&k).unwrap().capture_gen;
        cache.capture_if_eligible(k.clone(), gen).await;
        assert!(cache.lookup_for_play(9, &hash, 0, 0, 5).is_none());

        // 2) Credits (>= 90%) -> skipped
        cache.on_progress(9, MediaKind::Movie, 99, 0, 0, Some(&hash), 910.0, 1000.0, false);
        let gen = cache.inner.contexts.lock().unwrap().get(&k).unwrap().capture_gen;
        cache.capture_if_eligible(k.clone(), gen).await;
        assert!(cache.lookup_for_play(9, &hash, 0, 0, 910).is_none());

        // 3) Mid-movie pause (400s / 1000s) -> saved to disk and immediately hits lookup_for_play
        cache.on_progress(9, MediaKind::Movie, 99, 0, 0, Some(&hash), 400.0, 1000.0, false);
        let gen = cache.inner.contexts.lock().unwrap().get(&k).unwrap().capture_gen;
        cache.capture_if_eligible(k.clone(), gen).await;
        let hit = cache.lookup_for_play(9, &hash, 0, 0, 400);
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().probe.duration, Some(1000.0));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remembers_and_reuses_probe_across_devices_and_skips_empty_probe() {
        let cache = PauseCache::new(None, 5120, 72);
        let hash = "f".repeat(40);
        cache.remember_probe(&hash, 2, &Probe::default());
        assert!(cache.lookup_probe(&hash, 2).is_none());

        let p = Probe {
            duration: Some(2400.0),
            container: "matroska,webm".into(),
            video_codec: Some("h264".into()),
            ..Probe::default()
        };
        cache.remember_probe(&hash, 2, &p);
        assert_eq!(cache.lookup_probe(&hash, 2), Some(p));
        assert!(cache.lookup_probe(&hash, 3).is_none());
    }
}
