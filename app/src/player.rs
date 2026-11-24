//! Drives the `<video>` of [`PlayerView`]: starts playback on the server,
//! seeks, switches tracks, saves the position, moves to the next episode and
//! keeps multiple devices of the same account synchronized in real time.

use dioxus::prelude::*;
use kino_core::{
    episode_code, normalize_quality_key, EpisodeRating, MediaKind, MediaTrack, PlayInfo, PlayProbeRequest, PlayRequest,
    ProgressUpdate, ReleaseChoiceUpdate, ReleaseRef, ResolveReleaseRequest, ResolveReleaseResponse, RewatchMode,
    StreamPhase, SyncClientMsg, SyncEventKind, SyncJoinRequest, SyncLeaveRequest, SyncServerMsg, SyncState,
    TracksUpdate,
};
use kino_ui::{PlayerEpisode, PlayerMenu, PlayerState, PlayerView, ReleaseOption, VideoScale, NEXT_PROMPT_SEC};

use crate::{
    api, native, platform,
    platform::video::{video, PlayOutcome},
    sync, telegram,
};

/// What to play.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayStart {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub title: String,
    pub poster: Option<String>,
    pub season: i32,
    pub episode: i32,
    /// Remembered releases per season (for the next season).
    pub choices: Vec<ReleaseChoiceUpdate>,
    /// Release of this season.
    pub release: ReleaseChoiceUpdate,
    /// True when the user explicitly tapped a specific episode instead of Resume.
    pub explicit_episode: bool,
    /// True when the user explicitly picked a release in `ReleasePicker`.
    pub explicit_release: bool,
    /// Remembered picture scaling mode for this title from the server DB.
    pub video_scale: Option<VideoScale>,
    /// Episode filter active while rewatching favorites (`All` normally).
    pub rewatch_mode: RewatchMode,
    /// User's manual 1–5 ratings per episode (used by `RewatchMode::Only5` / `FourPlus`).
    pub episode_ratings: Vec<EpisodeRating>,
}

fn next_target_episode(
    cur_season: i32,
    cur_ep: i32,
    server_next: Option<(i32, i32)>,
    mode: RewatchMode,
    ratings: &[EpisodeRating],
) -> Option<(i32, i32)> {
    if mode == RewatchMode::All {
        return server_next;
    }
    let mut matching: Vec<(i32, i32)> = ratings
        .iter()
        .filter(|e| e.season > 0 && mode.matches(Some(e.rating)) && (e.season, e.episode) > (cur_season, cur_ep))
        .map(|e| (e.season, e.episode))
        .collect();
    matching.sort_unstable();
    matching.into_iter().next()
}

/// How long a background-prepared stream for the next episode stays warm in TorrServer.
const PREFETCH_TORR_TTL_MS: i64 = 75_000;
/// Refresh background-prepared live streams if older than this when reaching the prompt window again.
const PREFETCH_REFRESH_MS: i64 = 60_000;
/// Wait until the current HTML `<video>` has at least this many seconds buffered ahead (unless near the very end).
const PREFETCH_MIN_BUFFER_SEC: f64 = 5.0;
const PREFETCH_FORCE_REMAINING_SEC: f64 = 20.0;

#[derive(Debug, Clone, PartialEq)]
enum NextPrefetchStatus {
    InFlight,
    Ready { info: PlayInfo, prepared_at_ms: i64 },
    NeedsPicker,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
struct NextPrefetch {
    gen: u32,
    from_season: i32,
    from_episode: i32,
    from_infohash: String,
    next_season: i32,
    next_episode: i32,
    resolved_choice: Option<ReleaseChoiceUpdate>,
    status: NextPrefetchStatus,
}

impl NextPrefetch {
    fn matches_source(&self, season: i32, episode: i32, infohash: &str, ns: i32, ne: i32) -> bool {
        self.from_season == season
            && self.from_episode == episode
            && self.from_infohash.eq_ignore_ascii_case(infohash)
            && self.next_season == ns
            && self.next_episode == ne
    }

    fn matches_play(&self, season: i32, episode: i32, infohash: &str) -> bool {
        self.next_season == season
            && self.next_episode == episode
            && self
                .resolved_choice
                .as_ref()
                .is_some_and(|c| c.infohash.eq_ignore_ascii_case(infohash))
    }
}

fn resolve_response_to_choice(res: ResolveReleaseResponse) -> Option<ReleaseChoiceUpdate> {
    match res {
        ResolveReleaseResponse::Ready { choice, .. } => Some(choice),
        ResolveReleaseResponse::PreloadQualityMismatch {
            preloaded, fallback, ..
        } => match *fallback {
            ResolveReleaseResponse::Ready { choice, .. } => Some(choice),
            _ => Some(preloaded),
        },
        ResolveReleaseResponse::NeedsPicker { .. } => None,
    }
}

fn upsert_choice(choices: &mut Vec<ReleaseChoiceUpdate>, choice: ReleaseChoiceUpdate) {
    choices
        .retain(|c| !(c.season == choice.season && c.episode == choice.episode && c.quality_key == choice.quality_key));
    choices.insert(0, choice);
}

fn prefetch_is_fresh(info: &PlayInfo, prepared_at_ms: i64, now_ms: i64, ttl_ms: i64) -> bool {
    info.preloaded || now_ms.saturating_sub(prepared_at_ms) <= ttl_ms
}

fn can_prefetch_next(pos: f64, dur: f64, buffered_ahead: Option<f64>) -> bool {
    if !pos.is_finite() || !dur.is_finite() || dur <= 20.0 || pos < 5.0 {
        return false;
    }
    let remaining = dur - pos;
    if remaining >= NEXT_PROMPT_SEC {
        return false;
    }
    match buffered_ahead {
        Some(buf) if remaining > PREFETCH_FORCE_REMAINING_SEC => {
            buf >= PREFETCH_MIN_BUFFER_SEC || pos + buf >= dur - 2.0
        }
        _ => true,
    }
}

fn needs_prefetch_refresh(
    existing: Option<&NextPrefetch>,
    season: i32,
    ep: i32,
    infohash: &str,
    ns: i32,
    ne: i32,
    now_ms: i64,
) -> bool {
    let Some(p) = existing.filter(|p| p.matches_source(season, ep, infohash, ns, ne)) else {
        return true;
    };
    match &p.status {
        NextPrefetchStatus::InFlight | NextPrefetchStatus::NeedsPicker | NextPrefetchStatus::Failed => false,
        NextPrefetchStatus::Ready { info, prepared_at_ms } => {
            !prefetch_is_fresh(info, *prepared_at_ms, now_ms, PREFETCH_REFRESH_MS)
        }
    }
}

fn best_per_quality(
    list: &[ReleaseOption],
    choices: &[ReleaseChoiceUpdate],
    active: &ReleaseChoiceUpdate,
    season: i32,
) -> Vec<ReleaseOption> {
    let mut out = Vec::new();
    let active_q = active.quality.as_deref().map(normalize_quality_key);
    let active_voice = active.voice.as_deref().unwrap_or("");
    for bucket in ["2160p", "1080p", "720p", "SD"] {
        if active_q == Some(bucket) {
            if let Some(exact) = list.iter().find(|r| r.infohash == active.infohash) {
                out.push(exact.clone());
                continue;
            }
        }
        let user_hash = choices
            .iter()
            .filter(|c| {
                c.quality_key
                    .as_deref()
                    .or(c.quality.as_deref())
                    .map(normalize_quality_key)
                    == Some(bucket)
            })
            .find(|c| c.season == season)
            .or_else(|| {
                choices.iter().find(|c| {
                    c.season == 0
                        && c.quality_key
                            .as_deref()
                            .or(c.quality.as_deref())
                            .map(normalize_quality_key)
                            == Some(bucket)
                })
            })
            .map(|c| c.infohash.as_str());
        let best = list
            .iter()
            .filter(|r| normalize_quality_key(&r.quality) == bucket)
            .max_by_key(|r| {
                let is_user = user_hash.is_some_and(|h| h == r.infohash);
                let peer_picks = r.picked_by.len();
                let same_voice = !active_voice.is_empty() && r.voice.eq_ignore_ascii_case(active_voice);
                (r.preloaded, is_user, peer_picks, same_voice, r.seeders, r.size_bytes)
            });
        if let Some(r) = best {
            out.push(r.clone());
        }
    }
    out
}

/// How often the position is saved while playing, seconds of video.
const SAVE_EVERY: f64 = 10.0;

/// How often the player sends a sync heartbeat & checks drift, milliseconds.
const SYNC_TICK_MS: u32 = 2_000;

/// How long the controls stay on screen during playback after user activity, milliseconds.
const CONTROLS_MS: u32 = 3_500;

const SCALE_STORAGE_KEY: &str = "kino.player.scale";
const MAX_SAVED_SCALES: usize = 200;

fn scale_title_key(kind: MediaKind, tmdb_id: i64) -> String {
    format!("{}:{tmdb_id}", kind.as_str())
}

fn scale_code(scale: VideoScale) -> u8 {
    match scale {
        VideoScale::FitWidth => 1,
        VideoScale::FitHeight => 2,
    }
}

fn scale_from_code(code: u8) -> VideoScale {
    if code == 2 {
        VideoScale::FitHeight
    } else {
        VideoScale::FitWidth
    }
}

fn decode_saved_scales(raw: &str) -> Vec<(String, String)> {
    serde_json::from_str::<Vec<(String, String)>>(raw).unwrap_or_default()
}

fn lookup_saved_scale(entries: &[(String, String)], key: &str) -> Option<VideoScale> {
    entries
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| VideoScale::parse(v))
}

fn upsert_saved_scale(entries: &mut Vec<(String, String)>, key: &str, scale: VideoScale) -> String {
    entries.retain(|(k, _)| k != key);
    entries.push((key.to_owned(), scale.as_str().to_owned()));
    if entries.len() > MAX_SAVED_SCALES {
        let drop_n = entries.len() - MAX_SAVED_SCALES;
        entries.drain(..drop_n);
    }
    serde_json::to_string(entries).unwrap_or_else(|_| "[]".into())
}

fn apply_web_scale(saved: Option<VideoScale>) {
    let mode = match saved {
        Some(VideoScale::FitWidth) => "width",
        Some(VideoScale::FitHeight) => "height",
        None => "auto",
    };
    let _ = dioxus::document::eval(&format!(
        "window.__kinoVideoScaleMode = '{mode}'; if (window.__kinoSyncVideoScale) window.__kinoSyncVideoScale();"
    ));
}

pub use crate::platform::device_name;

fn sync_audio_index(sync_state: &Option<SyncState>, tracks: &[MediaTrack], fallback: Option<i32>) -> Option<i32> {
    sync_state
        .as_ref()
        .and_then(|st| st.audio_track.as_deref())
        .and_then(|lbl| tracks.iter().find(|a| a.label() == lbl))
        .map(|a| a.index)
        .or(fallback)
}

fn release_label(r: &ReleaseChoiceUpdate) -> String {
    match (&r.voice, &r.quality) {
        (Some(v), Some(q)) => format!("{v} · {q}"),
        _ => r.title.clone().unwrap_or_default(),
    }
}

fn waiting_label(st: &SyncState) -> String {
    if st.waiting_for.is_empty() {
        "Ждём синхронизацию с другим устройством…".into()
    } else {
        format!("Ждём синхронизацию: {}…", st.waiting_for.join(", "))
    }
}

/// Replaces or appends the cache-busting `r=` query parameter so `<video>` reloads cleanly.
fn bust_cache_url(src: &str, nonce: i64) -> String {
    let base = src
        .split_once("&r=")
        .or_else(|| src.split_once("?r="))
        .map_or(src, |(head, _)| head);
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}r={nonce}")
}

/// Aborts the `<video>` element's open network fetch and stops any active native player immediately.
fn stop_video_element() {
    platform::video::native_close();
    let _ = dioxus::document::eval(
        "document.documentElement.classList.remove('native-video'); \
         var v = document.getElementById('kino-video'); \
         if (v && typeof v.pause === 'function') { v.pause(); v.removeAttribute('src'); v.load(); }",
    );
}

/// Gives the screen back (rotation, system bars) when the player goes away.
struct PlayerModeGuard;

impl Drop for PlayerModeGuard {
    fn drop(&mut self) {
        platform::player_mode(false);
    }
}

/// Leaves Telegram's full screen when the player goes away.
struct FullscreenGuard;

impl Drop for FullscreenGuard {
    fn drop(&mut self) {
        if telegram::is_fullscreen() {
            telegram::set_fullscreen(false);
        }
    }
}

/// Removes the player's keyboard listener when the player unmounts.
struct KeyGuard;

impl Drop for KeyGuard {
    fn drop(&mut self) {
        let _ = dioxus::document::eval(
            "if (window.__kinoPlayerKeysStop) { try { window.__kinoPlayerKeysStop(); } catch (e) {} }",
        );
    }
}

/// Removes the player's video scaling observer when the player unmounts.
struct ScaleGuard;

impl Drop for ScaleGuard {
    fn drop(&mut self) {
        let _ = dioxus::document::eval(
            "if (window.__kinoPlayerScaleStop) { try { window.__kinoPlayerScaleStop(); } catch (e) {} }",
        );
    }
}

/// Ensures the sync WebSocket closes cleanly if `Player` is unmounted directly.
struct SyncGuard;

impl Drop for SyncGuard {
    fn drop(&mut self) {
        sync::stop(None);
    }
}

/// Stops the Android ExoPlayer surface if `Player` is unmounted directly.
struct NativePlayerGuard;

impl Drop for NativePlayerGuard {
    fn drop(&mut self) {
        platform::video::native_close();
        let _ = dioxus::document::eval("document.documentElement.classList.remove('native-video');");
    }
}

#[component]
pub fn Player(
    token: String,
    start: PlayStart,
    on_scale: EventHandler<VideoScale>,
    on_close: EventHandler<bool>,
    on_title: EventHandler<()>,
    /// The next season has no remembered release: pick one for `(season, episode)`.
    on_need_release: EventHandler<(i32, i32)>,
) -> Element {
    let kind = start.kind;
    let id = start.tmdb_id;
    let client_id = use_hook(platform::random_id);
    let device = use_hook(device_name);

    let rewatch_mode = start.rewatch_mode;
    let ep_ratings = start.episode_ratings.clone();
    let mut choices = use_signal(|| start.choices.clone());
    let mut target = use_signal(|| (start.season, start.episode));
    let mut release = use_signal(|| start.release.clone());
    let mut info = use_signal(|| None::<PlayInfo>);
    let mut quality_cache = use_signal(|| None::<(i32, Vec<ReleaseOption>)>);
    let mut quality_loading = use_signal(|| false);
    let mut override_seek = use_signal(|| None::<f64>);
    let mut ui = use_signal(|| PlayerState {
        title: start.title.clone(),
        poster: start.poster.clone(),
        status: Some("Подключаемся к раздаче…".into()),
        sync_client_id: client_id.clone(),
        active_infohash: Some(start.release.infohash.clone()),
        rewatch_label: (rewatch_mode != RewatchMode::All).then(|| rewatch_mode.label().to_owned()),
        ..PlayerState::default()
    });

    // Start of the current source within the file (re-encoded streams start at `offset`).
    let mut offset = use_signal(|| 0.0_f64);
    // Where to jump once a direct source has its metadata.
    let mut pending_seek = use_signal(|| None::<f64>);
    // Active direct seek target awaiting `seeked`.
    let mut seeking_target = use_signal(|| None::<f64>);
    let mut force_transcode = use_signal(|| false);
    let mut last_saved = use_signal(|| 0.0_f64);

    platform::video::use_bridge();
    // Finger or mouse: a tap on the picture shows the controls on a phone, pauses on a computer.
    let touch = use_resource(platform::touch_screen);
    // Tracks the last user interaction timestamp and whether the hide timer loop is active.
    let mut last_activity_ms = use_signal(|| 0_i64);
    let mut controls_timer = use_signal(|| false);
    let mut flash_gen = use_signal(|| 0u32);
    let mut play_gen = use_signal(|| 0u32);
    let mut stall_secs = use_signal(|| 0u32);
    let mut connecting = use_signal(|| true);
    let mut played = use_signal(|| false);
    let mut rate = use_signal(|| 1.0_f32);
    let mut volume = use_signal(|| 1.0_f32);
    let mut muted = use_signal(|| false);
    let title_scale_key = scale_title_key(kind, id);
    let db_scale = start.video_scale;
    let mut saved_scale = use_signal(|| db_scale);
    let mut active_scale = use_signal(|| db_scale.unwrap_or_default());
    let mut scales_cache = use_signal(Vec::<(String, String)>::new);
    let mut user_touched_scale = use_signal(|| false);

    // Synchronization state.
    let mut sync_state = use_signal(|| None::<SyncState>);
    let mut sync_connected = use_signal(|| false);
    let mut last_revision = use_signal(|| 0_u64);
    let mut clock = use_signal(sync::ClockSync::default);
    let mut desired_paused = use_signal(|| false);
    let mut awaiting_ready = use_signal(|| true);
    let mut canplay_seen = use_signal(|| false);
    let mut last_seek_cmd_ms = use_signal(|| 0_i64);
    let mut last_reload_ms = use_signal(|| 0_i64);
    let mut waiting_gen = use_signal(|| 0u32);
    let mut ws_gen = use_signal(|| 0u32);
    let mut closing = use_signal(|| false);
    let mut sync_init_done = use_signal(|| false);
    let mut auto_muted = use_signal(|| false);
    let mut last_prog_pause_ms = use_signal(|| 0_i64);
    let mut last_sound_cmd_ms = use_signal(|| 0_i64);
    let mut last_sync_unmute_ms = use_signal(|| 0_i64);
    let mut prefetch_gen = use_signal(|| 0u32);
    let mut next_prefetch = use_signal(|| None::<NextPrefetch>);
    let mut invalidate_prefetch = move || {
        prefetch_gen.set(prefetch_gen() + 1);
        if next_prefetch.peek().is_some() {
            next_prefetch.set(None);
        }
    };
    let mut paused_since_ms = use_signal(|| None::<i64>);
    let mut last_advance = use_signal(|| (0.0_f64, 0_i64));
    let mut auto_reloads = use_signal(|| 0_u32);

    let mut pause_video = move || {
        last_prog_pause_ms.set(platform::now_ms());
        if let Some(v) = video() {
            v.pause();
        }
    };

    let mut unmute_auto = move || {
        if !auto_muted() {
            return;
        }
        auto_muted.set(false);
        if let Some(v) = video() {
            v.set_muted(muted() || ui.peek().is_sync_muted());
        }
        ui.with_mut(|s| s.auto_muted = false);
    };

    // Shows the controls and hides them again after `CONTROLS_MS` of playback inactivity (never while paused).
    let mut show_controls = move || {
        last_activity_ms.set(platform::now_ms());
        if !ui.peek().controls {
            ui.with_mut(|s| s.controls = true);
        }
        if ui.peek().paused || ui.peek().menu.is_some() {
            return;
        }
        if *controls_timer.peek() {
            return;
        }
        controls_timer.set(true);
        spawn(async move {
            loop {
                if closing() || ui.peek().paused || !ui.peek().controls || ui.peek().menu.is_some() {
                    controls_timer.set(false);
                    break;
                }
                let elapsed = (platform::now_ms() - *last_activity_ms.peek()).max(0);
                let remain = i64::from(CONTROLS_MS) - elapsed;
                if remain <= 25 {
                    if !ui.peek().paused && ui.peek().menu.is_none() && ui.peek().controls {
                        ui.with_mut(|s| s.controls = false);
                    }
                    controls_timer.set(false);
                    break;
                }
                platform::sleep(remain as u32).await;
                if closing() || ui.peek().paused || !ui.peek().controls || ui.peek().menu.is_some() {
                    controls_timer.set(false);
                    break;
                }
            }
        });
    };

    let switch_sync_sound = {
        let self_cid = client_id.clone();
        move |target_cid: String| {
            last_sound_cmd_ms.set(platform::now_ms());
            let target_is_self = target_cid == self_cid;
            if target_is_self {
                auto_muted.set(false);
                muted.set(false);
                if volume() <= 0.001 {
                    volume.set(0.7);
                    if let Some(v) = video() {
                        v.set_volume(0.7);
                    }
                }
            }
            ui.with_mut(|s| {
                s.sync_sound_client_id = Some(target_cid.clone());
                if target_is_self {
                    s.auto_muted = false;
                }
            });
            if let Some(v) = video() {
                let should_mute = if target_is_self {
                    false
                } else {
                    ui.peek().is_sync_muted()
                };
                v.set_muted(should_mute);
            }
            sync::send(&SyncClientMsg::Sound { client_id: target_cid });
            show_controls();
        }
    };

    let mut attempt_play = move |expected_gen: u32, from_user: bool| {
        let Some(v) = video() else { return };
        if from_user && auto_muted() {
            auto_muted.set(false);
            v.set_muted(muted() || ui.peek().is_sync_muted());
            ui.with_mut(|s| s.auto_muted = false);
        }
        v.play_checked(move |outcome| {
            if closing() || play_gen() != expected_gen || desired_paused() {
                return;
            }
            if outcome == PlayOutcome::NotAllowed {
                // Keep audio unmuted and let the user start cleanly with one tap on Play.
                auto_muted.set(false);
                let alone = sync_state.peek().as_ref().is_none_or(|st| st.members.len() <= 1);
                if alone {
                    desired_paused.set(true);
                    paused_since_ms.set(Some(platform::now_ms()));
                    sync::send(&SyncClientMsg::Pause {
                        position_sec: ui.peek().position,
                    });
                }
                ui.with_mut(|s| {
                    s.auto_muted = false;
                    s.paused = true;
                    s.waiting = false;
                    s.status = None;
                    s.progress = None;
                });
                show_controls();
            }
        });
    };

    use_hook(|| std::rc::Rc::new(SyncGuard));
    use_hook(|| std::rc::Rc::new(NativePlayerGuard));

    // The episodes of the season being watched, with the ones already seen.
    let season_list = {
        let token = token.clone();
        use_resource(move || {
            let token = token.clone();
            let (season, _) = target();
            async move {
                if kind != MediaKind::Tv {
                    return Vec::new();
                }
                let Ok(list) = api::season(&token, id, season).await else {
                    return Vec::new();
                };
                let seen: Vec<i32> = api::title_progress(&token, kind, id)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .filter(|p| p.season == season && p.finished)
                    .map(|p| p.episode)
                    .collect();
                list.episodes
                    .iter()
                    .map(|e| PlayerEpisode {
                        number: e.number,
                        title: e.name.clone(),
                        watched: seen.contains(&e.number),
                    })
                    .collect()
            }
        })
    };

    // Points the video at the right source for the given audio track and second.
    let mut open_source = move |at: f64, force_reload: bool| {
        let Some(i) = info() else { return };
        let audio = ui.peek().audio_track;
        let native = platform::video::supports_native() && !force_transcode();
        let transcode = !native && (force_transcode() || !i.plays_directly(audio));
        let (mut src, off) = if transcode {
            last_reload_ms.set(platform::now_ms());
            pending_seek.set(None);
            seeking_target.set(None);
            (i.transcoded_url(audio, at), at.floor())
        } else {
            pending_seek.set((!native && at > 0.0).then_some(at));
            seeking_target.set(None);
            (i.stream.clone(), 0.0)
        };
        if force_reload {
            if !native {
                src = bust_cache_url(&src, platform::now_ms());
            }
            if transcode {
                last_reload_ms.set(platform::now_ms());
            }
        } else if transcode && ui.peek().src == src {
            if (ui.peek().position - at).abs() <= 1.5 {
                offset.set(off);
                sync::set_stream_offset(off);
                ui.with_mut(|s| s.position = at);
                if !awaiting_ready() {
                    let fi = info.peek().as_ref().map(|x| x.file_index);
                    sync::send(&SyncClientMsg::Ready {
                        position_sec: at,
                        duration_sec: ui.peek().duration,
                        file_index: fi,
                    });
                }
                return;
            }
            // Seeking back to the same start second (e.g. 0:00): bust identical URL so `<video>` actually reloads.
            src = bust_cache_url(&src, platform::now_ms());
        } else if !native && !transcode && ui.peek().src == src {
            if (ui.peek().position - at).abs() > 1.5 {
                if let Some(v) = video() {
                    if v.ready_state() >= 1 {
                        awaiting_ready.set(true);
                        canplay_seen.set(false);
                        seeking_target.set(Some(at));
                        v.set_current_time(at);
                        ui.with_mut(|s| s.position = at);
                    } else {
                        pending_seek.set(Some(at));
                        ui.with_mut(|s| s.position = at);
                    }
                }
            } else if !awaiting_ready() {
                let fi = info.peek().as_ref().map(|x| x.file_index);
                sync::send(&SyncClientMsg::Ready {
                    position_sec: ui.peek().position,
                    duration_sec: ui.peek().duration,
                    file_index: fi,
                });
            }
            return;
        }
        if !force_reload && native && ui.peek().native_video && ui.peek().src == src {
            platform::video::native_set_audio(audio);
            if (ui.peek().position - at).abs() > 1.5 {
                if let Some(v) = video() {
                    awaiting_ready.set(true);
                    seeking_target.set(Some(at));
                    v.set_current_time(at);
                    ui.with_mut(|s| s.position = at);
                }
            } else if !awaiting_ready() {
                let fi = info.peek().as_ref().map(|x| x.file_index);
                sync::send(&SyncClientMsg::Ready {
                    position_sec: ui.peek().position,
                    duration_sec: ui.peek().duration,
                    file_index: fi,
                });
            }
            return;
        }
        let (cur_s, cur_e) = *target.peek();
        let scoped_sync = sync_state
            .peek()
            .as_ref()
            .filter(|st| kind != MediaKind::Tv || (st.season == cur_s && st.episode == cur_e))
            .cloned();
        let waiting_peer = scoped_sync.as_ref().is_some_and(|st| st.waiting);
        let hold = if force_reload {
            *desired_paused.peek() || waiting_peer
        } else {
            scoped_sync
                .as_ref()
                .map_or(*desired_paused.peek(), |st| st.paused || st.waiting)
        };
        desired_paused.set(hold);
        if hold {
            paused_since_ms.set(Some(platform::now_ms()));
        } else {
            paused_since_ms.set(None);
        }
        last_advance.set((at, platform::now_ms()));
        awaiting_ready.set(true);
        canplay_seen.set(false);
        let status = if transcode {
            if at >= 1.0 {
                "Готовим видео с нужной минуты…"
            } else {
                "Запускаем перекодирование видео…"
            }
        } else if i.preloaded {
            "Открываем видео с сервера…"
        } else {
            "Запускаем видеопоток…"
        };
        offset.set(off);
        sync::set_stream_offset(off);
        sync::set_playback_state(at, ui.peek().duration, false);
        stall_secs.set(0);
        played.set(false);
        let sub_track = if i.preloaded || native {
            ui.peek().subtitle_track
        } else {
            None
        };
        if native {
            let mode = saved_scale.peek().map_or(0, scale_code);
            platform::video::native_open(&src, at, f64::from(rate()), audio, sub_track, !hold, mode);
            if let Some(v) = video() {
                v.set_volume(f64::from(volume()));
                v.set_muted(muted() || auto_muted() || ui.peek().is_sync_muted());
            }
            let _ = dioxus::document::eval("document.documentElement.classList.add('native-video');");
        } else {
            platform::video::native_close();
            let _ = dioxus::document::eval("document.documentElement.classList.remove('native-video');");
        }
        ui.with_mut(|s| {
            s.autoplay = !hold;
            s.src = src;
            s.native_video = native;
            s.transcoded = transcode;
            s.position = at;
            s.waiting = true;
            s.status = Some(status.into());
            s.progress = None;
            s.subtitles_src = if !native && i.preloaded {
                s.subtitle_track.map(|t| i.subtitle_url(t, off))
            } else {
                None
            };
        });
    };
    let mut load_source = move |at: f64| open_source(at, false);

    let save = {
        let token = token.clone();
        let device = device.clone();
        move |finished: bool| {
            let (Some(i), s) = (info.peek().clone(), ui.peek().clone()) else {
                return;
            };
            let duration = s.duration.unwrap_or(0.0).max(0.0);
            if !finished && duration <= 0.0 && s.position < 1.0 {
                return;
            }
            let (season, episode) = target();
            let rel = release.peek().clone();
            let u = ProgressUpdate {
                kind,
                tmdb_id: id,
                season: if kind == MediaKind::Tv { season } else { 0 },
                episode: if kind == MediaKind::Tv { episode } else { 0 },
                position_sec: if finished && duration > 0.0 {
                    duration
                } else {
                    s.position
                },
                duration_sec: duration,
                release: Some(ReleaseRef {
                    infohash: i.infohash.clone(),
                    file_index: i.file_index,
                    title: rel.title.clone(),
                }),
                device: Some(device.clone()),
            };
            last_saved.set(s.position);
            let t = token.clone();
            spawn(async move {
                let _ = api::save_progress(&t, &u).await;
            });
        }
    };

    let mut mark_media_ready = move || {
        awaiting_ready.set(false);
        stall_secs.set(0);
        waiting_gen.set(waiting_gen() + 1);
        let (cur_s, cur_e) = *target.peek();
        let st_opt = sync_state
            .peek()
            .as_ref()
            .filter(|st| kind != MediaKind::Tv || (st.season == cur_s && st.episode == cur_e))
            .cloned();
        let paused_by_user = st_opt.as_ref().map_or(*desired_paused.peek(), |st| st.paused);
        let waiting_peer = st_opt.as_ref().is_some_and(|st| st.waiting);
        let hold = paused_by_user || waiting_peer;
        desired_paused.set(hold);
        if hold {
            paused_since_ms.set(Some(platform::now_ms()));
        } else {
            paused_since_ms.set(None);
            last_advance.set((ui.peek().position, platform::now_ms()));
        }
        if hold {
            pause_video();
            let msg = if waiting_peer {
                st_opt.as_ref().map(waiting_label)
            } else {
                None
            };
            ui.with_mut(|s| {
                s.paused = paused_by_user;
                s.waiting = waiting_peer;
                s.progress = None;
                s.status = msg;
            });
            if paused_by_user {
                show_controls();
            }
        } else {
            let was_paused = ui.peek().paused;
            ui.with_mut(|s| {
                s.waiting = false;
                s.paused = false;
                s.progress = None;
                s.status = None;
            });
            if was_paused && ui.peek().controls {
                show_controls();
            }
            if video().is_some_and(|v| v.paused()) {
                attempt_play(play_gen(), false);
            }
        }
        let s = ui.peek();
        let file_index = info.peek().as_ref().map(|i| i.file_index);
        sync::send(&SyncClientMsg::Ready {
            position_sec: s.position,
            duration_sec: s.duration,
            file_index,
        });
    };

    // Applies a resolved `PlayInfo` (fresh or background-prefetched) to the player.
    let apply_play_result = {
        let save = save.clone();
        let ep_ratings_ctx = start.episode_ratings.clone();
        move |season: i32, episode: i32, my_gen: u32, t: String, res: Result<PlayInfo, api::Failure>| {
            let mut save_after_probe = save.clone();
            let ep_ratings_fetch = ep_ratings_ctx.clone();
            connecting.set(false);
            match res {
                Ok(i) => {
                    let synced_audio = sync_audio_index(&sync_state.peek(), &i.audio, i.audio_track);
                    let should_probe = i.duration.is_none();
                    let probe_stream = i.stream.clone();
                    let probe_native = i.native;
                    let computed_next = next_target_episode(season, episode, i.next, rewatch_mode, &ep_ratings_fetch);
                    ui.with_mut(|s| {
                        s.duration = i.duration;
                        s.audio = i.audio.clone();
                        s.subtitle_tracks = i.subtitles.clone();
                        s.audio_track = synced_audio;
                        s.subtitle_track = if i.preloaded || i.native {
                            i.subtitle_track
                        } else {
                            None
                        };
                        s.next = computed_next.map(|(s, e)| episode_code(s, e));
                        s.preloaded = i.preloaded;
                    });
                    let at = override_seek
                        .with_mut(|o| o.take())
                        .or_else(|| {
                            sync_state
                                .peek()
                                .as_ref()
                                .filter(|st| kind != MediaKind::Tv || (st.season == season && st.episode == episode))
                                .map(|st| clock.peek().expected_position(st, platform::now_ms()))
                        })
                        .unwrap_or_else(|| f64::from(i.start_at));
                    last_saved.set(at);
                    info.set(Some(i));
                    load_source(at);

                    if should_probe {
                        let probe_token = t;
                        spawn(async move {
                            for delay in [5_000_u32, 8_000, 12_000, 15_000, 20_000, 30_000] {
                                platform::sleep(delay).await;
                                if play_gen() != my_gen || closing() || ui.peek().duration.is_some() {
                                    break;
                                }
                                let probe_req = PlayProbeRequest {
                                    kind,
                                    tmdb_id: id,
                                    stream: probe_stream.clone(),
                                    native: probe_native,
                                };
                                let Ok(p) = api::play_probe(&probe_token, &probe_req).await else {
                                    continue;
                                };
                                if play_gen() != my_gen || closing() {
                                    break;
                                }
                                if p.duration.is_none() && p.audio.is_empty() && p.subtitles.is_empty() {
                                    continue;
                                }
                                // Playback already started with track 0 (or default when direct),
                                // so display the active track in the menu without jarring mid-stream reloads.
                                let fallback_audio = if ui.peek().transcoded {
                                    p.audio
                                        .iter()
                                        .find(|a| a.index == 0)
                                        .or(p.audio.first())
                                        .map(|a| a.index)
                                } else {
                                    p.audio.iter().find(|a| a.default).or(p.audio.first()).map(|a| a.index)
                                };
                                let synced_audio = sync_audio_index(&sync_state.peek(), &p.audio, fallback_audio);
                                info.with_mut(|cur| {
                                    if let Some(cur) = cur.as_mut() {
                                        if cur.duration.is_none() {
                                            cur.duration = p.duration;
                                        }
                                        cur.direct = p.direct;
                                        if let Some(s) = p.stream.clone() {
                                            cur.stream = s;
                                        }
                                        if cur.audio.is_empty() && !p.audio.is_empty() {
                                            cur.audio = p.audio.clone();
                                        }
                                        if cur.subtitles.is_empty() && !p.subtitles.is_empty() {
                                            cur.subtitles = p.subtitles.clone();
                                        }
                                    }
                                });
                                ui.with_mut(|s| {
                                    if s.duration.is_none() {
                                        s.duration = p.duration;
                                    }
                                    if s.audio.is_empty() && !p.audio.is_empty() {
                                        s.audio = p.audio.clone();
                                        if s.audio_track.is_none() {
                                            s.audio_track = synced_audio;
                                        }
                                    }
                                    if s.subtitle_tracks.is_empty() && !p.subtitles.is_empty() {
                                        s.subtitle_tracks = p.subtitles.clone();
                                        if (probe_native || s.preloaded) && s.subtitle_track.is_none() {
                                            s.subtitle_track = p.subtitle_track;
                                        }
                                    }
                                });
                                if ui.peek().duration.is_some() {
                                    let s = ui.peek().clone();
                                    if sync_connected() && played() && !awaiting_ready() {
                                        sync::send(&SyncClientMsg::Report {
                                            position_sec: s.position,
                                            duration_sec: s.duration,
                                            paused: desired_paused(),
                                            rate: rate(),
                                        });
                                    }
                                    if s.position >= 1.0 {
                                        save_after_probe(false);
                                    }
                                    break;
                                }
                            }
                        });
                    }
                }
                Err(e) => {
                    // Do not keep another peer blocked if this device failed to load the stream.
                    sync::send(&SyncClientMsg::Ready {
                        position_sec: 0.0,
                        duration_sec: None,
                        file_index: None,
                    });
                    let next_avail = if e.code == "no_file" && !e.available_episodes.is_empty() {
                        e.available_episodes
                            .iter()
                            .copied()
                            .find(|&(s, ep)| s == season && ep > episode)
                            .or_else(|| {
                                e.available_episodes
                                    .iter()
                                    .copied()
                                    .find(|&(s, ep)| (s, ep) > (season, episode))
                            })
                    } else {
                        None
                    };
                    ui.with_mut(|s| {
                        s.status = None;
                        s.progress = None;
                        s.next_available = next_avail;
                        s.error = Some(e.message);
                    });
                }
            }
        }
    };

    // Fetches `/api/play` for `(season, episode, rel)` (or uses a warm background prefetch) and loads the stream.
    let start_play_fetch = {
        let token = token.clone();
        let mut apply_play_result = apply_play_result.clone();
        move |season: i32, episode: i32, rel: ReleaseChoiceUpdate, initial_pos: f64, my_gen: u32| {
            let t = token.clone();
            let now_ms = platform::now_ms();
            enum PrefetchHit {
                Ready(PlayInfo),
                InFlight(u32),
                Miss,
            }
            let hit = match next_prefetch
                .peek()
                .as_ref()
                .filter(|p| p.matches_play(season, episode, &rel.infohash))
            {
                Some(p) => match &p.status {
                    NextPrefetchStatus::Ready { info, prepared_at_ms }
                        if prefetch_is_fresh(info, *prepared_at_ms, now_ms, PREFETCH_TORR_TTL_MS) =>
                    {
                        PrefetchHit::Ready(info.clone())
                    }
                    NextPrefetchStatus::InFlight => PrefetchHit::InFlight(p.gen),
                    _ => PrefetchHit::Miss,
                },
                None => PrefetchHit::Miss,
            };

            if let PrefetchHit::Ready(prefetched) = hit {
                invalidate_prefetch();
                stall_secs.set(0);
                auto_reloads.set(0);
                last_advance.set((0.0, now_ms));
                connecting.set(false);
                played.set(false);
                awaiting_ready.set(true);
                canplay_seen.set(false);
                pending_seek.set(None);
                seeking_target.set(None);
                info.set(None);
                force_transcode.set(false);
                ui.with_mut(|s| {
                    s.subtitles_src = None;
                    s.error = None;
                    s.next_available = None;
                    s.menu = None;
                    s.position = initial_pos;
                    s.duration = None;
                    s.waiting = false;
                    s.release = release_label(&rel);
                    s.active_infohash = Some(rel.infohash.clone());
                    s.subtitle = (kind == MediaKind::Tv).then(|| episode_code(season, episode));
                    s.status = None;
                    s.progress = None;
                });
                apply_play_result(season, episode, my_gen, t, Ok(prefetched));
                return;
            }

            let wait_pf_gen = match hit {
                PrefetchHit::InFlight(g) => Some(g),
                _ => {
                    invalidate_prefetch();
                    None
                }
            };

            spawn(async move {
                platform::sleep(95_000).await;
                if play_gen() != my_gen || closing() {
                    return;
                }
                let still_connecting = connecting() && ui.peek().error.is_none();
                if still_connecting {
                    connecting.set(false);
                    ui.with_mut(|s| {
                        s.status = None;
                        s.waiting = false;
                        s.progress = None;
                        s.error =
                            Some("Сервер не ответил вовремя. Попробуйте другую раздачу или «Скачать заранее».".into());
                    });
                }
            });

            stall_secs.set(0);
            auto_reloads.set(0);
            last_advance.set((0.0, now_ms));
            connecting.set(true);
            played.set(false);
            awaiting_ready.set(true);
            canplay_seen.set(false);
            pending_seek.set(None);
            seeking_target.set(None);
            info.set(None);
            force_transcode.set(false);
            ui.with_mut(|s| {
                s.src.clear();
                s.subtitles_src = None;
                s.error = None;
                s.next_available = None;
                s.menu = None;
                s.position = initial_pos;
                s.duration = None;
                s.waiting = false;
                s.release = release_label(&rel);
                s.active_infohash = Some(rel.infohash.clone());
                s.subtitle = (kind == MediaKind::Tv).then(|| episode_code(season, episode));
                s.status = Some("Подключаемся к раздаче…".into());
                s.progress = None;
            });
            stop_video_element();

            let mut apply_async = apply_play_result.clone();
            spawn(async move {
                if let Some(pf_gen) = wait_pf_gen {
                    loop {
                        if play_gen() != my_gen || closing() {
                            return;
                        }
                        let state = next_prefetch
                            .peek()
                            .as_ref()
                            .filter(|p| p.gen == pf_gen)
                            .map(|p| p.status.clone());
                        match state {
                            Some(NextPrefetchStatus::InFlight) => {
                                platform::sleep(100).await;
                            }
                            Some(NextPrefetchStatus::Ready { info, prepared_at_ms })
                                if prefetch_is_fresh(
                                    &info,
                                    prepared_at_ms,
                                    platform::now_ms(),
                                    PREFETCH_TORR_TTL_MS,
                                ) =>
                            {
                                invalidate_prefetch();
                                apply_async(season, episode, my_gen, t, Ok(info));
                                return;
                            }
                            _ => {
                                invalidate_prefetch();
                                break;
                            }
                        }
                    }
                }
                let req = PlayRequest {
                    kind,
                    tmdb_id: id,
                    season: if kind == MediaKind::Tv { season } else { 0 },
                    episode: if kind == MediaKind::Tv { episode } else { 0 },
                    release: rel.infohash.clone(),
                    native: platform::video::supports_native(),
                };
                let res = api::play(&t, &req).await;
                if play_gen() != my_gen || closing() {
                    return;
                }
                apply_async(season, episode, my_gen, t, res);
            });
        }
    };

    // Asks the server for the stream of the current target (optionally joining sync first).
    let begin_playback = {
        let token = token.clone();
        let client_id = client_id.clone();
        let device = device.clone();
        let explicit_ep_initial = start.explicit_episode;
        let explicit_rel_initial = start.explicit_release;
        let mut start_play_fetch = start_play_fetch.clone();
        move |join_sync: bool| {
            let (season, episode) = *target.peek();
            let rel = release.peek().clone();
            let my_gen = play_gen() + 1;
            play_gen.set(my_gen);

            if !join_sync || sync_init_done() {
                start_play_fetch(season, episode, rel, 0.0, my_gen);
                return;
            }

            stall_secs.set(0);
            auto_reloads.set(0);
            last_advance.set((0.0, platform::now_ms()));
            connecting.set(true);
            played.set(false);
            awaiting_ready.set(true);
            canplay_seen.set(false);
            pending_seek.set(None);
            seeking_target.set(None);
            info.set(None);
            force_transcode.set(false);
            ui.with_mut(|s| {
                s.src.clear();
                s.subtitles_src = None;
                s.error = None;
                s.next_available = None;
                s.menu = None;
                s.position = 0.0;
                s.duration = None;
                s.waiting = false;
                s.release = release_label(&rel);
                s.subtitle = (kind == MediaKind::Tv).then(|| episode_code(season, episode));
                s.status = Some("Подключаемся к раздаче…".into());
                s.progress = None;
            });
            stop_video_element();

            let t = token.clone();
            let cid = client_id.clone();
            let dev = device.clone();
            let mut start_play_fetch = start_play_fetch.clone();
            spawn(async move {
                let mut cur_season = season;
                let mut cur_episode = episode;
                let mut cur_rel = rel;
                let mut initial_pos = 0.0;

                let join_req = SyncJoinRequest {
                    client_id: cid.clone(),
                    device: dev.clone(),
                    kind,
                    tmdb_id: id,
                    season: if kind == MediaKind::Tv { cur_season } else { 0 },
                    episode: if kind == MediaKind::Tv { cur_episode } else { 0 },
                    release: cur_rel.clone(),
                    explicit_episode: explicit_ep_initial,
                    explicit_release: explicit_rel_initial,
                    reconnect_pos: None,
                };
                if let Ok(resp) = api::sync_join(&t, &join_req).await {
                    if play_gen() != my_gen || closing() {
                        return;
                    }
                    sync_init_done.set(true);
                    clock.with_mut(|c| c.seed_from_server(resp.state.server_now_ms, platform::now_ms()));
                    last_revision.set(resp.state.revision);
                    sync_state.set(Some(resp.state.clone()));

                    if kind == MediaKind::Tv {
                        cur_season = resp.state.season;
                        cur_episode = resp.state.episode;
                        target.set((cur_season, cur_episode));
                    }
                    cur_rel = resp.state.release.clone();
                    release.set(cur_rel.clone());
                    rate.set(resp.state.rate);
                    initial_pos = resp.state.position_sec;
                    let hold = resp.state.paused || resp.state.waiting;
                    desired_paused.set(hold);
                    ui.with_mut(|s| {
                        s.paused = resp.state.paused;
                        s.autoplay = !hold;
                        s.release = release_label(&cur_rel);
                        s.subtitle = (kind == MediaKind::Tv).then(|| episode_code(cur_season, cur_episode));
                        s.sync_members = resp.state.members.clone();
                        s.sync_waiting_for = resp.state.waiting_for.clone();
                        s.sync_sound_client_id = resp.state.sound_client_id.clone();
                    });

                    // Start WebSocket bridge loop with automatic reconnect while Player is open.
                    let my_ws = ws_gen() + 1;
                    ws_gen.set(my_ws);
                    let ws_token = t.clone();
                    let ws_cid = cid.clone();
                    let ws_dev = dev.clone();
                    let initial_ticket = resp.ticket;
                    let mut ws_start_fetch = start_play_fetch.clone();
                    spawn(async move {
                        let mut ticket = initial_ticket;
                        let mut backoff_ms: u32 = 1_500;
                        loop {
                            if ws_gen() != my_ws || closing() {
                                break;
                            }
                            let mut bridge = sync::start_bridge(&ticket, &ws_token, &ws_cid, kind, id);
                            while let Ok(raw) = bridge.recv::<String>().await {
                                if ws_gen() != my_ws || closing() {
                                    break;
                                }
                                let Ok(ev) = serde_json::from_str::<sync::WsBridgeEvent>(&raw) else {
                                    continue;
                                };
                                match ev {
                                    sync::WsBridgeEvent::Open => {
                                        backoff_ms = 1_500;
                                        sync_connected.set(true);
                                        sync::send(&SyncClientMsg::Ping {
                                            client_ms: platform::now_ms(),
                                        });
                                        if !awaiting_ready() && info.peek().is_some() {
                                            let s = ui.peek();
                                            let fi = info.peek().as_ref().map(|i| i.file_index);
                                            sync::send(&SyncClientMsg::Ready {
                                                position_sec: s.position,
                                                duration_sec: s.duration,
                                                file_index: fi,
                                            });
                                        }
                                    }
                                    sync::WsBridgeEvent::Msg { data } => {
                                        let Some(msg) = sync::parse_server_msg(&data) else {
                                            continue;
                                        };
                                        match msg {
                                            SyncServerMsg::Pong { client_ms, server_ms } => {
                                                clock.with_mut(|c| {
                                                    c.on_pong(client_ms, server_ms, platform::now_ms());
                                                });
                                            }
                                            SyncServerMsg::State(st) => {
                                                let st = *st;
                                                if st.revision < last_revision() {
                                                    continue;
                                                }
                                                let is_self = st.updated_by.as_deref() == Some(ws_cid.as_str());
                                                let now_ms = platform::now_ms();
                                                let apply_sound = st.event == SyncEventKind::Sound
                                                    || now_ms - last_sound_cmd_ms() > 1_500
                                                    || st.sound_client_id == ui.peek().sync_sound_client_id;
                                                let was_sync_muted = ui.peek().is_sync_muted();
                                                ui.with_mut(|s| {
                                                    s.sync_members = st.members.clone();
                                                    s.sync_waiting_for = st.waiting_for.clone();
                                                    if apply_sound {
                                                        s.sync_sound_client_id = st.sound_client_id.clone();
                                                    }
                                                    if st.members.len() <= 1 && s.menu == Some(PlayerMenu::Sync) {
                                                        s.menu = None;
                                                    }
                                                });
                                                let now_sync_muted = ui.peek().is_sync_muted();
                                                if apply_sound {
                                                    if now_sync_muted {
                                                        if let Some(v) = video() {
                                                            v.set_muted(true);
                                                        }
                                                    } else if was_sync_muted {
                                                        if st.event == SyncEventKind::Sound
                                                            && st.sound_client_id.as_deref() == Some(ws_cid.as_str())
                                                        {
                                                            muted.set(false);
                                                            auto_muted.set(false);
                                                            if volume() <= 0.001 {
                                                                volume.set(0.7);
                                                                if let Some(v) = video() {
                                                                    v.set_volume(0.7);
                                                                }
                                                            }
                                                            ui.with_mut(|s| s.auto_muted = false);
                                                        }
                                                        if !is_self
                                                            && !ui.peek().native_video
                                                            && !muted()
                                                            && !auto_muted()
                                                        {
                                                            last_sync_unmute_ms.set(now_ms);
                                                        }
                                                        if let Some(v) = video() {
                                                            v.set_muted(muted() || auto_muted());
                                                        }
                                                    }
                                                }
                                                last_revision.set(st.revision);
                                                sync_state.set(Some(st.clone()));

                                                let hold = st.paused || st.waiting;
                                                let was_paused = ui.peek().paused;
                                                desired_paused.set(hold);
                                                ui.with_mut(|s| {
                                                    s.paused = st.paused;
                                                    if st.waiting {
                                                        s.waiting = true;
                                                        if !connecting() && s.progress.is_none() {
                                                            s.status = Some(waiting_label(&st));
                                                        }
                                                    } else if !awaiting_ready() && !connecting() {
                                                        s.waiting = false;
                                                        s.status = None;
                                                    }
                                                });
                                                if (!is_self && st.paused != was_paused)
                                                    || (was_paused && !st.paused && ui.peek().controls)
                                                {
                                                    show_controls();
                                                }
                                                if hold && paused_since_ms().is_none() {
                                                    paused_since_ms.set(Some(now_ms));
                                                }

                                                if let Some(v) = video() {
                                                    if hold {
                                                        pause_video();
                                                    } else if !awaiting_ready() && info.peek().is_some() && v.paused() {
                                                        let paused_ms = paused_since_ms().map_or(0, |t| now_ms - t);
                                                        paused_since_ms.set(None);
                                                        if ui.peek().transcoded && (v.ended() || paused_ms >= 45_000) {
                                                            let expected = clock.peek().expected_position(&st, now_ms);
                                                            open_source(expected, true);
                                                        } else {
                                                            attempt_play(play_gen(), false);
                                                        }
                                                    } else if !hold {
                                                        paused_since_ms.set(None);
                                                    }
                                                } else if !hold {
                                                    paused_since_ms.set(None);
                                                }

                                                if is_self {
                                                    continue;
                                                }

                                                // Remote episode or release switch: reload stream without tearing down WS.
                                                let diff_ep =
                                                    kind == MediaKind::Tv && (st.season, st.episode) != *target.peek();
                                                let diff_rel = st.release.infohash != release.peek().infohash;
                                                if diff_ep || diff_rel {
                                                    target.set((st.season, st.episode));
                                                    release.set(st.release.clone());
                                                    rate.set(st.rate);
                                                    let n_gen = play_gen() + 1;
                                                    play_gen.set(n_gen);
                                                    ws_start_fetch(
                                                        st.season,
                                                        st.episode,
                                                        st.release.clone(),
                                                        st.position_sec,
                                                        n_gen,
                                                    );
                                                    continue;
                                                }

                                                // Remote audio track switch.
                                                if st.event == SyncEventKind::Audio {
                                                    if let Some(i) = info.peek().clone() {
                                                        let target_idx = st
                                                            .audio_track
                                                            .as_deref()
                                                            .and_then(|lbl| i.audio.iter().find(|a| a.label() == lbl))
                                                            .map(|a| a.index);
                                                        if target_idx.is_some() && target_idx != ui.peek().audio_track {
                                                            let at =
                                                                clock.peek().expected_position(&st, platform::now_ms());
                                                            invalidate_prefetch();
                                                            ui.with_mut(|s| s.audio_track = target_idx);
                                                            load_source(at);
                                                            continue;
                                                        }
                                                        if !awaiting_ready() {
                                                            sync::send(&SyncClientMsg::Ready {
                                                                position_sec: ui.peek().position,
                                                                duration_sec: ui.peek().duration,
                                                                file_index: Some(i.file_index),
                                                            });
                                                        }
                                                    }
                                                }

                                                // Remote playback rate switch.
                                                if (rate() - st.rate).abs() > 0.001 {
                                                    rate.set(st.rate);
                                                    if let Some(v) = video() {
                                                        v.set_rate(f64::from(st.rate));
                                                    }
                                                }

                                                // Remote position alignment (Seek / Resumed / Pause).
                                                if info.peek().is_some() && !connecting() {
                                                    let target_pos =
                                                        clock.peek().expected_position(&st, platform::now_ms());
                                                    let local_pos = ui.peek().position;
                                                    let diff = (local_pos - target_pos).abs();
                                                    let is_seek = st.event == SyncEventKind::Seek;
                                                    let is_align = matches!(
                                                        st.event,
                                                        SyncEventKind::Seek
                                                            | SyncEventKind::Resumed
                                                            | SyncEventKind::Play
                                                            | SyncEventKind::Pause
                                                    );
                                                    if is_align {
                                                        if ui.peek().transcoded {
                                                            let min_diff = if is_seek { 1.0 } else { 2.8 };
                                                            if diff > min_diff {
                                                                last_seek_cmd_ms.set(platform::now_ms());
                                                                load_source(target_pos);
                                                            } else if is_seek && !awaiting_ready() {
                                                                let fi = info.peek().as_ref().map(|i| i.file_index);
                                                                sync::send(&SyncClientMsg::Ready {
                                                                    position_sec: local_pos,
                                                                    duration_sec: ui.peek().duration,
                                                                    file_index: fi,
                                                                });
                                                            }
                                                        } else if diff > 0.45 {
                                                            if let Some(v) = video() {
                                                                last_seek_cmd_ms.set(platform::now_ms());
                                                                awaiting_ready.set(true);
                                                                canplay_seen.set(false);
                                                                seeking_target.set(Some(target_pos));
                                                                v.set_current_time(target_pos);
                                                                ui.with_mut(|s| s.position = target_pos);
                                                            }
                                                        } else if is_seek && !awaiting_ready() {
                                                            let fi = info.peek().as_ref().map(|i| i.file_index);
                                                            sync::send(&SyncClientMsg::Ready {
                                                                position_sec: local_pos,
                                                                duration_sec: ui.peek().duration,
                                                                file_index: fi,
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                            SyncServerMsg::Error { .. } => {}
                                        }
                                    }
                                    sync::WsBridgeEvent::Error | sync::WsBridgeEvent::Closed => {
                                        sync_connected.set(false);
                                        break;
                                    }
                                }
                            }

                            // Reconnect cleanly with exponential backoff until a fresh ticket arrives.
                            sync_connected.set(false);
                            let mut reconnected = false;
                            while ws_gen() == my_ws && !closing() {
                                platform::sleep(backoff_ms).await;
                                if ws_gen() != my_ws || closing() {
                                    break;
                                }
                                let (rs, re) = *target.peek();
                                let rrel = release.peek().clone();
                                let rejoin_req = SyncJoinRequest {
                                    client_id: ws_cid.clone(),
                                    device: ws_dev.clone(),
                                    kind,
                                    tmdb_id: id,
                                    season: if kind == MediaKind::Tv { rs } else { 0 },
                                    episode: if kind == MediaKind::Tv { re } else { 0 },
                                    release: rrel,
                                    explicit_episode: false,
                                    explicit_release: false,
                                    reconnect_pos: Some(ui.peek().position),
                                };
                                match api::sync_join(&ws_token, &rejoin_req).await {
                                    Ok(rejoin) => {
                                        clock.with_mut(|c| {
                                            c.seed_from_server(rejoin.state.server_now_ms, platform::now_ms());
                                        });
                                        last_revision.set(rejoin.state.revision);
                                        sync_state.set(Some(rejoin.state.clone()));
                                        let was_sync_muted = ui.peek().is_sync_muted();
                                        ui.with_mut(|s| {
                                            s.sync_members = rejoin.state.members.clone();
                                            s.sync_waiting_for = rejoin.state.waiting_for.clone();
                                            s.sync_sound_client_id = rejoin.state.sound_client_id.clone();
                                        });
                                        let now_sync_muted = ui.peek().is_sync_muted();
                                        if now_sync_muted != was_sync_muted {
                                            if let Some(v) = video() {
                                                v.set_muted(now_sync_muted || muted() || auto_muted());
                                            }
                                        }
                                        let diff_ep = kind == MediaKind::Tv
                                            && (rejoin.state.season, rejoin.state.episode) != *target.peek();
                                        let diff_rel = rejoin.state.release.infohash != release.peek().infohash;
                                        if diff_ep || diff_rel {
                                            target.set((rejoin.state.season, rejoin.state.episode));
                                            release.set(rejoin.state.release.clone());
                                            rate.set(rejoin.state.rate);
                                            let n_gen = play_gen() + 1;
                                            play_gen.set(n_gen);
                                            ws_start_fetch(
                                                rejoin.state.season,
                                                rejoin.state.episode,
                                                rejoin.state.release.clone(),
                                                rejoin.state.position_sec,
                                                n_gen,
                                            );
                                        } else {
                                            let hold = rejoin.state.paused || rejoin.state.waiting;
                                            desired_paused.set(hold);
                                            ui.with_mut(|s| s.paused = rejoin.state.paused);
                                            if hold {
                                                pause_video();
                                            }
                                        }
                                        ticket = rejoin.ticket;
                                        reconnected = true;
                                        break;
                                    }
                                    Err(_) => {
                                        backoff_ms = (backoff_ms.saturating_mul(2)).min(15_000);
                                    }
                                }
                            }
                            if !reconnected {
                                break;
                            }
                        }
                    });
                }

                start_play_fetch(cur_season, cur_episode, cur_rel, initial_pos, my_gen);
            });
        }
    };
    use_hook({
        let mut begin_playback = begin_playback.clone();
        move || begin_playback(true)
    });

    // While connecting or buffering, shows how far the torrent has got instead of a bare spinner;
    // also detects silent `<video>` stalls (e.g. a closed chunked transcode stream) and auto-reloads from the current second.
    {
        let token = token.clone();
        use_future(move || {
            let token = token.clone();
            async move {
                loop {
                    platform::sleep(1_000).await;
                    if closing() {
                        break;
                    }
                    let (busy, preloaded, is_transcoded, is_native, is_paused, has_err, has_info) = {
                        let s = ui.peek();
                        let waiting_for_peer = sync_state
                            .peek()
                            .as_ref()
                            .is_some_and(|st| st.waiting && !awaiting_ready() && !connecting());
                        (
                            s.error.is_none() && (connecting() || (s.waiting && !waiting_for_peer)),
                            s.preloaded,
                            s.transcoded,
                            s.native_video,
                            s.paused || desired_paused() || waiting_for_peer,
                            s.error.is_some(),
                            info.peek().is_some(),
                        )
                    };
                    if has_info
                        && !has_err
                        && !connecting()
                        && !is_paused
                        && !is_native
                        && seeking_target.peek().is_none()
                        && pending_seek.peek().is_none()
                    {
                        if let Some(v) = video() {
                            let cur_t = offset() + v.current_time();
                            let now_ms = platform::now_ms();
                            let (prev_t, prev_ms) = last_advance();
                            let dur = ui.peek().duration;
                            let premature_eof = v.ended() && dur.is_some_and(|d| d > 30.0 && cur_t + 12.0 < d);
                            if !premature_eof && (cur_t - prev_t).abs() > 0.15 {
                                last_advance.set((cur_t, now_ms));
                                if auto_reloads() > 0 && now_ms - last_reload_ms() > 3_500 {
                                    auto_reloads.set(0);
                                }
                            } else {
                                let stuck_ms = now_ms - prev_ms;
                                let silent_freeze = !awaiting_ready()
                                    && !ui.peek().waiting
                                    && ((is_transcoded && stuck_ms >= 4_500)
                                        || (!is_transcoded && preloaded && stuck_ms >= 6_000));
                                let waiting_stuck = (awaiting_ready() || ui.peek().waiting)
                                    && (is_transcoded || preloaded)
                                    && stuck_ms >= 15_000;
                                if premature_eof || silent_freeze || waiting_stuck {
                                    if auto_reloads() < 2 {
                                        auto_reloads.set(auto_reloads() + 1);
                                        last_advance.set((cur_t, now_ms));
                                        open_source(cur_t, true);
                                        continue;
                                    } else {
                                        ui.with_mut(|s| {
                                            s.src.clear();
                                            s.subtitles_src = None;
                                            s.status = None;
                                            s.waiting = false;
                                            s.progress = None;
                                            s.error = Some(
                                                "Поток завис или оборвался. Попробуйте перезапустить или выбрать другую раздачу."
                                                    .into(),
                                            );
                                        });
                                        stop_video_element();
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    if !busy || preloaded {
                        stall_secs.set(0);
                        if ui.peek().progress.is_some() {
                            ui.with_mut(|s| s.progress = None);
                        }
                        continue;
                    }
                    let key = release.peek().infohash.clone();
                    if let Ok(p) = api::play_progress(&token, &key).await {
                        // The answer may arrive after playback began.
                        let (is_conn, waiting, has_err) = {
                            let s = ui.peek();
                            (connecting(), s.waiting, s.error.is_some())
                        };
                        if !has_err && (is_conn || waiting) {
                            // Fail cleanly instead of spinning forever when no data is arriving after `/api/play`.
                            if !is_conn && waiting && p.loaded_bytes == 0 && p.speed_bps == 0 {
                                let secs = stall_secs() + 1;
                                stall_secs.set(secs);
                                let limit = if p.phase == StreamPhase::Connecting && p.peers == 0 {
                                    45
                                } else {
                                    60
                                };
                                if secs >= limit {
                                    sync::send(&SyncClientMsg::Ready {
                                        position_sec: ui.peek().position,
                                        duration_sec: ui.peek().duration,
                                        file_index: info.peek().as_ref().map(|i| i.file_index),
                                    });
                                    ui.with_mut(|s| {
                                        s.src.clear();
                                        s.status = None;
                                        s.waiting = false;
                                        s.progress = None;
                                        s.error = Some("Нет раздающих или поток не отвечает. Попробуйте другую раздачу или «Скачать заранее».".into());
                                    });
                                    stop_video_element();
                                    continue;
                                }
                            } else {
                                stall_secs.set(0);
                            }
                            let shown = (is_conn || p.phase != StreamPhase::Ready).then_some(p);
                            ui.with_mut(|s| s.progress = shown);
                        }
                    }
                }
            }
        });
    }

    // Periodic sync heartbeat + smooth drift correction across devices.
    use_future(move || async move {
        loop {
            platform::sleep(SYNC_TICK_MS).await;
            if closing() {
                break;
            }
            if !sync_connected() {
                continue;
            }
            let now_ms = platform::now_ms();
            sync::send(&SyncClientMsg::Ping { client_ms: now_ms });
            if info.peek().is_none()
                || connecting()
                || awaiting_ready()
                || seeking_target.peek().is_some()
                || pending_seek.peek().is_some()
                || !played()
                || ui.peek().waiting
            {
                continue;
            }
            let Some(v) = video() else { continue };
            let local_pos = offset() + v.current_time();
            let dur = ui.peek().duration;
            sync::send(&SyncClientMsg::Report {
                position_sec: local_pos,
                duration_sec: dur,
                paused: desired_paused(),
                rate: rate(),
            });

            let Some(st) = sync_state.peek().clone() else {
                continue;
            };
            if st.members.len() <= 1 || st.paused || st.waiting || !played() || awaiting_ready() || ui.peek().waiting {
                continue;
            }
            if now_ms - last_seek_cmd_ms() < 4_000 || now_ms - last_reload_ms() < 8_000 {
                continue;
            }

            let expected = clock.peek().expected_position(&st, now_ms);
            let drift = local_pos - expected;
            let abs_drift = drift.abs();
            let target_rate = f64::from(st.rate);

            if !ui.peek().transcoded {
                if abs_drift > 1.5 {
                    last_seek_cmd_ms.set(now_ms);
                    v.set_rate(target_rate);
                    v.set_current_time(expected);
                    ui.with_mut(|s| s.position = expected);
                } else if abs_drift > 0.35 {
                    let nudge = if drift > 0.0 { 0.95 } else { 1.05 };
                    v.set_rate((target_rate * nudge).clamp(0.25, 4.0));
                } else {
                    v.set_rate(target_rate);
                }
            } else if abs_drift > 3.5 {
                last_seek_cmd_ms.set(now_ms);
                v.set_rate(target_rate);
                load_source(expected);
            } else if abs_drift > 0.6 {
                let nudge = if drift > 0.0 { 0.94 } else { 1.06 };
                v.set_rate((target_rate * nudge).clamp(0.25, 4.0));
            } else {
                v.set_rate(target_rate);
            }
        }
    });

    let maybe_prefetch_next = {
        let prefetch_token = token.clone();
        let ep_ratings_pf = ep_ratings.clone();
        move |pos: f64, dur: f64, buffered_ahead: Option<f64>| {
            if closing() || kind != MediaKind::Tv || connecting() || awaiting_ready() || !played() || desired_paused() {
                return;
            }
            {
                let s = ui.peek();
                if s.paused || s.waiting || s.error.is_some() {
                    return;
                }
            }
            if !can_prefetch_next(pos, dur, buffered_ahead) {
                return;
            }
            let (season, cur_ep) = *target.peek();
            let server_next = info.peek().as_ref().and_then(|i| i.next);
            let Some((ns, ne)) = next_target_episode(season, cur_ep, server_next, rewatch_mode, &ep_ratings_pf) else {
                return;
            };
            let cur_rel = release.peek().clone();
            let now_ms = platform::now_ms();
            if !needs_prefetch_refresh(
                next_prefetch.peek().as_ref(),
                season,
                cur_ep,
                &cur_rel.infohash,
                ns,
                ne,
                now_ms,
            ) {
                return;
            }

            let need_switch = ns != season || cur_rel.episode > 0;
            let pf_gen = *prefetch_gen.peek() + 1;
            prefetch_gen.set(pf_gen);
            let initial_choice = (!need_switch).then(|| cur_rel.clone());
            next_prefetch.set(Some(NextPrefetch {
                gen: pf_gen,
                from_season: season,
                from_episode: cur_ep,
                from_infohash: cur_rel.infohash,
                next_season: ns,
                next_episode: ne,
                resolved_choice: initial_choice.clone(),
                status: NextPrefetchStatus::InFlight,
            }));

            let tok = prefetch_token.clone();
            let is_mobile = telegram::is_mobile();
            let native = platform::video::supports_native();
            spawn(async move {
                let target_choice = match initial_choice {
                    Some(c) => c,
                    None => {
                        let req = ResolveReleaseRequest {
                            season: ns,
                            episode: ne,
                            is_mobile,
                        };
                        let Ok(res) = api::resolve_release(&tok, kind, id, &req).await else {
                            if !closing() && *prefetch_gen.peek() == pf_gen {
                                next_prefetch.with_mut(|p| {
                                    if let Some(p) = p.as_mut().filter(|p| p.gen == pf_gen) {
                                        p.status = NextPrefetchStatus::Failed;
                                    }
                                });
                            }
                            return;
                        };
                        if closing() || *prefetch_gen.peek() != pf_gen {
                            return;
                        }
                        match resolve_response_to_choice(res) {
                            Some(c) => {
                                next_prefetch.with_mut(|p| {
                                    if let Some(p) = p.as_mut().filter(|p| p.gen == pf_gen) {
                                        p.resolved_choice = Some(c.clone());
                                    }
                                });
                                c
                            }
                            None => {
                                next_prefetch.with_mut(|p| {
                                    if let Some(p) = p.as_mut().filter(|p| p.gen == pf_gen) {
                                        p.status = NextPrefetchStatus::NeedsPicker;
                                    }
                                });
                                return;
                            }
                        }
                    }
                };
                let req = PlayRequest {
                    kind,
                    tmdb_id: id,
                    season: ns,
                    episode: ne,
                    release: target_choice.infohash.clone(),
                    native,
                };
                let res = api::play(&tok, &req).await;
                if closing() || *prefetch_gen.peek() != pf_gen {
                    return;
                }
                next_prefetch.with_mut(|p| {
                    if let Some(p) = p.as_mut().filter(|p| p.gen == pf_gen) {
                        p.status = match res {
                            Ok(play_info) => NextPrefetchStatus::Ready {
                                info: play_info,
                                prepared_at_ms: platform::now_ms(),
                            },
                            Err(_) => NextPrefetchStatus::Failed,
                        };
                    }
                });
            });
        }
    };

    let mut go_next = {
        let mut save = save.clone();
        let mut begin_playback = begin_playback.clone();
        let start_fetch_next = start_play_fetch.clone();
        let tok_next = token.clone();
        let ep_ratings_next = ep_ratings.clone();
        move || {
            let (season, cur_ep) = *target.peek();
            let server_next = info.peek().as_ref().and_then(|i| i.next);
            let Some((ns, ne)) = next_target_episode(season, cur_ep, server_next, rewatch_mode, &ep_ratings_next)
            else {
                return;
            };
            save(true);
            let was_paused = ui.peek().paused;
            desired_paused.set(false);
            paused_since_ms.set(None);
            ui.with_mut(|s| s.paused = false);
            if was_paused && ui.peek().controls {
                show_controls();
            }
            let cur_rel = release.peek().clone();
            let need_switch = ns != season || cur_rel.episode > 0;
            if !need_switch {
                target.set((ns, ne));
                sync::send(&SyncClientMsg::Episode {
                    season: ns,
                    episode: ne,
                    release: None,
                });
                begin_playback(false);
                return;
            }

            let pf_hit = next_prefetch
                .peek()
                .as_ref()
                .filter(|p| p.matches_source(season, cur_ep, &cur_rel.infohash, ns, ne))
                .cloned();
            if let Some(pf) = &pf_hit {
                if let Some(choice) = pf.resolved_choice.clone() {
                    choices.with_mut(|v| upsert_choice(v, choice.clone()));
                    release.set(choice.clone());
                    target.set((ns, ne));
                    sync::send(&SyncClientMsg::Episode {
                        season: ns,
                        episode: ne,
                        release: Some(choice),
                    });
                    begin_playback(false);
                    return;
                }
                if pf.status == NextPrefetchStatus::NeedsPicker {
                    invalidate_prefetch();
                    on_need_release.call((ns, ne));
                    return;
                }
            }

            let wait_pf_gen = pf_hit
                .filter(|p| p.status == NextPrefetchStatus::InFlight)
                .map(|p| p.gen);
            let my_gen = play_gen() + 1;
            play_gen.set(my_gen);
            connecting.set(true);
            ui.with_mut(|s| {
                s.src.clear();
                s.subtitles_src = None;
                s.error = None;
                s.next_available = None;
                s.menu = None;
                s.waiting = false;
                s.progress = None;
                s.status = Some("Подключаемся к раздаче…".into());
            });
            stop_video_element();

            let tok = tok_next.clone();
            let req = ResolveReleaseRequest {
                season: ns,
                episode: ne,
                is_mobile: telegram::is_mobile(),
            };
            let mut start_async = start_fetch_next.clone();
            spawn(async move {
                let mut pre_resolved: Option<Option<ReleaseChoiceUpdate>> = None;
                if let Some(pf_gen) = wait_pf_gen {
                    loop {
                        if play_gen() != my_gen || closing() {
                            return;
                        }
                        let snap = next_prefetch.peek().as_ref().filter(|p| p.gen == pf_gen).cloned();
                        match snap {
                            Some(p) if p.resolved_choice.is_some() => {
                                pre_resolved = Some(p.resolved_choice);
                                break;
                            }
                            Some(p) if p.status == NextPrefetchStatus::NeedsPicker => {
                                pre_resolved = Some(None);
                                break;
                            }
                            Some(p) if p.status == NextPrefetchStatus::InFlight => {
                                platform::sleep(100).await;
                            }
                            _ => break,
                        }
                    }
                }
                let resolved = match pre_resolved {
                    Some(v) => v,
                    None => api::resolve_release(&tok, kind, id, &req)
                        .await
                        .ok()
                        .and_then(resolve_response_to_choice),
                };
                if play_gen() != my_gen || closing() {
                    return;
                }
                if let Some(choice) = resolved {
                    choices.with_mut(|v| upsert_choice(v, choice.clone()));
                    release.set(choice.clone());
                    target.set((ns, ne));
                    sync::send(&SyncClientMsg::Episode {
                        season: ns,
                        episode: ne,
                        release: Some(choice.clone()),
                    });
                    start_async(ns, ne, choice, 0.0, my_gen);
                } else {
                    connecting.set(false);
                    ui.with_mut(|s| s.status = None);
                    on_need_release.call((ns, ne));
                }
            });
        }
    };

    let tracks_token = token.clone();
    let save_tracks = move || {
        let s = ui.peek().clone();
        let label = |list: &[kino_core::MediaTrack], i: Option<i32>| {
            i.and_then(|i| list.iter().find(|t| t.index == i)).map(|t| t.label())
        };
        let t = TracksUpdate {
            audio_track: label(&s.audio, s.audio_track),
            subtitle_track: Some(label(&s.subtitle_tracks, s.subtitle_track).unwrap_or_else(|| "off".into())),
        };
        let token = tracks_token.clone();
        spawn(async move {
            let _ = api::set_tracks(&token, kind, id, &t).await;
        });
    };

    let mut toggle_play = {
        let mut save = save.clone();
        move || {
            let Some(v) = video() else { return };
            let cur_pos = ui.peek().position;
            let (cur_s, cur_e) = *target.peek();
            let st_opt = sync_state
                .peek()
                .as_ref()
                .filter(|st| kind != MediaKind::Tv || (st.season == cur_s && st.episode == cur_e))
                .cloned();
            let waiting_peer = st_opt.as_ref().is_some_and(|st| st.waiting);
            let is_paused_now = ui.peek().paused || (!waiting_peer && (desired_paused() || v.paused()));
            if is_paused_now {
                let now_ms = platform::now_ms();
                let paused_ms = paused_since_ms().map_or(0, |t| now_ms - t);
                desired_paused.set(false);
                paused_since_ms.set(None);
                last_advance.set((cur_pos, now_ms));
                auto_reloads.set(0);
                sync_state.with_mut(|st| {
                    if let Some(s) = st.as_mut() {
                        s.paused = false;
                    }
                });
                if st_opt.as_ref().is_none_or(|st| st.paused || st.waiting) {
                    sync::send(&SyncClientMsg::Play { position_sec: cur_pos });
                }
                if !waiting_peer {
                    let sync_expected = st_opt
                        .as_ref()
                        .filter(|st| !st.paused && !st.waiting)
                        .map(|st| clock.peek().expected_position(st, now_ms));
                    let need_transcode_reload = ui.peek().transcoded
                        && (v.ended()
                            || paused_ms >= 45_000
                            || sync_expected.is_some_and(|exp| (cur_pos - exp).abs() > 2.0));
                    if need_transcode_reload {
                        ui.with_mut(|s| s.paused = false);
                        open_source(sync_expected.unwrap_or(cur_pos), true);
                    } else {
                        if let Some(expected) = sync_expected {
                            if (cur_pos - expected).abs() > 1.5 && !ui.peek().transcoded {
                                v.set_current_time(expected);
                                ui.with_mut(|s| s.position = expected);
                            }
                        }
                        ui.with_mut(|s| s.paused = false);
                        attempt_play(play_gen(), true);
                    }
                }
                show_controls();
            } else {
                desired_paused.set(true);
                paused_since_ms.set(Some(platform::now_ms()));
                pause_video();
                ui.with_mut(|s| s.paused = true);
                show_controls();
                sync::send(&SyncClientMsg::Pause { position_sec: cur_pos });
                save(false);
            }
        }
    };

    let mut seek_to = move |at: f64| {
        if ui.peek().controls {
            show_controls();
        }
        let s = ui.peek().clone();
        let at = match s.duration {
            Some(d) => at.clamp(0.0, (d - 1.0).max(0.0)),
            None => at.max(0.0),
        };
        auto_reloads.set(0);
        last_advance.set((at, platform::now_ms()));
        last_seek_cmd_ms.set(platform::now_ms());
        sync::send(&SyncClientMsg::Seek { position_sec: at });
        if s.transcoded {
            load_source(at);
        } else if let Some(v) = video() {
            if !s.native_video && v.ready_state() < 1 {
                pending_seek.set(Some(at));
                ui.with_mut(|s| s.position = at);
                sync::set_playback_state(at, ui.peek().duration, played());
            } else if (v.current_time() - at).abs() > 0.25 {
                awaiting_ready.set(true);
                canplay_seen.set(false);
                seeking_target.set(Some(at));
                v.set_current_time(at);
                ui.with_mut(|s| s.position = at);
                sync::set_playback_state(at, ui.peek().duration, played());
            } else {
                seeking_target.set(None);
                ui.with_mut(|s| s.position = at);
                sync::set_playback_state(at, ui.peek().duration, played());
                if canplay_seen() || v.ready_state() >= 2 {
                    mark_media_ready();
                }
            }
        }
    };

    let on_video = {
        let mut save = save.clone();
        let mut go_next = go_next.clone();
        let mut maybe_prefetch_next = maybe_prefetch_next.clone();
        let err_token = token.clone();
        let mut mark_media_ready = mark_media_ready;
        let ep_ratings_ev = ep_ratings.clone();
        move |ev: &'static str| {
            let Some(v) = video() else { return };
            match ev {
                "time" => {
                    if pending_seek.peek().is_some() || seeking_target.peek().is_some() || awaiting_ready() {
                        return;
                    }
                    let pos = offset() + v.current_time();
                    if (pos - last_advance.peek().0).abs() > 0.12 {
                        last_advance.set((pos, platform::now_ms()));
                    }
                    let direct_len = (!ui.peek().transcoded && v.duration().is_finite()).then(|| v.duration());
                    let mut eff_dur = ui.peek().duration;
                    ui.with_mut(|s| {
                        s.position = pos;
                        if s.duration.is_none() {
                            s.duration = direct_len;
                            eff_dur = s.duration;
                        }
                    });
                    sync::set_playback_state(pos, eff_dur, played());
                    if (pos - last_saved()).abs() >= SAVE_EVERY {
                        save(false);
                    }
                    if let Some(dur) = eff_dur {
                        maybe_prefetch_next(pos, dur, v.buffered_ahead());
                    }
                }
                "metadata" => {
                    if !ui.peek().native_video {
                        apply_web_scale(*saved_scale.peek());
                    }
                    if let Some(at) = pending_seek.take() {
                        if (v.current_time() - at).abs() > 0.25 {
                            canplay_seen.set(false);
                            seeking_target.set(Some(at));
                            v.set_current_time(at);
                        } else {
                            seeking_target.set(None);
                        }
                    }
                    // A new source starts at normal speed and full volume.
                    if (rate() - 1.0).abs() > 0.001 {
                        v.set_rate(f64::from(rate()));
                    }
                    v.set_volume(f64::from(volume()));
                    v.set_muted(muted() || auto_muted() || ui.peek().is_sync_muted());
                    if desired_paused() {
                        pause_video();
                    }
                    if pending_seek.peek().is_none()
                        && seeking_target.peek().is_none()
                        && (canplay_seen() || v.ready_state() >= 2)
                    {
                        mark_media_ready();
                    }
                }
                "seeked" => {
                    if let Some(tgt) = seeking_target() {
                        if ui.peek().native_video || (v.current_time() - tgt).abs() < 20.0 {
                            seeking_target.set(None);
                        }
                    }
                    if pending_seek.peek().is_none()
                        && seeking_target.peek().is_none()
                        && (canplay_seen() || v.ready_state() >= 2)
                    {
                        mark_media_ready();
                    }
                }
                "canplay" => {
                    canplay_seen.set(true);
                    if let Some(tgt) = seeking_target() {
                        if (v.current_time() - tgt).abs() < 1.5 {
                            seeking_target.set(None);
                        }
                    }
                    if pending_seek.peek().is_none() && seeking_target.peek().is_none() {
                        mark_media_ready();
                    }
                }
                "stalled" => {
                    if !desired_paused() && !closing() && !awaiting_ready() {
                        ui.with_mut(|s| {
                            s.waiting = true;
                            if s.status.is_none() {
                                s.status = Some("Буферизация видео…".into());
                            }
                        });
                    }
                }
                "waiting" => {
                    let already_played = played();
                    ui.with_mut(|s| {
                        s.waiting = true;
                        if s.status.is_none() {
                            let msg = if already_played {
                                "Буферизация видео…"
                            } else if s.transcoded {
                                "Запускаем перекодирование видео…"
                            } else {
                                "Запускаем видеопоток…"
                            };
                            s.status = Some(msg.into());
                        }
                    });
                    if !awaiting_ready()
                        && already_played
                        && !desired_paused()
                        && sync_state.peek().as_ref().is_some_and(|st| st.members.len() > 1)
                    {
                        let my_wgen = waiting_gen() + 1;
                        waiting_gen.set(my_wgen);
                        spawn(async move {
                            platform::sleep(600).await;
                            if waiting_gen() == my_wgen
                                && ui.peek().waiting
                                && !awaiting_ready()
                                && !desired_paused()
                                && !closing()
                            {
                                sync::send(&SyncClientMsg::Buffering {
                                    position_sec: ui.peek().position,
                                });
                            }
                        });
                    }
                }
                "playing" => {
                    if !ui.peek().native_video {
                        apply_web_scale(*saved_scale.peek());
                    }
                    last_advance.set((offset() + v.current_time(), platform::now_ms()));
                    played.set(true);
                    canplay_seen.set(true);
                    seeking_target.set(None);
                    pending_seek.set(None);
                    mark_media_ready();
                }
                "play" => {
                    if closing() || awaiting_ready() {
                        return;
                    }
                    if desired_paused() {
                        let waiting_peer = sync_state.peek().as_ref().is_some_and(|st| st.waiting);
                        if waiting_peer {
                            pause_video();
                            return;
                        }
                        if sync_state.peek().as_ref().is_some_and(|st| st.paused) {
                            // External play trigger (e.g. MediaSession / headphone button).
                            desired_paused.set(false);
                            sync::send(&SyncClientMsg::Play {
                                position_sec: offset() + v.current_time(),
                            });
                        }
                    }
                    let was_paused = ui.peek().paused;
                    ui.with_mut(|s| s.paused = false);
                    if was_paused {
                        show_controls();
                    }
                }
                "pause" => {
                    if closing() || awaiting_ready() || seeking_target.peek().is_some() || pending_seek.peek().is_some()
                    {
                        return;
                    }
                    let pos = offset() + v.current_time();
                    if let Some(d) = ui.peek().duration {
                        if d > 0.0 && d - pos < 0.8 {
                            return;
                        }
                    }
                    if desired_paused() {
                        let paused_state = sync_state.peek().as_ref().is_none_or(|st| st.paused);
                        ui.with_mut(|s| s.paused = paused_state);
                        return;
                    }
                    if platform::now_ms() - last_sync_unmute_ms() < 800 {
                        // Browser blocked unmuting without a local user gesture: keep playing muted
                        // with the "Включить звук" prompt instead of pausing synchronized peers.
                        auto_muted.set(true);
                        v.set_muted(true);
                        ui.with_mut(|s| s.auto_muted = true);
                        attempt_play(play_gen(), false);
                        return;
                    }
                    if !v.paused() || platform::now_ms() - last_prog_pause_ms() < 600 {
                        return;
                    }
                    // External pause trigger (e.g. MediaSession / headphone disconnect).
                    desired_paused.set(true);
                    paused_since_ms.set(Some(platform::now_ms()));
                    ui.with_mut(|s| s.paused = true);
                    show_controls();
                    sync::send(&SyncClientMsg::Pause { position_sec: pos });
                    save(false);
                }
                "ended" => {
                    let pos = offset() + v.current_time();
                    if ui.peek().transcoded
                        && ui.peek().duration.is_some_and(|d| d > 30.0 && pos + 12.0 < d)
                        && auto_reloads() < 2
                    {
                        auto_reloads.set(auto_reloads() + 1);
                        last_advance.set((pos, platform::now_ms()));
                        open_source(pos, true);
                        return;
                    }
                    save(true);
                    let (cur_s, cur_e) = *target.peek();
                    let s_next = info.peek().as_ref().and_then(|i| i.next);
                    if next_target_episode(cur_s, cur_e, s_next, rewatch_mode, &ep_ratings_ev).is_some() {
                        go_next();
                    } else {
                        desired_paused.set(true);
                        ui.with_mut(|s| s.paused = true);
                        show_controls();
                    }
                }
                "scale" => {
                    if saved_scale.peek().is_none() {
                        active_scale.set(scale_from_code(platform::video::native_scale_mode()));
                    }
                }
                "error" => {
                    let s = ui.peek().clone();
                    if s.src.is_empty() {
                        return;
                    }
                    if !s.transcoded {
                        // The browser could not decode the file: let the server re-encode it.
                        force_transcode.set(true);
                        load_source(s.position);
                    } else {
                        let my_gen = play_gen();
                        let t = err_token.clone();
                        let key = release.peek().infohash.clone();
                        let preloaded = s.preloaded;
                        sync::send(&SyncClientMsg::Ready {
                            position_sec: s.position,
                            duration_sec: s.duration,
                            file_index: info.peek().as_ref().map(|i| i.file_index),
                        });
                        ui.with_mut(|s| {
                            s.src.clear();
                            s.subtitles_src = None;
                            s.waiting = false;
                            s.progress = None;
                            s.status = Some("Проверяем состояние потока…".into());
                        });
                        stop_video_element();
                        spawn(async move {
                            let healthy = if preloaded {
                                true
                            } else {
                                api::play_progress(&t, &key)
                                    .await
                                    .is_ok_and(|p| p.peers > 0 || p.loaded_bytes > 0)
                            };
                            if play_gen() != my_gen || closing() {
                                return;
                            }
                            ui.with_mut(|s| {
                                s.status = None;
                                s.error = Some(if healthy {
                                    "Видеопоток не запустился или был прерван (сервер занят или поток не ответил вовремя). Попробуйте обновить или запустить ещё раз.".into()
                                } else {
                                    "Видео не загружается. Возможно, у раздачи не осталось сидов: попробуйте другую раздачу.".into()
                                });
                            });
                        });
                    }
                }
                _ => {}
            }
        }
    };
    let on_video = use_callback(on_video);
    platform::video::use_native_events(on_video);

    let finish_close = Callback::new({
        let mut save = save.clone();
        let token = token.clone();
        let client_id = client_id.clone();
        move |to_title: bool| {
            if closing() {
                return;
            }
            let from_pop = !to_title && telegram::in_popstate();
            closing.set(true);
            invalidate_prefetch();
            ws_gen.set(ws_gen() + 1);
            let s = ui.peek().clone();
            if !sync_connected() {
                save(false);
            }
            pause_video();
            ui.with_mut(|s| {
                s.src.clear();
                s.subtitles_src = None;
                s.waiting = false;
                s.status = None;
            });
            stop_video_element();
            sync::stop(Some(&SyncClientMsg::Leave {
                position_sec: s.position,
                duration_sec: s.duration,
                finished: false,
            }));
            let leave_req = SyncLeaveRequest {
                client_id: client_id.clone(),
                kind,
                tmdb_id: id,
                position_sec: s.position,
                duration_sec: s.duration,
                finished: false,
            };
            let t = token.clone();
            // Detach HTTP leave so a slow/offline network never traps the user on a black screen.
            dioxus::core::spawn_forever(async move {
                let _ = api::sync_leave(&t, &leave_req).await;
            });
            if to_title {
                on_title.call(());
            } else {
                on_close.call(from_pop);
            }
        }
    });
    let close = Callback::new(move |_: ()| finish_close.call(false));
    let open_title = Callback::new(move |_: ()| finish_close.call(true));

    // On a phone in Telegram the film takes the whole screen, and Telegram's Back closes it.
    use_hook(|| {
        if telegram::is_mobile() {
            telegram::set_fullscreen(true);
        }
        std::rc::Rc::new(FullscreenGuard)
    });
    // Films are watched sideways: the phone turns, the bars go.
    use_hook(|| {
        platform::player_mode(true);
        std::rc::Rc::new(PlayerModeGuard)
    });
    use_hook(show_controls);

    // Jump by seconds from the current place (double tap, buttons, keyboard).
    let mut flash_jump = move |forward: bool| {
        let cur = ui.peek().position;
        seek_to(cur + if forward { 10.0 } else { -10.0 });
        let n = flash_gen() + 1;
        flash_gen.set(n);
        ui.with_mut(|s| {
            let total = match s.flash {
                Some((f, secs)) if f == forward => secs + 10,
                _ => 10,
            };
            s.flash = Some((forward, total));
        });
        spawn(async move {
            platform::sleep(700).await;
            if flash_gen() == n {
                ui.with_mut(|s| s.flash = None);
            }
        });
    };

    let mut set_vol = move |v: f32| {
        let clamped = v.clamp(0.0, 1.0);
        volume.set(clamped);
        let is_muted = clamped <= 0.001;
        muted.set(is_muted);
        if let Some(vid) = video() {
            vid.set_volume(f64::from(clamped));
            vid.set_muted(is_muted || ui.peek().is_sync_muted());
        }
        show_controls();
    };
    let mut toggle_mute = {
        let mut switch_sync_sound = switch_sync_sound.clone();
        let self_cid = client_id.clone();
        move || {
            if ui.peek().is_sync_muted() {
                switch_sync_sound(self_cid.clone());
                return;
            }
            if auto_muted() {
                unmute_auto();
                show_controls();
                return;
            }
            let next = !muted();
            muted.set(next);
            if !next && volume() <= 0.001 {
                volume.set(0.7);
                if let Some(vid) = video() {
                    vid.set_volume(0.7);
                }
            }
            if let Some(vid) = video() {
                vid.set_muted(next);
            }
            show_controls();
        }
    };
    let mut toggle_fullscreen = move || {
        platform::toggle_fullscreen();
        if ui.peek().controls {
            show_controls();
        }
    };
    let mut toggle_scale = {
        let key = title_scale_key.clone();
        move || {
            let current = match saved_scale() {
                Some(s) => s,
                None => active_scale(),
            };
            let next = current.toggle();
            user_touched_scale.set(true);
            saved_scale.set(Some(next));
            active_scale.set(next);
            if platform::video::supports_native() {
                platform::video::native_set_scale(scale_code(next));
            }
            apply_web_scale(Some(next));
            let encoded = scales_cache.with_mut(|entries| upsert_saved_scale(entries, &key, next));
            platform::storage_set(SCALE_STORAGE_KEY, &encoded);
            on_scale.call(next);
            show_controls();
        }
    };

    // Load per-title video scale (DB takes precedence; localStorage acts as fallback/cache)
    // and track HTML `<video>` aspect fit.
    {
        let key = title_scale_key.clone();
        use_hook(move || {
            spawn(async move {
                let mut entries = platform::storage_get(SCALE_STORAGE_KEY)
                    .await
                    .map(|raw| decode_saved_scales(&raw))
                    .unwrap_or_default();
                if user_touched_scale() {
                    if let Some(picked) = *saved_scale.peek() {
                        let encoded = upsert_saved_scale(&mut entries, &key, picked);
                        scales_cache.set(entries);
                        platform::storage_set(SCALE_STORAGE_KEY, &encoded);
                    } else {
                        scales_cache.set(entries);
                    }
                    return;
                }
                if let Some(s) = db_scale {
                    let encoded = upsert_saved_scale(&mut entries, &key, s);
                    scales_cache.set(entries);
                    platform::storage_set(SCALE_STORAGE_KEY, &encoded);
                    return;
                }
                let loaded = lookup_saved_scale(&entries, &key);
                scales_cache.set(entries);
                if let Some(s) = loaded {
                    saved_scale.set(Some(s));
                    active_scale.set(s);
                    if platform::video::supports_native() {
                        platform::video::native_set_scale(scale_code(s));
                    }
                    apply_web_scale(Some(s));
                }
            });
        });
    }
    use_hook(move || {
        let initial_js_mode = match db_scale {
            Some(VideoScale::FitWidth) => "width",
            Some(VideoScale::FitHeight) => "height",
            None => "auto",
        };
        spawn(async move {
            let mut bridge = dioxus::document::eval(&format!(
                "if (window.__kinoPlayerScaleStop) {{ try {{ window.__kinoPlayerScaleStop(); }} catch (e) {{}} }}
                 window.__kinoVideoScaleMode = window.__kinoVideoScaleMode || '{initial_js_mode}';
                 var lastSent = '';
                 var ro = null;
                 var observed = false;
                 var ensureObs = function() {{
                   if (ro && !observed) {{
                     var p = document.querySelector('.player');
                     if (p) {{ ro.observe(p); observed = true; }}
                   }}
                 }};
                 var sync = function() {{
                   ensureObs();
                   var mode = window.__kinoVideoScaleMode || 'auto';
                   var v = document.getElementById('kino-video');
                   if (!v || v.tagName !== 'VIDEO') return;
                   var vw = v.videoWidth || 0;
                   var vh = v.videoHeight || 0;
                   var cw = v.clientWidth || 0;
                   var ch = v.clientHeight || 0;
                   var fit = 'contain';
                   var eff = mode === 'height' ? 'height' : 'width';
                   if (vw > 0 && vh > 0 && cw > 0 && ch > 0) {{
                     var vr = vw / vh;
                     var cr = cw / ch;
                     if (mode === 'width') {{
                       eff = 'width';
                       fit = (vr < cr - 0.01) ? 'cover' : 'contain';
                     }} else if (mode === 'height') {{
                       eff = 'height';
                       fit = (vr > cr + 0.01) ? 'cover' : 'contain';
                     }} else {{
                       eff = (vr >= cr) ? 'width' : 'height';
                       fit = 'contain';
                     }}
                   }}
                   if (v.style.objectFit !== fit) v.style.objectFit = fit;
                   if (mode === 'auto' && eff !== lastSent) {{
                     lastSent = eff;
                     dioxus.send(eff);
                   }}
                 }};
                 window.__kinoSyncVideoScale = sync;
                 var onMeta = function(e) {{
                   if (e && e.target && e.target.id === 'kino-video') sync();
                 }};
                 var onResize = function() {{ sync(); }};
                 document.addEventListener('loadedmetadata', onMeta, true);
                 document.addEventListener('loadeddata', onMeta, true);
                 document.addEventListener('resize', onMeta, true);
                 document.addEventListener('fullscreenchange', onResize);
                 window.addEventListener('resize', onResize);
                 if (typeof ResizeObserver === 'function') {{
                   ro = new ResizeObserver(function() {{ sync(); }});
                 }}
                 sync();
                 if (typeof requestAnimationFrame === 'function') requestAnimationFrame(sync);
                 window.__kinoPlayerScaleStop = function() {{
                   document.removeEventListener('loadedmetadata', onMeta, true);
                   document.removeEventListener('loadeddata', onMeta, true);
                   document.removeEventListener('resize', onMeta, true);
                   document.removeEventListener('fullscreenchange', onResize);
                   window.removeEventListener('resize', onResize);
                   if (ro) ro.disconnect();
                   delete window.__kinoSyncVideoScale;
                   delete window.__kinoVideoScaleMode;
                 }};"
            ));
            while let Ok(eff) = bridge.recv::<String>().await {
                if saved_scale.peek().is_none() {
                    if let Some(s) = VideoScale::parse(&eff) {
                        active_scale.set(s);
                    }
                }
            }
        });
        std::rc::Rc::new(ScaleGuard)
    });

    // Desktop keyboard shortcuts (Space/K, ←/→/J/L, ↑/↓ volume, M mute, F fullscreen, Esc).
    {
        let mut flash_jump = flash_jump;
        let mut set_vol = set_vol;
        let mut toggle_mute = toggle_mute.clone();
        let mut show_controls = show_controls;
        let mut toggle_play = toggle_play.clone();
        use_hook(move || {
            spawn(async move {
                let mut bridge = dioxus::document::eval(
                    "if (window.__kinoPlayerKeysStop) { try { window.__kinoPlayerKeysStop(); } catch (e) {} }
                     var handler = function(e) {
                       var tag = e.target && e.target.tagName;
                       var isText = (tag === 'INPUT' && e.target.type !== 'range') || tag === 'TEXTAREA' || (e.target && e.target.isContentEditable);
                       if (isText || e.ctrlKey || e.metaKey || e.altKey) return;
                       var code = e.code;
                       if (code === 'Space' || code === 'KeyK') { e.preventDefault(); dioxus.send('toggle'); }
                       else if (code === 'ArrowLeft' || code === 'KeyJ') { e.preventDefault(); dioxus.send('back'); }
                       else if (code === 'ArrowRight' || code === 'KeyL') { e.preventDefault(); dioxus.send('fwd'); }
                       else if (code === 'ArrowUp') { e.preventDefault(); dioxus.send('vol_up'); }
                       else if (code === 'ArrowDown') { e.preventDefault(); dioxus.send('vol_dn'); }
                       else if (code === 'KeyM') { e.preventDefault(); dioxus.send('mute'); }
                       else if (code === 'KeyF') { e.preventDefault(); dioxus.send('full'); }
                       else if (code === 'Escape') {
                         if (document.fullscreenElement && document.exitFullscreen) {
                           e.preventDefault();
                           document.exitFullscreen().catch(function(){});
                         } else {
                           e.preventDefault();
                           dioxus.send('esc');
                         }
                       }
                     };
                     window.addEventListener('keydown', handler);
                     window.__kinoPlayerKeysStop = function() { window.removeEventListener('keydown', handler); };",
                );
                while let Ok(cmd) = bridge.recv::<String>().await {
                    match cmd.as_str() {
                        "toggle" => {
                            toggle_play();
                            show_controls();
                        }
                        "back" => {
                            flash_jump(false);
                            show_controls();
                        }
                        "fwd" => {
                            flash_jump(true);
                            show_controls();
                        }
                        "vol_up" => {
                            set_vol(*volume.peek() + 0.1);
                            show_controls();
                        }
                        "vol_dn" => {
                            set_vol(*volume.peek() - 0.1);
                            show_controls();
                        }
                        "mute" => {
                            toggle_mute();
                            show_controls();
                        }
                        "full" => toggle_fullscreen(),
                        "esc" => {
                            if ui.peek().menu.is_some() {
                                ui.with_mut(|s| s.menu = None);
                                show_controls();
                            } else {
                                close.call(());
                            }
                        }
                        _ => {}
                    }
                }
            });
            std::rc::Rc::new(KeyGuard)
        });
    }

    // Plays episode `n` of the current season.
    let mut play_episode = {
        let mut save = save.clone();
        let mut begin_playback = begin_playback.clone();
        move |n: i32| {
            let (season, current) = target();
            if n == current {
                ui.with_mut(|s| s.menu = None);
                show_controls();
                return;
            }
            save(false);
            invalidate_prefetch();
            target.set((season, n));
            sync::send(&SyncClientMsg::Episode {
                season,
                episode: n,
                release: None,
            });
            begin_playback(false);
        }
    };

    let finger = touch.read().unwrap_or(false);
    // What the view shows: our state plus the episode list, volume and current episode.
    let shown = {
        let mut st = ui();
        let (season, episode) = target();
        st.season = season;
        st.episode = episode;
        st.rate = rate();
        st.scale = match saved_scale() {
            Some(s) => s,
            None => active_scale(),
        };
        st.volume = volume();
        st.muted = muted();
        st.show_volume = !finger;
        st.touch_controls = finger;
        st.episodes = season_list.read().clone().unwrap_or_default();
        let scope_s = if kind == MediaKind::Tv { season } else { 0 };
        if let Some((qs, qlist)) = quality_cache() {
            if qs == scope_s {
                st.available_qualities = qlist;
                st.qualities_loaded = true;
            }
        }
        st.active_infohash = Some(release().infohash);
        st
    };

    let menu_token = token.clone();
    let qual_token = token.clone();
    let mut start_fetch_qual = start_play_fetch.clone();
    let mut save_qual = save.clone();

    rsx! {
        native::BackButton { on_back: move |_| close.call(()) }
        PlayerView {
            state: shown,
            on_tap: {
                let mut toggle_play = toggle_play.clone();
                move |_| {
                    if auto_muted() {
                        unmute_auto();
                        if finger {
                            show_controls();
                        }
                        return;
                    }
                    if ui.peek().menu.is_some() {
                        ui.with_mut(|s| s.menu = None);
                        show_controls();
                    } else if finger {
                        if ui.peek().paused {
                            show_controls();
                        } else if ui.peek().controls {
                            ui.with_mut(|s| s.controls = false);
                        } else {
                            show_controls();
                        }
                    } else {
                        // Mouse: a click on the picture pauses or resumes.
                        toggle_play();
                    }
                }
            },
            on_double_tap: move |forward: bool| flash_jump(forward),
            on_speed: move |r: f32| {
                rate.set(r);
                ui.with_mut(|s| s.menu = None);
                show_controls();
                if let Some(v) = video() {
                    v.set_rate(f64::from(r));
                }
                sync::send(&SyncClientMsg::Rate {
                    rate: r,
                    position_sec: ui.peek().position,
                });
            },
            on_episode: move |n: i32| play_episode(n),
            on_pick_release: {
                let mut save = save.clone();
                move |_| {
                    save(false);
                    let (season, episode) = target();
                    on_need_release.call((season, episode));
                }
            },
            on_toggle: move |_| toggle_play(),
            on_seek: move |at: f64| seek_to(at),
            on_menu: move |m: Option<PlayerMenu>| {
                ui.with_mut(|s| s.menu = m);
                show_controls();
                if m == Some(PlayerMenu::Episodes) {
                    // The list is long: bring the current episode into view.
                    spawn(async move {
                        platform::sleep(50).await;
                        let _ = dioxus::document::eval(
                            "var e = document.querySelector('.p-popover .p-opt.on'); if (e) { e.scrollIntoView({ block: 'center' }); }",
                        );
                    });
                } else if m == Some(PlayerMenu::Quality) {
                    let scope_s = if kind == MediaKind::Tv { target.peek().0 } else { 0 };
                    if quality_cache.peek().as_ref().map(|(s, _)| *s) != Some(scope_s) && !*quality_loading.peek() {
                        quality_loading.set(true);
                        let tok = menu_token.clone();
                        let cur_rel = release.peek().clone();
                        let cur_choices = choices.peek().clone();
                        spawn(async move {
                            let res = api::releases(&tok, kind, id, scope_s).await;
                            quality_loading.set(false);
                            let items = res
                                .map(|l| best_per_quality(&l.releases, &cur_choices, &cur_rel, scope_s))
                                .unwrap_or_default();
                            quality_cache.set(Some((scope_s, items)));
                        });
                    }
                }
            },
            on_pick_quality: Some(Callback::new(move |rel: ReleaseOption| {
                let cur_pos = ui.peek().position;
                save_qual(false);
                override_seek.set(Some(cur_pos));
                let (cur_s, cur_e) = *target.peek();
                let (scope_s, scope_e) = if kind == MediaKind::Movie {
                    (0, 0)
                } else if release.peek().episode > 0 {
                    (cur_s, cur_e)
                } else {
                    (cur_s, 0)
                };
                let qk = normalize_quality_key(&rel.quality).to_owned();
                let new_choice = ReleaseChoiceUpdate {
                    kind,
                    tmdb_id: id,
                    season: scope_s,
                    episode: scope_e,
                    infohash: rel.infohash.clone(),
                    title: Some(rel.title.clone()),
                    quality: Some(rel.quality.clone()),
                    quality_key: Some(qk.clone()),
                    voice: (!rel.voice.is_empty()).then(|| rel.voice.clone()),
                };
                release.set(new_choice.clone());
                invalidate_prefetch();
                let my_gen = play_gen() + 1;
                play_gen.set(my_gen);
                ui.with_mut(|s| {
                    s.menu = None;
                    s.active_infohash = Some(new_choice.infohash.clone());
                });
                let tok = qual_token.clone();
                let req_q = api::SetDeviceQualityRequest {
                    kind,
                    tmdb_id: id,
                    season: scope_s,
                    episode: scope_e,
                    is_mobile: telegram::is_mobile(),
                    quality_key: qk,
                };
                spawn(async move {
                    let _ = api::set_release_quality(&tok, &req_q).await;
                });
                sync::send(&SyncClientMsg::Episode {
                    season: cur_s,
                    episode: cur_e,
                    release: Some(new_choice.clone()),
                });
                start_fetch_qual(cur_s, cur_e, new_choice, cur_pos, my_gen);
            })),
            on_jump_episode: Some(Callback::new({
                let mut jump_fetch = start_play_fetch.clone();
                move |(ns, ne): (i32, i32)| {
                    let rel = release.peek().clone();
                    invalidate_prefetch();
                    let my_gen = play_gen() + 1;
                    play_gen.set(my_gen);
                    target.set((ns, ne));
                    sync::send(&SyncClientMsg::Episode {
                        season: ns,
                        episode: ne,
                        release: None,
                    });
                    jump_fetch(ns, ne, rel, 0.0, my_gen);
                }
            })),
            on_audio: {
                let save_tracks = save_tracks.clone();
                move |a: i32| {
                    if ui.peek().audio_track == Some(a) {
                        ui.with_mut(|s| s.menu = None);
                        show_controls();
                        return;
                    }
                    let at = ui.peek().position;
                    let label = ui
                        .peek()
                        .audio
                        .iter()
                        .find(|t| t.index == a)
                        .map(|t| t.label());
                    ui.with_mut(|s| {
                        s.audio_track = Some(a);
                        s.menu = None;
                    });
                    invalidate_prefetch();
                    show_controls();
                    sync::send(&SyncClientMsg::Audio {
                        audio_track: label,
                        position_sec: at,
                    });
                    load_source(at);
                    save_tracks();
                }
            },
            on_subtitle: {
                let save_tracks = save_tracks.clone();
                move |t: Option<i32>| {
                    let native = ui.peek().native_video;
                    if native {
                        platform::video::native_set_subtitle(t);
                    }
                    let off = offset();
                    let url = (!native)
                        .then(|| {
                            info.peek()
                                .as_ref()
                                .filter(|i| i.preloaded)
                                .and_then(|i| t.map(|t| i.subtitle_url(t, off)))
                        })
                        .flatten();
                    ui.with_mut(|s| {
                        s.subtitle_track = t;
                        s.subtitles_src = url;
                        s.menu = None;
                    });
                    invalidate_prefetch();
                    show_controls();
                    save_tracks();
                }
            },
            on_next: move |_| go_next(),
            on_scale: move |_| toggle_scale(),
            on_fullscreen: move |_| toggle_fullscreen(),
            on_close: close,
            on_title: open_title,
            on_video,
            on_volume: move |v: f32| set_vol(v),
            on_mute: move |_| toggle_mute(),
            on_unmute: move |_| unmute_auto(),
            on_sync_sound: {
                let mut switch_sync_sound = switch_sync_sound;
                move |cid: String| switch_sync_sound(cid)
            },
            on_activity: move |_| show_controls(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_storage_round_trips_and_evicts_oldest() {
        let k1 = scale_title_key(MediaKind::Movie, 27205);
        let k2 = scale_title_key(MediaKind::Tv, 1396);
        assert_eq!(k1, "movie:27205");
        assert_eq!(k2, "tv:1396");

        let mut entries = decode_saved_scales("not json");
        assert!(entries.is_empty());

        let raw = upsert_saved_scale(&mut entries, &k1, VideoScale::FitHeight);
        let raw = {
            let mut e = decode_saved_scales(&raw);
            upsert_saved_scale(&mut e, &k2, VideoScale::FitWidth)
        };
        let mut parsed = decode_saved_scales(&raw);
        assert_eq!(lookup_saved_scale(&parsed, &k1), Some(VideoScale::FitHeight));
        assert_eq!(lookup_saved_scale(&parsed, &k2), Some(VideoScale::FitWidth));
        assert_eq!(lookup_saved_scale(&parsed, "movie:999"), None);

        // Updating an existing key refreshes its value without duplicating it.
        let updated = upsert_saved_scale(&mut parsed, &k1, VideoScale::FitWidth);
        let parsed = decode_saved_scales(&updated);
        assert_eq!(parsed.iter().filter(|(k, _)| k == &k1).count(), 1);
        assert_eq!(lookup_saved_scale(&parsed, &k1), Some(VideoScale::FitWidth));

        // Capacity stays bounded to MAX_SAVED_SCALES.
        let mut many = Vec::new();
        for i in 0..(MAX_SAVED_SCALES + 5) {
            upsert_saved_scale(&mut many, &format!("movie:{i}"), VideoScale::FitHeight);
        }
        assert_eq!(many.len(), MAX_SAVED_SCALES);
        assert_eq!(lookup_saved_scale(&many, "movie:0"), None);
        assert_eq!(
            lookup_saved_scale(&many, &format!("movie:{}", MAX_SAVED_SCALES + 4)),
            Some(VideoScale::FitHeight)
        );
    }

    #[test]
    fn scale_codes_map_between_native_and_ui() {
        assert_eq!(scale_code(VideoScale::FitWidth), 1);
        assert_eq!(scale_code(VideoScale::FitHeight), 2);
        assert_eq!(scale_from_code(1), VideoScale::FitWidth);
        assert_eq!(scale_from_code(2), VideoScale::FitHeight);
        assert_eq!(serde_json::to_string(&VideoScale::FitHeight).unwrap(), "\"height\"");
        assert_eq!(
            serde_json::from_str::<VideoScale>("\"width\"").unwrap(),
            VideoScale::FitWidth
        );
        assert_eq!(
            bust_cache_url("/api/stream/tok?audio=0&start=15", 101),
            "/api/stream/tok?audio=0&start=15&r=101"
        );
        assert_eq!(
            bust_cache_url("/api/stream/tok?audio=0&start=15&r=101", 202),
            "/api/stream/tok?audio=0&start=15&r=202"
        );
        assert_eq!(bust_cache_url("/api/stream/tok?r=101", 303), "/api/stream/tok?r=303");
    }

    fn sample_play_info(preloaded: bool) -> PlayInfo {
        PlayInfo {
            infohash: "a".repeat(40),
            file_index: 2,
            file_name: "ep2.mkv".into(),
            stream: "/api/stream/tok".into(),
            direct: true,
            duration: Some(1200.0),
            audio: Vec::new(),
            subtitles: Vec::new(),
            audio_track: Some(0),
            subtitle_track: None,
            start_at: 0,
            next: Some((1, 3)),
            preloaded,
            native: false,
        }
    }

    fn sample_choice(hash_char: char, season: i32) -> ReleaseChoiceUpdate {
        ReleaseChoiceUpdate {
            kind: MediaKind::Tv,
            tmdb_id: 1396,
            season,
            episode: 0,
            infohash: hash_char.to_string().repeat(40),
            title: Some("Release".into()),
            quality: Some("1080p".into()),
            quality_key: Some("1080p".into()),
            voice: Some("LostFilm".into()),
        }
    }

    #[test]
    fn next_prefetch_timing_and_buffer_guards() {
        // Too early or invalid durations never prefetch.
        assert!(!can_prefetch_next(100.0, 200.0, Some(30.0)));
        assert!(!can_prefetch_next(3.0, 35.0, Some(30.0)));
        assert!(!can_prefetch_next(10.0, 15.0, Some(15.0)));

        // Inside the prompt window: require >= 5s buffered ahead unless at EOF or <= 20s remaining.
        assert!(!can_prefetch_next(965.0, 1000.0, Some(2.0)));
        assert!(can_prefetch_next(965.0, 1000.0, Some(6.0)));
        assert!(can_prefetch_next(981.0, 1000.0, Some(1.0)));
        assert!(can_prefetch_next(998.5, 1000.0, Some(1.5)));
        // Native player without JS buffer telemetry triggers right in the prompt window.
        assert!(can_prefetch_next(965.0, 1000.0, None));
    }

    #[test]
    fn next_prefetch_freshness_and_refresh_rules() {
        let hash_a = "a".repeat(40);
        let live_info = sample_play_info(false);
        assert!(prefetch_is_fresh(
            &live_info,
            1_000,
            1_000 + PREFETCH_TORR_TTL_MS,
            PREFETCH_TORR_TTL_MS
        ));
        assert!(!prefetch_is_fresh(
            &live_info,
            1_000,
            1_000 + PREFETCH_TORR_TTL_MS + 1,
            PREFETCH_TORR_TTL_MS
        ));
        let disk_info = sample_play_info(true);
        assert!(prefetch_is_fresh(&disk_info, 1_000, 10_000_000, PREFETCH_TORR_TTL_MS));

        let pf = NextPrefetch {
            gen: 1,
            from_season: 1,
            from_episode: 1,
            from_infohash: hash_a.clone(),
            next_season: 1,
            next_episode: 2,
            resolved_choice: Some(sample_choice('a', 1)),
            status: NextPrefetchStatus::Ready {
                info: live_info,
                prepared_at_ms: 10_000,
            },
        };
        assert!(!needs_prefetch_refresh(Some(&pf), 1, 1, &hash_a, 1, 2, 10_000 + 30_000));
        assert!(needs_prefetch_refresh(
            Some(&pf),
            1,
            1,
            &hash_a,
            1,
            2,
            10_000 + PREFETCH_REFRESH_MS + 1
        ));
        assert!(needs_prefetch_refresh(Some(&pf), 1, 2, &hash_a, 1, 3, 10_000));
    }

    #[test]
    fn resolves_and_upserts_next_season_choices() {
        let c1 = sample_choice('a', 1);
        let c2 = sample_choice('b', 2);
        let c2_new = sample_choice('c', 2);
        let mut list = vec![c1.clone(), c2.clone()];
        upsert_choice(&mut list, c2_new.clone());
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], c2_new);
        assert_eq!(list[1], c1);

        let ready = ResolveReleaseResponse::Ready {
            choice: c2.clone(),
            from_peer: None,
        };
        assert_eq!(resolve_response_to_choice(ready), Some(c2.clone()));

        let mismatch = ResolveReleaseResponse::PreloadQualityMismatch {
            preloaded: c1.clone(),
            preloaded_quality: "720p".into(),
            target_quality: "1080p".into(),
            fallback: Box::new(ResolveReleaseResponse::Ready {
                choice: c2.clone(),
                from_peer: None,
            }),
        };
        assert_eq!(resolve_response_to_choice(mismatch), Some(c2));

        let picker = ResolveReleaseResponse::NeedsPicker {
            target_quality: "1080p".into(),
            releases: kino_core::ReleaseList {
                releases: Vec::new(),
                configured: true,
            },
        };
        assert_eq!(resolve_response_to_choice(picker), None);
    }
}
