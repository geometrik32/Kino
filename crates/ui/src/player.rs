//! The video player: a `<video>` with our own cinema floating glass controls,
//! unified Audio/Subtitles menu, screen lock, live scrub tooltip and desktop volume.

use std::rc::Rc;

use dioxus::html::MountedData;
use dioxus::prelude::*;
pub use kino_core::{
    active_skip_segment, next_prompt_start, MediaSegment, SegmentKind, VideoScale, NEXT_CREDITS_LEAD_SEC,
    NEXT_PROMPT_END_RATIO, NEXT_PROMPT_MIN_SEC,
};
use kino_core::{
    clock, episode_code, normalize_quality_key, rating_description, MediaTrack, PreloadItem, PreloadState,
    ReleaseOption, StreamProgress, SyncMemberInfo,
};

use crate::human_size;
use crate::icons::{self, Icon};
use crate::tooltip::{clear_active_tooltip, provide_tooltip_host, use_tip, TipHandle, TooltipLayer};
use crate::WindowChrome;

/// `id` of the `<video>` element; the app drives it through the DOM.
pub const VIDEO_ID: &str = "kino-video";

/// Which menu of the player is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerMenu {
    Audio,
    Subtitles,
    Quality,
    Speed,
    Scale,
    Episodes,
    Sync,
}

/// Playback speeds offered in the menu.
pub const SPEEDS: [f32; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

/// "1×", "1.25×".
pub fn speed_label(rate: f32) -> String {
    if (rate - rate.round()).abs() < 0.001 {
        format!("{}×", rate.round() as i32)
    } else {
        format!("{rate}×")
    }
}

/// One line of the episode list in the player.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerEpisode {
    pub number: i32,
    pub title: String,
    pub watched: bool,
}

/// Banner shown in the player when a preload of the current title finishes downloading to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskReadyPrompt {
    pub infohash: String,
    pub season: i32,
    pub title: Option<String>,
    pub quality: Option<String>,
    pub voice: Option<String>,
}

impl DiskReadyPrompt {
    /// Compact `1080p · Дубляж` hint displayed in the banner title when available.
    pub fn detail_label(&self) -> Option<String> {
        let q = self
            .quality
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(quality_short_label);
        let v = self.voice.as_deref().map(str::trim).filter(|s| !s.is_empty());
        match (q, v) {
            (Some(q), Some(v)) => Some(format!("{q} · {v}")),
            (Some(q), None) => Some(q.to_owned()),
            (None, Some(v)) => Some(v.to_owned()),
            (None, None) => None,
        }
    }
}

/// Everything the player shows. The app owns the state and the `<video>`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    /// Title, e.g. "Во все тяжкие".
    pub title: String,
    /// "S02E06 · Мандала", or the year of a film.
    pub subtitle: Option<String>,
    /// Release label, e.g. "LostFilm · 1080p · RuTracker".
    pub release: String,
    /// Video URL; empty while starting.
    pub src: String,
    /// WebVTT URL of the shown subtitles.
    pub subtitles_src: Option<String>,
    /// Picture shown before the first frame.
    pub poster: Option<String>,
    /// Absolute position and length, seconds.
    pub position: f64,
    pub duration: Option<f64>,
    pub paused: bool,
    /// Waiting for data (start, seek, slow peers).
    pub waiting: bool,
    /// What is happening while there is no picture yet.
    pub status: Option<String>,
    /// How far the torrent is from playing; shown instead of a bare spinner.
    pub progress: Option<StreamProgress>,
    pub error: Option<String>,
    pub audio: Vec<MediaTrack>,
    pub subtitle_tracks: Vec<MediaTrack>,
    pub audio_track: Option<i32>,
    pub subtitle_track: Option<i32>,
    pub menu: Option<PlayerMenu>,
    /// "S02E07", when there is a next episode.
    pub next: Option<String>,
    /// Re-encoded on the server (shown as a small note).
    pub transcoded: bool,
    /// Video renders natively behind a transparent WebView (e.g. Android ExoPlayer TextureView).
    pub native_video: bool,
    /// Played from the server's disk.
    pub preloaded: bool,
    /// The buttons and the seek bar are shown (a tap on the picture toggles them).
    pub controls: bool,
    /// Playback speed.
    pub rate: f32,
    /// Video scaling mode: fit width or fit height.
    pub scale: VideoScale,
    /// Volume 0.0..=1.0 and mute flag (desktop).
    pub volume: f32,
    pub muted: bool,
    /// Whether to show the inline desktop volume slider.
    pub show_volume: bool,
    /// Whether to show the fullscreen button (hidden in native Android immersive mode).
    pub show_fullscreen: bool,
    /// True while the player is currently in fullscreen mode.
    pub fullscreen: bool,
    /// Touch device mode: tapping reveals controls with a center Pause/Play button.
    pub touch_controls: bool,
    /// Season being watched and its episodes (series only); `episode` is the current one.
    pub season: i32,
    pub episode: i32,
    pub episodes: Vec<PlayerEpisode>,
    /// "«10 с» / «10 с»»" feedback after a double tap: forward or back, total seconds.
    pub flash: Option<(bool, i32)>,
    /// Auto-start `<video>` when loaded (false while joining a paused/waiting sync room).
    pub autoplay: bool,
    /// True when browser autoplay policy forced muted playback until the first user tap.
    pub auto_muted: bool,
    /// Devices currently connected to the live playback sync room.
    pub sync_members: Vec<SyncMemberInfo>,
    /// Device names currently holding the buffering barrier.
    pub sync_waiting_for: Vec<String>,
    /// Identifier of this client inside `sync_members`.
    pub sync_client_id: String,
    /// Identifier of the device currently outputting playback sound in the sync room.
    pub sync_sound_client_id: Option<String>,
    /// Representative releases per quality bucket (`2160p`, `1080p`, `720p`, `SD`) for the Quality tab.
    pub available_qualities: Vec<ReleaseOption>,
    /// Currently playing release infohash.
    pub active_infohash: Option<String>,
    /// Whether quality list finished loading for the current title/season.
    pub qualities_loaded: bool,
    /// Active/finished preloads for releases of the current title/season.
    pub release_downloads: Vec<PreloadItem>,
    /// Non-blocking notice/error shown inside the Quality tab when queuing a download fails.
    pub download_note: Option<String>,
    /// Info about a release that finished downloading to disk and awaits user switch confirmation.
    pub disk_ready_prompt: Option<DiskReadyPrompt>,
    /// Optional label shown when rewatching favorite episodes (e.g. `Только любимые (5★)`).
    pub rewatch_label: Option<String>,
    /// Next episode actually present in the current release when the requested episode is missing.
    pub next_available: Option<(i32, i32)>,
    /// Detected or community-sourced intro / recap / credits segments for the current video.
    pub segments: Vec<MediaSegment>,
    /// Current user's 1–5 rating for the playing episode (TV) or movie (`None` if unrated).
    pub my_rating: Option<i16>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            title: String::new(),
            subtitle: None,
            release: String::new(),
            src: String::new(),
            subtitles_src: None,
            poster: None,
            position: 0.0,
            duration: None,
            paused: false,
            waiting: false,
            status: None,
            progress: None,
            error: None,
            audio: Vec::new(),
            subtitle_tracks: Vec::new(),
            audio_track: None,
            subtitle_track: None,
            menu: None,
            next: None,
            transcoded: false,
            native_video: false,
            preloaded: false,
            controls: false,
            rate: 1.0,
            scale: VideoScale::default(),
            volume: 1.0,
            muted: false,
            show_volume: true,
            show_fullscreen: true,
            fullscreen: false,
            touch_controls: false,
            season: 0,
            episode: 0,
            episodes: Vec::new(),
            flash: None,
            autoplay: true,
            auto_muted: false,
            sync_members: Vec::new(),
            sync_waiting_for: Vec::new(),
            sync_client_id: String::new(),
            sync_sound_client_id: None,
            available_qualities: Vec::new(),
            active_infohash: None,
            qualities_loaded: false,
            release_downloads: Vec::new(),
            download_note: None,
            disk_ready_prompt: None,
            rewatch_label: None,
            next_available: None,
            segments: Vec::new(),
            my_rating: None,
        }
    }
}

impl PlayerState {
    /// True when multiple devices are synchronized and another device owns audio output.
    pub fn is_sync_muted(&self) -> bool {
        self.sync_members.len() > 1
            && self
                .sync_sound_client_id
                .as_deref()
                .is_some_and(|cid| cid != self.sync_client_id)
    }

    /// Preload record for `infohash` within the current title/season, if queued or ready.
    pub fn download_item(&self, infohash: &str) -> Option<&PreloadItem> {
        self.release_downloads
            .iter()
            .find(|p| p.infohash.eq_ignore_ascii_case(infohash))
    }

    /// Download state and progress `0..=1` for `infohash`, if it is queued or ready on disk.
    pub fn download_status(&self, infohash: &str) -> Option<(PreloadState, f32)> {
        self.download_item(infohash).map(|p| (p.state, p.progress))
    }
}

fn track_name(list: &[MediaTrack], index: Option<i32>, none: &str) -> String {
    index
        .and_then(|i| list.iter().find(|t| t.index == i))
        .map(|t| t.label())
        .unwrap_or_else(|| none.to_owned())
}

fn quality_rank_bucket(q: &str) -> u32 {
    match normalize_quality_key(q) {
        "2160p" => 4,
        "1080p" => 3,
        "720p" => 2,
        _ => 1,
    }
}

fn quality_short_label(q: &str) -> &'static str {
    match normalize_quality_key(q) {
        "2160p" => "4K",
        other => other,
    }
}

fn voice_from_release_label(label: &str) -> String {
    let mut parts = label.split(" · ").map(str::trim).filter(|s| !s.is_empty());
    let Some(first) = parts.next() else {
        return String::new();
    };
    let is_quality = matches!(
        first.to_ascii_lowercase().as_str(),
        "4k" | "uhd" | "2160p" | "1080p" | "720p" | "sd"
    );
    if is_quality {
        String::new()
    } else {
        first.to_owned()
    }
}

fn quality_from_release_label(label: &str) -> String {
    for part in label.split(" · ").map(str::trim) {
        let lower = part.to_ascii_lowercase();
        if matches!(lower.as_str(), "4k" | "uhd" | "2160p" | "1080p" | "720p" | "sd") {
            return normalize_quality_key(part).to_owned();
        }
    }
    "1080p".to_owned()
}

struct QualityDownloadAction<'a> {
    rel: ReleaseOption,
    is_cur: bool,
    playing_preloaded: bool,
    dl: Option<&'a PreloadItem>,
    on_download: Option<EventHandler<ReleaseOption>>,
    on_cancel_download: Option<EventHandler<i64>>,
    on_switch_to_disk: Option<EventHandler<()>>,
    tip: TipHandle,
    slot: u32,
}

fn render_quality_download_action(ctx: QualityDownloadAction<'_>) -> Element {
    let QualityDownloadAction {
        rel,
        is_cur,
        playing_preloaded,
        dl,
        on_download,
        on_cancel_download,
        on_switch_to_disk,
        tip,
        slot,
    } = ctx;
    let is_ready = dl.is_some_and(|p| p.state == PreloadState::Ready) || rel.preloaded || (is_cur && playing_preloaded);
    if is_ready {
        if is_cur && !playing_preloaded {
            if let Some(cb) = on_switch_to_disk {
                return rsx! {
                    button {
                        class: "p-q-dl switch",
                        "aria-label": "Переключиться на скачанный файл",
                        onmounted: move |e| tip.mount(slot, e.data()),
                        onmouseenter: move |_| tip.show(slot, "Переключиться на скачанный файл"),
                        onmouseleave: move |_| tip.hide(slot),
                        onmousedown: move |_| tip.hide(slot),
                        onclick: move |e| {
                            e.stop_propagation();
                            cb.call(());
                        },
                        Icon { svg: icons::CHECK, class: "ic xs" }
                        span { "Скачан" }
                    }
                };
            }
        }
        return rsx! {
            span {
                class: "p-q-dl done",
                onmounted: move |e| tip.mount(slot, e.data()),
                onmouseenter: move |_| tip.show(slot, "Уже скачан на сервер"),
                onmouseleave: move |_| tip.hide(slot),
                Icon { svg: icons::CHECK, class: "ic xs" }
            }
        };
    }
    if let Some(item) = dl.filter(|p| matches!(p.state, PreloadState::Queued | PreloadState::Downloading)) {
        if let Some(cancel) = on_cancel_download.filter(|_| item.can_delete) {
            let dl_id = item.id;
            return rsx! {
                button {
                    class: "p-q-dl busy",
                    "aria-label": "Отменить скачивание на сервер",
                    onmounted: move |e| tip.mount(slot, e.data()),
                    onmouseenter: move |_| tip.show(slot, "Отменить скачивание на сервер"),
                    onmouseleave: move |_| tip.hide(slot),
                    onmousedown: move |_| tip.hide(slot),
                    onclick: move |e| {
                        e.stop_propagation();
                        cancel.call(dl_id);
                    },
                    Icon { svg: icons::CLOSE, class: "ic xs" }
                }
            };
        }
        let st_tip = item.status_text();
        return rsx! {
            span {
                class: "p-q-dl busy",
                onmounted: move |e| tip.mount(slot, e.data()),
                onmouseenter: move |_| tip.show(slot, st_tip.clone()),
                onmouseleave: move |_| tip.hide(slot),
                "{item.compact_status_text()}"
            }
        };
    }
    if is_cur {
        if let Some(cb) = on_download {
            return rsx! {
                button {
                    class: "p-q-dl",
                    "aria-label": "Скачать текущую раздачу на сервер",
                    onmounted: move |e| tip.mount(slot, e.data()),
                    onmouseenter: move |_| tip.show(slot, "Скачать текущую раздачу на сервер"),
                    onmouseleave: move |_| tip.hide(slot),
                    onmousedown: move |_| tip.hide(slot),
                    onclick: move |e| {
                        e.stop_propagation();
                        cb.call(rel.clone());
                    },
                    Icon { svg: icons::DOWNLOAD, class: "ic xs" }
                }
            };
        }
    }
    rsx! {}
}

/// The bar shown while connecting to a release: a percentage when it is
/// known, else a moving bar with what we do know (peers, speed).
#[component]
pub fn ConnectProgress(progress: StreamProgress) -> Element {
    let percent = progress.percent();
    let details = progress.details();
    rsx! {
        div { class: "connect",
            div { class: "connect-head",
                span { "{progress.headline()}" }
                if let Some(p) = percent { b { "{p}%" } }
            }
            div { class: if percent.is_some() { "connect-bar" } else { "connect-bar wait" },
                div { class: "fill", style: "width: {percent.unwrap_or(0)}%" }
            }
            if !details.is_empty() { div { class: "connect-sub", "{details}" } }
        }
    }
}

/// Whether player controls (bars, dock, center play) should be visible.
pub fn controls_shown(locked: bool, state: &PlayerState, scrubbing: bool) -> bool {
    !locked && (state.controls || state.paused || state.menu.is_some() || scrubbing)
}

/// Filters zero-pixel synthetic `pointermove` events emitted on layout/cursor changes.
pub fn pointer_moved(prev: Option<(f64, f64)>, next: (f64, f64)) -> bool {
    match prev {
        Some((lx, ly)) => (next.0 - lx).abs() >= 1.0 || (next.1 - ly).abs() >= 1.0,
        None => true,
    }
}

#[component]
pub fn PlayerView(
    state: PlayerState,
    on_toggle: EventHandler<()>,
    /// Absolute second to jump to.
    on_seek: EventHandler<f64>,
    on_menu: EventHandler<Option<PlayerMenu>>,
    on_audio: EventHandler<i32>,
    on_subtitle: EventHandler<Option<i32>>,
    on_next: EventHandler<()>,
    on_scale: EventHandler<()>,
    on_fullscreen: EventHandler<()>,
    on_close: EventHandler<()>,
    #[props(default)] on_title: Option<EventHandler<()>>,
    /// A tap on the picture: show or hide the controls.
    on_tap: EventHandler<()>,
    /// A double tap on the left (`false`) or right (`true`) half: jump 10 seconds.
    on_double_tap: EventHandler<bool>,
    on_speed: EventHandler<f32>,
    /// Play this episode of the current season.
    on_episode: EventHandler<i32>,
    /// Choose another release (another voice-over) for the current episode.
    on_pick_release: EventHandler<()>,
    /// `<video>` events: time update, metadata, waiting, playing, pause, play, ended, error.
    on_video: EventHandler<&'static str>,
    #[props(default)] on_volume: Option<EventHandler<f32>>,
    #[props(default)] on_mute: Option<EventHandler<()>>,
    #[props(default)] on_unmute: Option<EventHandler<()>>,
    #[props(default)] on_sync_sound: Option<EventHandler<String>>,
    #[props(default)] on_activity: Option<EventHandler<()>>,
    #[props(default)] on_pick_quality: Option<EventHandler<ReleaseOption>>,
    #[props(default)] on_download_release: Option<EventHandler<ReleaseOption>>,
    #[props(default)] on_cancel_download: Option<EventHandler<i64>>,
    #[props(default)] on_switch_to_disk: Option<EventHandler<()>>,
    #[props(default)] on_dismiss_disk_prompt: Option<EventHandler<()>>,
    #[props(default)] on_jump_episode: Option<EventHandler<(i32, i32)>>,
    #[props(default)] on_lock: Option<EventHandler<bool>>,
    #[props(default)] on_rate: Option<EventHandler<Option<i16>>>,
    #[props(default)] window_chrome: Option<WindowChrome>,
) -> Element {
    let tip_host = provide_tooltip_host();
    let tip = use_tip();
    let mut locked = use_signal(|| false);
    let mut lock_hint = use_signal(|| false);
    let mut lock_hint_gen = use_signal(|| 0u64);
    let mut scrub = use_signal(|| None::<f64>);
    let mut track_el = use_signal(|| None::<Rc<MountedData>>);
    // `(is_vertical, start_coord, length)` so both horizontal and rotated vertical tracks share one drag path.
    let mut drag_rect = use_signal(|| None::<(bool, f64, f64)>);
    let mut drag_gen = use_signal(|| 0u64);
    let mut drag_active = use_signal(|| false);
    let mut drag_end_pos = use_signal(|| None::<(f64, f64)>);
    let mut dismissed_next = use_signal(|| None::<String>);
    let mut dismissed_rate = use_signal(|| None::<(i32, i32)>);
    let mut rate_just_saved = use_signal(|| None::<(i32, i32)>);
    let mut rate_hover = use_signal(|| None::<i16>);
    let mut rate_popped = use_signal(|| None::<(i16, u32)>);
    let mut rate_tick = use_signal(|| 0u32);
    let mut last_mouse = use_signal(|| None::<(f64, f64)>);
    let mut tap_gen = use_signal(|| 0u64);
    let ping_activity = move || {
        if let Some(cb) = on_activity {
            cb.call(());
        }
    };
    let mut show_lock_hint = move || {
        let n = *lock_hint_gen.peek() + 1;
        lock_hint_gen.set(n);
        lock_hint.set(true);
        spawn(async move {
            let _ = dioxus::document::eval("await new Promise(r => setTimeout(r, 3000)); return 0;")
                .join::<i32>()
                .await;
            if *lock_hint_gen.peek() == n {
                lock_hint.set(false);
            }
        });
    };

    let s = state;
    let dur = s.duration.unwrap_or(0.0);
    let pos = s.position.clamp(0.0, if dur > 0.0 { dur } else { f64::MAX });
    let display_pos = scrub()
        .unwrap_or(pos)
        .clamp(0.0, if dur > 0.0 { dur } else { f64::MAX });
    let pct = if dur > 0.0 { display_pos / dur * 100.0 } else { 0.0 };
    let remaining = (dur - display_pos).max(0.0);
    let prompt_at = next_prompt_start(dur, &s.segments);
    let near_end = dur > 0.0 && prompt_at.is_some_and(|at| pos >= at);
    let disk_prompt = s.disk_ready_prompt.clone();
    let show_disk_prompt = disk_prompt.is_some();
    let disk_ready_title = match disk_prompt.as_ref().and_then(DiskReadyPrompt::detail_label) {
        Some(detail) => format!("Скачалось ({detail})"),
        None if s.season > 0 => "Сезон скачался".to_owned(),
        None => "Фильм скачался".to_owned(),
    };
    let show_next = !show_disk_prompt
        && near_end
        && s.next.is_some()
        && dismissed_next().as_deref() != s.next.as_deref()
        && !locked();
    let rate_ep_key = (s.season, s.episode);
    let show_rate = on_rate.is_some()
        && !show_disk_prompt
        && near_end
        && !locked()
        && !s.waiting
        && s.error.is_none()
        && dismissed_rate() != Some(rate_ep_key)
        && (s.my_rating.is_none() || rate_just_saved() == Some(rate_ep_key));
    let active_skip = if !locked() && !show_disk_prompt && !s.waiting && s.error.is_none() {
        active_skip_segment(pos, dur, &s.segments, s.next.is_some()).cloned()
    } else {
        None
    };
    let sub_name = track_name(&s.subtitle_tracks, s.subtitle_track, "Выкл");
    let sub_lang = s
        .subtitle_track
        .and_then(|idx| s.subtitle_tracks.iter().find(|t| t.index == idx))
        .and_then(|t| t.language.as_deref())
        .map(|l| match l {
            "rus" | "ru" => "ru",
            "eng" | "en" => "en",
            "ukr" | "uk" => "uk",
            "jpn" | "ja" => "ja",
            other if other.len() == 2 => other,
            _ => "ru",
        })
        .unwrap_or("ru");
    let headphones_open = matches!(s.menu, Some(PlayerMenu::Audio | PlayerMenu::Subtitles));
    let settings_open = matches!(
        s.menu,
        Some(PlayerMenu::Quality | PlayerMenu::Speed | PlayerMenu::Scale)
    );
    let shown = controls_shown(locked(), &s, scrub().is_some());
    let busy = s.error.is_none() && (s.waiting || s.status.is_some());
    let rate = if s.rate > 0.0 { s.rate } else { 1.0 };
    let sync_muted = s.is_sync_muted();
    let effective_muted = s.muted || s.auto_muted || sync_muted;
    let vol_pct = if effective_muted {
        0.0
    } else {
        (s.volume.clamp(0.0, 1.0)) * 100.0
    };
    let next_for_dismiss = s.next.clone();

    let player_cls = match (shown, s.native_video) {
        (true, true) => "player show native-video",
        (true, false) => "player show",
        (false, true) => "player native-video",
        (false, false) => "player",
    };

    rsx! {
        div {
            class: "{player_cls}",
            onmounted: move |e| tip_host.mount_root(e.data()),
            onpointermove: move |e| {
                let active_rect = *drag_rect.peek();
                if let Some((vert, start, len)) = active_rect {
                    if e.data().pointer_type() == "mouse"
                        && !e
                            .held_buttons()
                            .contains(dioxus::html::input_data::MouseButton::Primary)
                    {
                        let next_gen = *drag_gen.peek() + 1;
                        drag_gen.set(next_gen);
                        drag_active.set(false);
                        drag_end_pos.set(None);
                        drag_rect.set(None);
                        if let Some(v) = scrub.take() {
                            on_seek.call(v);
                        }
                        return;
                    }
                    let c = e.client_coordinates();
                    let coord = if vert { c.y } else { c.x };
                    let frac = ((coord - start) / len).clamp(0.0, 1.0);
                    scrub.set(Some(frac * dur.max(1.0)));
                    ping_activity();
                    return;
                }
                if e.data().pointer_type() == "mouse" {
                    let p = e.client_coordinates();
                    let next = (p.x, p.y);
                    if pointer_moved(*last_mouse.peek(), next) {
                        last_mouse.set(Some(next));
                        ping_activity();
                    }
                }
            },
            onpointerup: move |e| {
                if *drag_active.peek() {
                    drag_active.set(false);
                    let c = e.client_coordinates();
                    if let Some((vert, start, len)) = drag_rect.take() {
                        let coord = if vert { c.y } else { c.x };
                        let frac = ((coord - start) / len).clamp(0.0, 1.0);
                        let v = frac * dur.max(1.0);
                        scrub.set(None);
                        on_seek.call(v);
                    } else {
                        // `get_client_rect().await` is still resolving; finish the tap once it returns.
                        drag_end_pos.set(Some((c.x, c.y)));
                    }
                }
            },
            onpointercancel: move |_| {
                if *drag_active.peek() || drag_rect.peek().is_some() {
                    drag_gen.set(drag_gen() + 1);
                    drag_active.set(false);
                    drag_end_pos.set(None);
                    drag_rect.set(None);
                    scrub.set(None);
                }
            },
            onmouseleave: move |_| {
                last_mouse.set(None);
            },
            if s.native_video {
                div { id: VIDEO_ID, class: "p-native-slot" }
            } else {
                video {
                    id: VIDEO_ID,
                    src: if s.src.is_empty() { None } else { Some(s.src.clone()) },
                    poster: s.poster.clone(),
                    autoplay: s.autoplay,
                    muted: effective_muted,
                    playsinline: true,
                    preload: "auto",
                    ontimeupdate: move |_| on_video.call("time"),
                    onloadedmetadata: move |_| on_video.call("metadata"),
                    oncanplay: move |_| on_video.call("canplay"),
                    onseeked: move |_| on_video.call("seeked"),
                    onwaiting: move |_| on_video.call("waiting"),
                    onstalled: move |_| on_video.call("stalled"),
                    onplaying: move |_| on_video.call("playing"),
                    onpause: move |_| on_video.call("pause"),
                    onplay: move |_| on_video.call("play"),
                    onended: move |_| on_video.call("ended"),
                    onerror: move |_| on_video.call("error"),
                    if let Some(vtt) = &s.subtitles_src {
                        track {
                            key: "{vtt}",
                            kind: "subtitles",
                            src: "{vtt}",
                            srclang: "{sub_lang}",
                            label: "{sub_name}",
                            default: true,
                        }
                    }
                }
            }
            div { class: "p-vignette" }

            if locked() {
                div {
                    class: "p-lock-screen",
                    onclick: move |_| {
                        if *lock_hint.peek() {
                            let next_gen = *lock_hint_gen.peek() + 1;
                            lock_hint_gen.set(next_gen);
                            lock_hint.set(false);
                        } else {
                            show_lock_hint();
                        }
                    },
                    if lock_hint() {
                        button {
                            class: "p-glass-btn p-unlock-btn",
                            onclick: move |e| {
                                e.stop_propagation();
                                locked.set(false);
                                let next_gen = *lock_hint_gen.peek() + 1;
                                lock_hint_gen.set(next_gen);
                                lock_hint.set(false);
                                if let Some(cb) = on_lock {
                                    cb.call(false);
                                }
                                ping_activity();
                            },
                            Icon { svg: icons::UNLOCK, class: "ic sm" }
                            "Разблокировать экран"
                        }
                    }
                }
            } else {
                // Taps on the picture: debounce single tap vs double tap so jumping ±10s never fires pause+play first.
                div { class: "p-zones",
                    div {
                        class: "p-zone",
                        onclick: move |_| {
                            let n = *tap_gen.peek() + 1;
                            tap_gen.set(n);
                            spawn(async move {
                                let _ = dioxus::document::eval("await new Promise(r => setTimeout(r, 220)); return 0;")
                                    .join::<i32>()
                                    .await;
                                if *tap_gen.peek() == n {
                                    on_tap.call(());
                                }
                            });
                        },
                        ondoubleclick: move |_| {
                            let next = *tap_gen.peek() + 1;
                            tap_gen.set(next);
                            on_double_tap.call(false);
                        },
                    }
                    div {
                        class: "p-zone",
                        onclick: move |_| {
                            let n = *tap_gen.peek() + 1;
                            tap_gen.set(n);
                            spawn(async move {
                                let _ = dioxus::document::eval("await new Promise(r => setTimeout(r, 220)); return 0;")
                                    .join::<i32>()
                                    .await;
                                if *tap_gen.peek() == n {
                                    on_tap.call(());
                                }
                            });
                        },
                        ondoubleclick: move |_| {
                            let next = *tap_gen.peek() + 1;
                            tap_gen.set(next);
                            on_double_tap.call(true);
                        },
                    }
                }

                if let Some((forward, secs)) = s.flash {
                    div { class: if forward { "p-flash right" } else { "p-flash left" },
                        if forward { "{secs} с ›››" } else { "‹‹‹ {secs} с" }
                    }
                }

                div {
                    class: if window_chrome.is_some() { "p-top has-win-chrome" } else { "p-top" },
                    onpointerdown: move |_| ping_activity(),
                    onmousedown: move |e| {
                        if let Some(chrome) = &window_chrome {
                            if e.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary) {
                                chrome.on_drag.call(());
                            }
                        }
                    },
                    div { class: "p-top-l",
                        button {
                            class: "p-glass-btn icon",
                            "aria-label": "Назад",
                            onmounted: move |e| tip.mount(0, e.data()),
                            onmouseenter: move |_| tip.show(0, "Назад"),
                            onmouseleave: move |_| tip.hide(0),
                            onmousedown: move |e| {
                                tip.hide(0);
                                e.stop_propagation();
                            },
                            onclick: move |_| on_close.call(()),
                            Icon { svg: icons::BACK, class: "ic" }
                        }
                        button {
                            class: "p-title-link",
                            onmounted: move |e| tip.mount(1, e.data()),
                            onmouseenter: move |_| tip.show(1, "Открыть страницу тайтла"),
                            onmouseleave: move |_| tip.hide(1),
                            onmousedown: move |e| {
                                tip.hide(1);
                                e.stop_propagation();
                            },
                            onclick: move |_| {
                                if let Some(cb) = on_title {
                                    cb.call(());
                                } else {
                                    on_close.call(());
                                }
                            },
                            div { class: "p-title-t",
                                span { "{s.title}" }
                                Icon { svg: icons::CHEV_R, class: "ic sm muted" }
                            }
                            div { class: "p-title-s",
                                if let Some(sub) = &s.subtitle { span { "{sub} · " } }
                                span { "{s.release}" }
                                if let Some(rw) = &s.rewatch_label { span { class: "tag acc", "{rw}" } }
                                if s.preloaded { span { class: "tag srv", "Скачан" } }
                                if s.transcoded { span { class: "tag warn", "перекодируется" } }
                            }
                        }
                    }
                    div {
                        class: "p-top-r",
                        onmousedown: move |e| e.stop_propagation(),
                        button {
                            class: "p-glass-btn icon",
                            "aria-label": "Заблокировать экран",
                            onmounted: move |e| tip.mount(2, e.data()),
                            onmouseenter: move |_| tip.show(2, "Заблокировать экран от случайных нажатий"),
                            onmouseleave: move |_| tip.hide(2),
                            onmousedown: move |_| tip.hide(2),
                            onclick: move |_| {
                                on_menu.call(None);
                                locked.set(true);
                                if let Some(cb) = on_lock {
                                    cb.call(true);
                                }
                                show_lock_hint();
                            },
                            Icon { svg: icons::LOCK, class: "ic sm" }
                        }
                        if s.sync_members.len() > 1 {
                            {
                                let sync_count = s.sync_members.len();
                                let sync_wait = !s.sync_waiting_for.is_empty();
                                let sync_title = if sync_wait {
                                    format!("Синхронизация ({sync_count} устр.) · ждём: {}", s.sync_waiting_for.join(", "))
                                } else {
                                    format!("Синхронизация активна ({sync_count} устр.)")
                                };
                                let sync_tip = sync_title.clone();
                                rsx! {
                                    button {
                                        class: if s.menu == Some(PlayerMenu::Sync) {
                                            "p-glass-btn icon p-sync-btn active"
                                        } else if sync_wait {
                                            "p-glass-btn icon p-sync-btn wait"
                                        } else {
                                            "p-glass-btn icon p-sync-btn"
                                        },
                                        "aria-label": "{sync_title}",
                                        onmounted: move |e| tip.mount(3, e.data()),
                                        onmouseenter: move |_| tip.show(3, sync_tip.clone()),
                                        onmouseleave: move |_| tip.hide(3),
                                        onmousedown: move |_| tip.hide(3),
                                        onclick: move |_| on_menu.call(if s.menu == Some(PlayerMenu::Sync) { None } else { Some(PlayerMenu::Sync) }),
                                        span { class: "p-sync-dot" }
                                    }
                                }
                            }
                        }
                    }
                }

                if busy {
                    div { class: "p-center",
                        if let Some(pr) = &s.progress {
                            ConnectProgress { progress: pr.clone() }
                        } else {
                            div { class: "p-wait",
                                div { class: "spinner" }
                                p { "{s.status.as_deref().unwrap_or(\"Готовим видео…\")}" }
                            }
                        }
                    }
                } else if (s.paused || s.touch_controls) && s.menu.is_none() && s.error.is_none() {
                    // On touch devices, tapping reveals the center Pause/Play button alongside the controls
                    // (hidden via CSS when controls fade out); desktop shows it only while paused.
                    button {
                        class: "p-center p-big",
                        "aria-label": if s.paused { "Продолжить воспроизведение" } else { "Пауза" },
                        onmounted: move |e| tip.mount(16, e.data()),
                        onmouseenter: move |_| tip.show(16, if s.paused { "Продолжить воспроизведение" } else { "Пауза" }),
                        onmouseleave: move |_| tip.hide(16),
                        onmousedown: move |_| tip.hide(16),
                        onpointerdown: move |_| ping_activity(),
                        onclick: move |_| on_toggle.call(()),
                        Icon { svg: if s.paused { icons::PLAY } else { icons::PAUSE }, class: "ic xl" }
                    }
                }

                if let Some(err) = &s.error {
                    div { class: "p-center p-error",
                        p { "{err}" }
                        div { class: "p-err-acts",
                            button { class: "btn primary sm", onclick: move |_| on_pick_release.call(()),
                                Icon { svg: icons::SWAP, class: "ic sm" }
                                "Выбрать другую раздачу"
                            }
                            if let (Some((ns, ne)), Some(cb)) = (s.next_available, on_jump_episode) {
                                button {
                                    class: "btn glass sm",
                                    onclick: move |_| cb.call((ns, ne)),
                                    Icon { svg: icons::PLAY, class: "ic sm" }
                                    "Перейти к {episode_code(ns, ne)}"
                                }
                            }
                            button { class: "btn glass sm", onclick: move |_| on_close.call(()), "Закрыть" }
                        }
                    }
                }

                if s.auto_muted && !s.paused && s.error.is_none() {
                    button {
                        class: "p-glass-btn p-unmute-btn",
                        "aria-label": "Включить звук",
                        onmounted: move |e| tip.mount(4, e.data()),
                        onmouseenter: move |_| tip.show(4, "Нажмите, чтобы включить звук"),
                        onmouseleave: move |_| tip.hide(4),
                        onmousedown: move |_| tip.hide(4),
                        onclick: move |_| {
                            if let Some(cb) = on_unmute {
                                cb.call(());
                            }
                        },
                        Icon { svg: icons::VOL_MUTE, class: "ic sm" }
                        "Включить звук"
                    }
                }

                if show_disk_prompt && !locked() {
                    div { class: "p-next-card p-disk-card",
                        div { class: "p-next-info",
                            div { class: "p-next-sub", "{disk_ready_title}" }
                            div { class: "p-next-title", "Хотите сразу его воспроизвести?" }
                        }
                        button {
                            class: "btn primary sm",
                            onclick: move |_| {
                                if let Some(cb) = on_switch_to_disk {
                                    cb.call(());
                                }
                            },
                            Icon { svg: icons::PLAY, class: "ic sm" }
                            "Да"
                        }
                        button {
                            class: "p-glass-btn icon p-next-close",
                            "aria-label": "Позже",
                            onmounted: move |e| tip.mount(5, e.data()),
                            onmouseenter: move |_| tip.show(5, "Позже"),
                            onmouseleave: move |_| tip.hide(5),
                            onmousedown: move |_| tip.hide(5),
                            onclick: move |_| {
                                if let Some(cb) = on_dismiss_disk_prompt {
                                    cb.call(());
                                }
                            },
                            Icon { svg: icons::CLOSE, class: "ic sm" }
                        }
                    }
                }

                if let Some(skip) = active_skip {
                    {
                        let target_sec = skip.end_sec.min((dur - 1.0).max(0.0));
                        let cls = if show_next { "p-glass-btn p-skip-btn with-next" } else { "p-glass-btn p-skip-btn" };
                        let label = skip.kind.skip_label();
                        let skip_tip = format!("{label} (до {})", clock(target_sec));
                        rsx! {
                            button {
                                class: "{cls}",
                                onmounted: move |e| tip.mount(6, e.data()),
                                onmouseenter: move |_| tip.show(6, skip_tip.clone()),
                                onmouseleave: move |_| tip.hide(6),
                                onmousedown: move |_| tip.hide(6),
                                onclick: move |_| on_seek.call(target_sec),
                                Icon { svg: icons::FWD10, class: "ic sm" }
                                span { "{label}" }
                            }
                        }
                    }
                }

                if show_rate {
                    {
                        let rate_cls = if show_next {
                            "p-next-card p-rate-card with-next"
                        } else {
                            "p-next-card p-rate-card"
                        };
                        let hovered_star = rate_hover();
                        let display_star = hovered_star.or(s.my_rating).unwrap_or(0);
                        let sub_text = if let Some(h) = hovered_star {
                            format!("{h}★ — {}", rating_description(h))
                        } else if let Some(r) = s.my_rating {
                            format!("Оценка {r}★ — {}", rating_description(r))
                        } else if s.season > 0 {
                            "Как вам серия?".to_owned()
                        } else {
                            "Как вам фильм?".to_owned()
                        };
                        let pop_state = rate_popped();
                        rsx! {
                            div {
                                class: "{rate_cls}",
                                onmouseleave: move |_| rate_hover.set(None),
                                div { class: "p-next-info",
                                    div { class: "p-next-sub", "{sub_text}" }
                                    div {
                                        class: "p-rate-stars",
                                        role: "radiogroup",
                                        "aria-label": "Оценка",
                                        for n in 1..=5i16 {
                                            {
                                                let is_on = n <= display_star;
                                                let is_popped = pop_state.is_some_and(|(pn, _)| pn == n) && s.my_rating == Some(n);
                                                let cls = match (is_on, is_popped) {
                                                    (true, true) => "p-rate-star on pop",
                                                    (true, false) => "p-rate-star on",
                                                    _ => "p-rate-star",
                                                };
                                                let star_key = if is_popped {
                                                    format!("p-star-{n}-{}", pop_state.map(|(_, t)| t).unwrap_or(0))
                                                } else {
                                                    format!("p-star-{n}")
                                                };
                                                let desc = rating_description(n);
                                                rsx! {
                                                    button {
                                                        key: "{star_key}",
                                                        class: "{cls}",
                                                        role: "radio",
                                                        "aria-checked": if s.my_rating == Some(n) { "true" } else { "false" },
                                                        "aria-label": "{n} из 5: {desc}",
                                                        onmouseenter: move |_| rate_hover.set(Some(n)),
                                                        onclick: move |_| {
                                                            rate_hover.set(None);
                                                            let t = rate_tick() + 1;
                                                            rate_tick.set(t);
                                                            rate_popped.set(Some((n, t)));
                                                            rate_just_saved.set(Some(rate_ep_key));
                                                            if let Some(cb) = on_rate {
                                                                cb.call(Some(n));
                                                            }
                                                            spawn(async move {
                                                                let _ = dioxus::document::eval(
                                                                    "await new Promise(r => setTimeout(r, 2200)); return 0;",
                                                                )
                                                                .join::<i32>()
                                                                .await;
                                                                if *rate_tick.peek() == t {
                                                                    rate_just_saved.set(None);
                                                                }
                                                            });
                                                        },
                                                        Icon { svg: icons::STAR, class: "ic" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                button {
                                    class: "p-glass-btn icon p-next-close",
                                    "aria-label": "Скрыть",
                                    onmounted: move |e| tip.mount(17, e.data()),
                                    onmouseenter: move |_| tip.show(17, "Скрыть"),
                                    onmouseleave: move |_| tip.hide(17),
                                    onmousedown: move |_| tip.hide(17),
                                    onclick: move |_| {
                                        tip.hide(17);
                                        rate_just_saved.set(None);
                                        dismissed_rate.set(Some(rate_ep_key));
                                    },
                                    Icon { svg: icons::CLOSE, class: "ic sm" }
                                }
                            }
                        }
                    }
                }

                if let Some(next) = s.next.as_deref().filter(|_| show_next) {
                    div { class: "p-next-card",
                        div { class: "p-next-info",
                            div { class: "p-next-sub", "Следующая через {clock(remaining)}" }
                            div { class: "p-next-title", "{next}" }
                        }
                        button { class: "btn primary sm", onclick: move |_| on_next.call(()),
                            Icon { svg: icons::PLAY, class: "ic sm" }
                            "Смотреть"
                        }
                        button {
                            class: "p-glass-btn icon p-next-close",
                            "aria-label": "Скрыть",
                            onmounted: move |e| tip.mount(7, e.data()),
                            onmouseenter: move |_| tip.show(7, "Скрыть"),
                            onmouseleave: move |_| tip.hide(7),
                            onmousedown: move |_| tip.hide(7),
                            onclick: move |_| dismissed_next.set(next_for_dismiss.clone()),
                            Icon { svg: icons::CLOSE, class: "ic sm" }
                        }
                    }
                }

                div { class: "p-dock", onpointerdown: move |_| ping_activity(),
                    div { class: "p-seek-row",
                        span { class: "p-time", "{clock(display_pos)}" }
                        div {
                            class: "p-track",
                            onmounted: move |e| track_el.set(Some(e.data())),
                            onpointerdown: move |e| {
                                if dur <= 0.0 {
                                    return;
                                }
                                let Some(el) = track_el() else { return };
                                let c = e.client_coordinates();
                                let (cx, cy) = (c.x, c.y);
                                let my_gen = *drag_gen.peek() + 1;
                                drag_gen.set(my_gen);
                                drag_active.set(true);
                                drag_end_pos.set(None);
                                drag_rect.set(None);
                                let seek_dur = dur;
                                spawn(async move {
                                    let kind = dioxus::document::eval(
                                        "return await new Promise(function(resolve) {
                                           var done = false;
                                           var finish = function(k) {
                                             if (done) return;
                                             done = true;
                                             window.removeEventListener('pointerup', onUp);
                                             window.removeEventListener('blur', onBlur);
                                             resolve(k);
                                           };
                                           var onUp = function() { setTimeout(function() { finish('up'); }, 0); };
                                           var onBlur = function() { finish('blur'); };
                                           window.addEventListener('pointerup', onUp);
                                           window.addEventListener('blur', onBlur);
                                         });",
                                    )
                                    .join::<String>()
                                    .await
                                    .unwrap_or_default();
                                    if *drag_gen.peek() == my_gen
                                        && (*drag_active.peek() || drag_rect.peek().is_some())
                                    {
                                        drag_gen.set(my_gen + 1);
                                        drag_active.set(false);
                                        drag_end_pos.set(None);
                                        drag_rect.set(None);
                                        let v = scrub.take();
                                        if kind == "up" {
                                            if let Some(pos) = v {
                                                on_seek.call(pos);
                                            }
                                        }
                                    }
                                });
                                spawn(async move {
                                    if let Ok(rect) = el.get_client_rect().await {
                                        if *drag_gen.peek() != my_gen {
                                            return;
                                        }
                                        let vert = rect.height() > rect.width();
                                        let (start, len) = if vert {
                                            (rect.min_y(), rect.height())
                                        } else {
                                            (rect.min_x(), rect.width())
                                        };
                                        if len > 1.0 {
                                            if *drag_active.peek() {
                                                drag_rect.set(Some((vert, start, len)));
                                                let coord = if vert { cy } else { cx };
                                                let frac = ((coord - start) / len).clamp(0.0, 1.0);
                                                scrub.set(Some(frac * seek_dur));
                                            } else if let Some((end_x, end_y)) = drag_end_pos.take() {
                                                let coord = if vert { end_y } else { end_x };
                                                let frac = ((coord - start) / len).clamp(0.0, 1.0);
                                                scrub.set(None);
                                                on_seek.call(frac * seek_dur);
                                            }
                                        } else {
                                            let was_released = !*drag_active.peek();
                                            drag_active.set(false);
                                            drag_end_pos.set(None);
                                            if was_released {
                                                scrub.set(None);
                                            }
                                        }
                                    } else if *drag_gen.peek() == my_gen {
                                        let was_released = !*drag_active.peek();
                                        drag_active.set(false);
                                        drag_end_pos.set(None);
                                        if was_released {
                                            scrub.set(None);
                                        }
                                    }
                                });
                            },
                            if dur > 0.0 {
                                for seg in &s.segments {
                                    if matches!(seg.kind, SegmentKind::Intro | SegmentKind::Recap | SegmentKind::Outro)
                                        && seg.end_sec > seg.start_sec
                                        && seg.start_sec < dur
                                    {
                                        {
                                            let left = (seg.start_sec / dur * 100.0).clamp(0.0, 100.0);
                                            let width = ((seg.end_sec.min(dur) - seg.start_sec.max(0.0)) / dur * 100.0)
                                                .clamp(0.4, 100.0 - left);
                                            rsx! { div { class: "p-seg", style: "left: {left:.2}%; width: {width:.2}%" } }
                                        }
                                    }
                                }
                            }
                            div { class: "p-fill", style: "width: {pct:.2}%" }
                            if scrub().is_some() {
                                div { class: "p-tip", style: "left: {pct:.2}%", "{clock(display_pos)}" }
                            }
                            input {
                                r#type: "range",
                                min: "0",
                                max: "{dur.max(1.0)}",
                                step: "1",
                                value: "{display_pos.floor()}",
                                disabled: dur <= 0.0,
                                "aria-label": "Перемотка",
                                oninput: move |e| {
                                    if let Ok(v) = e.value().parse::<f64>() {
                                        scrub.set(Some(v));
                                        ping_activity();
                                    }
                                },
                                onchange: move |e| {
                                    scrub.set(None);
                                    if let Ok(v) = e.value().parse::<f64>() {
                                        on_seek.call(v);
                                    }
                                },
                                onpointercancel: move |_| {
                                    scrub.set(None);
                                },
                                onblur: move |_| {
                                    scrub.set(None);
                                },
                            }
                        }
                        span { class: "p-time r", if dur > 0.0 { "−{clock(remaining)}" } else { "--:--" } }
                    }
                    div { class: "p-ctrl-row",
                        button {
                            class: "p-glass-btn icon",
                            "aria-label": "Назад на 10 секунд",
                            onmounted: move |e| tip.mount(8, e.data()),
                            onmouseenter: move |_| tip.show(8, "Назад на 10 секунд"),
                            onmouseleave: move |_| tip.hide(8),
                            onmousedown: move |_| tip.hide(8),
                            onclick: move |_| on_seek.call((pos - 10.0).max(0.0)),
                            Icon { svg: icons::BACK10, class: "ic" }
                        }
                        button {
                            class: "p-play-btn",
                            "aria-label": if s.paused { "Смотреть" } else { "Пауза" },
                            onmounted: move |e| tip.mount(9, e.data()),
                            onmouseenter: move |_| tip.show(9, if s.paused { "Смотреть" } else { "Пауза" }),
                            onmouseleave: move |_| tip.hide(9),
                            onmousedown: move |_| tip.hide(9),
                            onclick: move |_| on_toggle.call(()),
                            Icon { svg: if s.paused { icons::PLAY } else { icons::PAUSE }, class: "ic" }
                        }
                        button {
                            class: "p-glass-btn icon",
                            "aria-label": "Вперёд на 10 секунд",
                            onmounted: move |e| tip.mount(10, e.data()),
                            onmouseenter: move |_| tip.show(10, "Вперёд на 10 секунд"),
                            onmouseleave: move |_| tip.hide(10),
                            onmousedown: move |_| tip.hide(10),
                            onclick: move |_| on_seek.call(pos + 10.0),
                            Icon { svg: icons::FWD10, class: "ic" }
                        }
                        if s.show_volume {
                            {
                                let vol_tip = if sync_muted {
                                    "Звук на другом устройстве — нажмите, чтобы включить здесь"
                                } else if effective_muted || s.volume <= 0.001 {
                                    "Включить звук"
                                } else {
                                    "Выключить звук"
                                };
                                rsx! {
                                    div { class: "p-vol desktop-only",
                                        button {
                                            class: "p-vol-btn",
                                            "aria-label": "Громкость",
                                            onmounted: move |e| tip.mount(11, e.data()),
                                            onmouseenter: move |_| tip.show(11, vol_tip),
                                            onmouseleave: move |_| tip.hide(11),
                                            onmousedown: move |_| tip.hide(11),
                                            onclick: move |_| {
                                                if let Some(cb) = on_mute {
                                                    cb.call(());
                                                }
                                            },
                                            Icon { svg: if effective_muted || s.volume <= 0.001 { icons::VOL_MUTE } else { icons::VOL }, class: "ic sm" }
                                        }
                                        div { class: "p-vol-track",
                                            div { class: "p-vol-fill", style: "width: {vol_pct:.0}%" }
                                            input {
                                                r#type: "range",
                                                min: "0",
                                                max: "100",
                                                step: "2",
                                                value: "{vol_pct.round() as i32}",
                                                "aria-label": "Уровень громкости",
                                                oninput: move |e| {
                                                    if let (Some(cb), Ok(v)) = (on_volume, e.value().parse::<f32>()) {
                                                        cb.call((v / 100.0).clamp(0.0, 1.0));
                                                    }
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "p-spacer" }
                        if !s.episodes.is_empty() {
                            button {
                                class: if s.menu == Some(PlayerMenu::Episodes) { "p-glass-btn p-ep-btn active" } else { "p-glass-btn p-ep-btn" },
                                "aria-label": "Серии",
                                onmounted: move |e| tip.mount(12, e.data()),
                                onmouseenter: move |_| tip.show(12, "Список серий"),
                                onmouseleave: move |_| tip.hide(12),
                                onmousedown: move |_| tip.hide(12),
                                onclick: move |_| on_menu.call(if s.menu == Some(PlayerMenu::Episodes) { None } else { Some(PlayerMenu::Episodes) }),
                                Icon { svg: icons::LIST, class: "ic sm" }
                                span { class: "long", "Серии" }
                            }
                        }
                        // Button 1: Headphones (Audio + Subtitles).
                        button {
                            class: if headphones_open { "p-glass-btn audio-btn active" } else { "p-glass-btn audio-btn" },
                            "aria-label": "Наушники: звук и субтитры",
                            onmounted: move |e| tip.mount(13, e.data()),
                            onmouseenter: move |_| tip.show(13, "Наушники: звук и субтитры"),
                            onmouseleave: move |_| tip.hide(13),
                            onmousedown: move |_| tip.hide(13),
                            onclick: move |_| on_menu.call(if headphones_open { None } else { Some(PlayerMenu::Audio) }),
                            Icon { svg: icons::AUDIO_CC, class: "ic sm" }
                            span { class: "long", "Наушники" }
                        }
                        // Button 2: Settings (Speed + Scale + Quality).
                        button {
                            class: if settings_open { "p-glass-btn settings-btn active" } else { "p-glass-btn settings-btn" },
                            "aria-label": "Настройки",
                            onmounted: move |e| tip.mount(14, e.data()),
                            onmouseenter: move |_| tip.show(14, "Настройки: скорость, масштаб и качество"),
                            onmouseleave: move |_| tip.hide(14),
                            onmousedown: move |_| tip.hide(14),
                            onclick: move |_| on_menu.call(if settings_open { None } else { Some(PlayerMenu::Speed) }),
                            Icon { svg: icons::SETTINGS, class: "ic sm" }
                            span { class: "long", "Настройки" }
                        }
                        if s.show_fullscreen {
                            {
                                let fs_tip = if s.fullscreen {
                                    "Выйти из полноэкранного режима"
                                } else {
                                    "Во весь экран"
                                };
                                rsx! {
                                    button {
                                        class: if s.fullscreen { "p-glass-btn icon p-fs-btn active" } else { "p-glass-btn icon p-fs-btn" },
                                        "aria-label": "{fs_tip}",
                                        onmounted: move |e| tip.mount(15, e.data()),
                                        onmouseenter: move |_| tip.show(15, fs_tip),
                                        onmouseleave: move |_| tip.hide(15),
                                        onmousedown: move |_| tip.hide(15),
                                        onclick: move |_| on_fullscreen.call(()),
                                        Icon { svg: if s.fullscreen { icons::EXIT_FULL } else { icons::FULL }, class: "ic sm" }
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(menu) = s.menu {
                    div {
                        class: match menu {
                            PlayerMenu::Sync => "p-popover sync",
                            _ => "p-popover",
                        },
                        onscroll: move |_| clear_active_tooltip(),
                        onpointerdown: move |_| ping_activity(),
                        if matches!(menu, PlayerMenu::Audio | PlayerMenu::Subtitles) {
                            div { class: "p-pop-tabs",
                                button {
                                    class: if menu == PlayerMenu::Audio { "active" } else { "" },
                                    onclick: move |_| on_menu.call(Some(PlayerMenu::Audio)),
                                    "Звук ({s.audio.len()})"
                                }
                                button {
                                    class: if menu == PlayerMenu::Subtitles { "active" } else { "" },
                                    onclick: move |_| on_menu.call(Some(PlayerMenu::Subtitles)),
                                    if s.subtitle_track.is_some() {
                                        "Субтитры (Вкл)"
                                    } else {
                                        "Субтитры ({s.subtitle_tracks.len()})"
                                    }
                                }
                            }
                            if menu == PlayerMenu::Audio {
                                if s.audio.is_empty() {
                                    p { class: "muted small p-pop-note", "Сервер не смог прочитать звуковые дорожки файла." }
                                }
                                for t in s.audio.clone() {
                                    button {
                                        key: "a{t.index}",
                                        class: if s.audio_track == Some(t.index) { "p-opt on" } else { "p-opt" },
                                        onclick: move |_| on_audio.call(t.index),
                                        span { class: "p-opt-l",
                                            if s.audio_track == Some(t.index) {
                                                Icon { svg: icons::CHECK, class: "ic xs acc" }
                                            }
                                            span { "{t.label()}" }
                                        }
                                        span { class: "tag", "{t.codec.to_uppercase()}" }
                                    }
                                }
                            } else {
                                button {
                                    class: if s.subtitle_track.is_none() { "p-opt on" } else { "p-opt" },
                                    onclick: move |_| on_subtitle.call(None),
                                    span { class: "p-opt-l",
                                        if s.subtitle_track.is_none() {
                                            Icon { svg: icons::CHECK, class: "ic xs acc" }
                                        }
                                        span { "Выключены" }
                                    }
                                }
                                if !s.preloaded && !s.native_video && !s.subtitle_tracks.is_empty() {
                                    p { class: "muted small p-pop-note",
                                        "Встроенные субтитры отключены при потоке с торрента, чтобы не тормозить загрузку видео."
                                    }
                                }
                                for t in s.subtitle_tracks.clone() {
                                    button {
                                        key: "s{t.index}",
                                        class: if s.subtitle_track == Some(t.index) { "p-opt on" } else { "p-opt" },
                                        disabled: !s.preloaded && !s.native_video,
                                        onclick: move |_| on_subtitle.call(Some(t.index)),
                                        span { class: "p-opt-l",
                                            if s.subtitle_track == Some(t.index) {
                                                Icon { svg: icons::CHECK, class: "ic xs acc" }
                                            }
                                            span { "{t.label()}" }
                                        }
                                    }
                                }
                            }
                            p { class: "muted small p-pop-note", "Выбор дорожки и субтитров запоминается для этого тайтла." }
                        } else if matches!(menu, PlayerMenu::Quality | PlayerMenu::Speed | PlayerMenu::Scale) {
                            div { class: "p-pop-tabs",
                                button {
                                    class: if menu == PlayerMenu::Speed { "active" } else { "" },
                                    onclick: move |_| on_menu.call(Some(PlayerMenu::Speed)),
                                    "Скорость"
                                }
                                button {
                                    class: if menu == PlayerMenu::Scale { "active" } else { "" },
                                    onclick: move |_| on_menu.call(Some(PlayerMenu::Scale)),
                                    "Масштаб"
                                }
                                if on_pick_quality.is_some() {
                                    button {
                                        class: if menu == PlayerMenu::Quality { "active" } else { "" },
                                        onclick: move |_| on_menu.call(Some(PlayerMenu::Quality)),
                                        "Качество"
                                    }
                                }
                            }
                            if menu == PlayerMenu::Quality {
                                {
                                    let mut unified: Vec<ReleaseOption> = s.available_qualities.clone();
                                    for p in &s.release_downloads {
                                        if matches!(
                                            p.state,
                                            PreloadState::Queued | PreloadState::Downloading | PreloadState::Ready
                                        ) && !unified.iter().any(|r| r.infohash.eq_ignore_ascii_case(&p.infohash))
                                        {
                                            unified.push(ReleaseOption {
                                                infohash: p.infohash.clone(),
                                                title: p.title.clone().unwrap_or_default(),
                                                quality: p.quality.clone().unwrap_or_else(|| "1080p".to_owned()),
                                                size_bytes: p.size_bytes.unwrap_or(0).max(0) as u64,
                                                seeders: 0,
                                                voice: p.voice.clone().unwrap_or_default(),
                                                language: String::new(),
                                                tracker: String::new(),
                                                added: String::new(),
                                                year: None,
                                                season: (p.season > 0).then_some(p.season),
                                                episode_start: None,
                                                episode_end: None,
                                                hdr: false,
                                                dolby_vision: false,
                                                subtitles: false,
                                                direct_play: true,
                                                codec: None,
                                                bitrate_kbps: None,
                                                bitrate_approx: false,
                                                picked_by: Vec::new(),
                                                preloaded: p.state == PreloadState::Ready,
                                                collection: false,
                                            });
                                        }
                                    }
                                    if let Some(cur_hash) = s.active_infohash.as_deref().filter(|h| !h.is_empty()) {
                                        if !unified.iter().any(|r| r.infohash.eq_ignore_ascii_case(cur_hash)) {
                                            unified.push(ReleaseOption {
                                                infohash: cur_hash.to_owned(),
                                                // Leave synthetic title empty so POST /api/preload resolves the true torrent name.
                                                title: String::new(),
                                                quality: quality_from_release_label(&s.release),
                                                size_bytes: 0,
                                                seeders: 0,
                                                voice: voice_from_release_label(&s.release),
                                                language: String::new(),
                                                tracker: String::new(),
                                                added: String::new(),
                                                year: None,
                                                season: (s.season > 0).then_some(s.season),
                                                episode_start: None,
                                                episode_end: None,
                                                hdr: false,
                                                dolby_vision: false,
                                                subtitles: false,
                                                direct_play: true,
                                                codec: None,
                                                bitrate_kbps: None,
                                                bitrate_approx: false,
                                                picked_by: Vec::new(),
                                                preloaded: s.preloaded,
                                                collection: false,
                                            });
                                        }
                                    }
                                    unified.sort_by_key(|r| {
                                        let is_cur = s
                                            .active_infohash
                                            .as_deref()
                                            .is_some_and(|h| h.eq_ignore_ascii_case(&r.infohash));
                                        let dl = s.download_item(&r.infohash);
                                        let is_ready = dl.is_some_and(|p| p.state == PreloadState::Ready)
                                            || r.preloaded
                                            || (is_cur && s.preloaded);
                                        let is_busy = dl.is_some_and(|p| {
                                            matches!(p.state, PreloadState::Queued | PreloadState::Downloading)
                                        });
                                        (
                                            std::cmp::Reverse(quality_rank_bucket(&r.quality)),
                                            !is_cur,
                                            !is_ready,
                                            !is_busy,
                                        )
                                    });
                                    let cur_voice = s
                                        .active_infohash
                                        .as_deref()
                                        .and_then(|h| {
                                            unified
                                                .iter()
                                                .find(|r| r.infohash.eq_ignore_ascii_case(h))
                                                .map(|r| r.voice.trim().to_owned())
                                        })
                                        .filter(|v| !v.is_empty())
                                        .unwrap_or_else(|| voice_from_release_label(&s.release));
                                    rsx! {
                                        if unified.is_empty() {
                                            p { class: "muted small p-pop-note",
                                                if s.qualities_loaded {
                                                    "Других вариантов качества не нашлось."
                                                } else {
                                                    "Загружаем доступные качества…"
                                                }
                                            }
                                        }
                                        for (q_idx, rel) in unified.into_iter().enumerate() {
                                            {
                                                let is_cur = s.active_infohash.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(&rel.infohash));
                                                let qk = quality_short_label(&rel.quality);
                                                let voice_lbl = rel.voice.trim().to_owned();
                                                let other_voice = !cur_voice.is_empty()
                                                    && !voice_lbl.is_empty()
                                                    && !voice_lbl.eq_ignore_ascii_case(&cur_voice);
                                                let dl_item = s.download_item(&rel.infohash);
                                                let is_ready = dl_item.is_some_and(|p| p.state == PreloadState::Ready)
                                                    || rel.preloaded
                                                    || (is_cur && s.preloaded);
                                                let is_busy = dl_item.is_some_and(|p| {
                                                    matches!(p.state, PreloadState::Queued | PreloadState::Downloading)
                                                });
                                                let mut meta_parts: Vec<String> = Vec::new();
                                                if rel.size_bytes > 0 {
                                                    meta_parts.push(human_size(rel.size_bytes));
                                                }
                                                let tracker_lbl = rel.tracker.trim();
                                                let has_tracker = !tracker_lbl.is_empty() && tracker_lbl != "—";
                                                if rel.seeders > 0 || has_tracker {
                                                    meta_parts.push(format!("{} сидов", rel.seeders));
                                                }
                                                if has_tracker {
                                                    meta_parts.push(tracker_lbl.to_owned());
                                                }
                                                let meta_line = meta_parts.join(" · ");
                                                let pick_cnt = rel.picked_by.len();
                                                let rel_click = rel.clone();
                                                let dl_el = render_quality_download_action(QualityDownloadAction {
                                                    rel,
                                                    is_cur,
                                                    playing_preloaded: s.preloaded,
                                                    dl: dl_item,
                                                    on_download: on_download_release,
                                                    on_cancel_download,
                                                    on_switch_to_disk,
                                                    tip,
                                                    slot: 100 + q_idx as u32,
                                                });
                                                let q_st_slot = 300 + q_idx as u32;
                                                let mut row_cls = String::from("p-opt p-q-row");
                                                if is_cur {
                                                    row_cls.push_str(" on");
                                                }
                                                if is_ready || is_busy {
                                                    row_cls.push_str(" dl-hot");
                                                }
                                                rsx! {
                                                    div {
                                                        key: "q-{rel_click.infohash}",
                                                        class: "{row_cls}",
                                                        button {
                                                            class: "p-q-main",
                                                            disabled: is_cur,
                                                            onclick: move |_| {
                                                                if let Some(cb) = on_pick_quality {
                                                                    cb.call(rel_click.clone());
                                                                }
                                                            },
                                                            div { class: "p-q-stack",
                                                                div { class: "p-q-top",
                                                                    if is_cur {
                                                                        Icon { svg: icons::CHECK, class: "ic xs acc" }
                                                                    }
                                                                    strong { "{qk}" }
                                                                    if !voice_lbl.is_empty() {
                                                                        span { class: "p-q-voice", "{voice_lbl}" }
                                                                    }
                                                                    if other_voice {
                                                                        span { class: "tag warn", "Другая озвучка" }
                                                                    }
                                                                    if is_ready {
                                                                        span { class: "tag srv", "Скачан" }
                                                                    } else if let Some(dl) = dl_item.filter(|_| is_busy) {
                                                                        {
                                                                            let st_tip = dl.status_text();
                                                                            rsx! {
                                                                                span {
                                                                                    class: "tag acc",
                                                                                    onmounted: move |e| tip.mount(q_st_slot, e.data()),
                                                                                    onmouseenter: move |_| tip.show(q_st_slot, st_tip.clone()),
                                                                                    onmouseleave: move |_| tip.hide(q_st_slot),
                                                                                    "{dl.compact_status_text()}"
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                    if pick_cnt > 0 {
                                                                        span { class: "tag acc", "👥 {pick_cnt}" }
                                                                    }
                                                                }
                                                                if !meta_line.is_empty() {
                                                                    div { class: "p-q-sub", "{meta_line}" }
                                                                }
                                                            }
                                                        }
                                                        {dl_el}
                                                    }
                                                }
                                            }
                                        }
                                        if let Some(note) = &s.download_note {
                                            p { class: "muted small p-pop-note", "{note}" }
                                        }
                                        div { class: "p-opt-div",
                                            button {
                                                class: "p-opt",
                                                onclick: move |_| on_pick_release.call(()),
                                                span { class: "p-opt-l",
                                                    Icon { svg: icons::SWAP, class: "ic sm acc" }
                                                    span { "Выбрать другую раздачу" }
                                                }
                                                Icon { svg: icons::CHEV_R, class: "ic sm muted" }
                                            }
                                        }
                                        p { class: "muted small p-pop-note", "Выбор качества запоминается для этого устройства." }
                                    }
                                }
                            } else if menu == PlayerMenu::Speed {
                                div { class: "p-speed-grid",
                                    for r in SPEEDS {
                                        button {
                                            key: "r{r}",
                                            class: if (rate - r).abs() < 0.001 { "p-opt on" } else { "p-opt" },
                                            onclick: move |_| on_speed.call(r),
                                            "{speed_label(r)}"
                                        }
                                    }
                                }
                            } else {
                                button {
                                    class: if s.scale == VideoScale::FitWidth { "p-opt on" } else { "p-opt" },
                                    onclick: move |_| {
                                        if s.scale != VideoScale::FitWidth {
                                            on_scale.call(());
                                        }
                                        on_menu.call(None);
                                    },
                                    span { class: "p-opt-l",
                                        Icon { svg: icons::FIT_WIDTH, class: "ic sm" }
                                        span { "По ширине экрана" }
                                    }
                                    if s.scale == VideoScale::FitWidth {
                                        Icon { svg: icons::CHECK, class: "ic xs acc" }
                                    }
                                }
                                button {
                                    class: if s.scale == VideoScale::FitHeight { "p-opt on" } else { "p-opt" },
                                    onclick: move |_| {
                                        if s.scale != VideoScale::FitHeight {
                                            on_scale.call(());
                                        }
                                        on_menu.call(None);
                                    },
                                    span { class: "p-opt-l",
                                        Icon { svg: icons::FIT_HEIGHT, class: "ic sm" }
                                        span { "По высоте экрана" }
                                    }
                                    if s.scale == VideoScale::FitHeight {
                                        Icon { svg: icons::CHECK, class: "ic xs acc" }
                                    }
                                }
                            }
                        } else if menu == PlayerMenu::Sync {
                            h4 { "Синхронизация · {s.sync_members.len()} устр." }
                            p { class: "muted small p-pop-note p-sync-note",
                                "Воспроизведение, пауза, скорость и перемотка синхронизируются. Звук играет только на одном устройстве."
                            }
                            div { class: "p-sync-list",
                                for (m_idx, m) in s.sync_members.clone().into_iter().enumerate() {
                                    {
                                        let is_self = !s.sync_client_id.is_empty() && m.client_id == s.sync_client_id;
                                        let has_sound = s.sync_sound_client_id.as_deref() == Some(m.client_id.as_str())
                                            || (s.sync_sound_client_id.is_none() && is_self);
                                        let dev_lower = m.device.to_lowercase();
                                        let icon_svg = if dev_lower.contains("телефон")
                                            || dev_lower.contains("android")
                                            || dev_lower.contains("iphone")
                                            || dev_lower.contains("telegram")
                                        {
                                            icons::PHONE
                                        } else if dev_lower.contains("тв") || dev_lower.contains("tv") {
                                            icons::TV
                                        } else {
                                            icons::LAPTOP
                                        };
                                        let (status_txt, status_cls) = if !m.ready {
                                            ("Подключается…", "p-sync-badge wait")
                                        } else if m.buffering {
                                            ("Буферизация…", "p-sync-badge wait")
                                        } else if s.paused {
                                            ("На паузе", "p-sync-badge")
                                        } else {
                                            ("Смотрит", "p-sync-badge ok")
                                        };
                                        let sound_label = if is_self { "Звук здесь" } else { "Звук там" };
                                        let sound_title = if has_sound {
                                            format!("Звук сейчас на устройстве «{}»", m.device)
                                        } else if is_self {
                                            "Переключить звук на это устройство".to_owned()
                                        } else {
                                            format!("Переключить звук на «{}»", m.device)
                                        };
                                        let sound_tip = sound_title.clone();
                                        let sync_slot = 500 + m_idx as u32;
                                        let target_cid = m.client_id.clone();
                                        rsx! {
                                            div {
                                                key: "{m.client_id}",
                                                class: if is_self { "p-sync-row self" } else { "p-sync-row" },
                                                div { class: "p-sync-head",
                                                    Icon { svg: icon_svg, class: "ic sm muted" }
                                                    div { class: "p-sync-meta",
                                                        div { class: "p-sync-name", "{m.device}" }
                                                        if is_self {
                                                            div { class: "p-sync-sub", "Это устройство" }
                                                        }
                                                    }
                                                    span { class: "{status_cls}", "{status_txt}" }
                                                }
                                                button {
                                                    class: if has_sound { "p-sync-sound on" } else { "p-sync-sound" },
                                                    "aria-label": "{sound_title}",
                                                    "aria-pressed": if has_sound { "true" } else { "false" },
                                                    onmounted: move |e| tip.mount(sync_slot, e.data()),
                                                    onmouseenter: move |_| tip.show(sync_slot, sound_tip.clone()),
                                                    onmouseleave: move |_| tip.hide(sync_slot),
                                                    onmousedown: move |_| tip.hide(sync_slot),
                                                    onclick: move |_| {
                                                        if let Some(cb) = on_sync_sound {
                                                            cb.call(target_cid.clone());
                                                        }
                                                    },
                                                    Icon { svg: if has_sound { icons::VOL } else { icons::VOL_MUTE }, class: "ic xs" }
                                                    span { "{sound_label}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            h4 { "Серии · сезон {s.season}" }
                            for (e_idx, e) in s.episodes.clone().into_iter().enumerate() {
                                {
                                    let ep_slot = 700 + e_idx as u32;
                                    rsx! {
                                        button {
                                            key: "e{e.number}",
                                            class: if e.number == s.episode { "p-opt ep on" } else { "p-opt ep" },
                                            onclick: move |_| on_episode.call(e.number),
                                            span { "{e.number}. {e.title}" }
                                            if e.watched {
                                                span {
                                                    class: "p-seen done",
                                                    onmounted: move |ev| tip.mount(ep_slot, ev.data()),
                                                    onmouseenter: move |_| tip.show(ep_slot, "Просмотрена"),
                                                    onmouseleave: move |_| tip.hide(ep_slot),
                                                    Icon { svg: icons::CHECK, class: "ic xs" }
                                                }
                                            } else {
                                                span {
                                                    class: "p-seen",
                                                    onmounted: move |ev| tip.mount(ep_slot, ev.data()),
                                                    onmouseenter: move |_| tip.show(ep_slot, "Не просмотрена"),
                                                    onmouseleave: move |_| tip.hide(ep_slot),
                                                    "○"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            TooltipLayer { host: tip_host }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{controls_shown, pointer_moved, PlayerMenu, PlayerState, PreloadState, VideoScale};

    #[test]
    fn paused_keeps_controls_visible() {
        let mut s = PlayerState {
            paused: true,
            controls: false,
            ..PlayerState::default()
        };
        assert!(controls_shown(false, &s, false));
        assert!(!controls_shown(true, &s, false));

        s.paused = false;
        assert!(!controls_shown(false, &s, false));
        assert!(controls_shown(false, &s, true));

        s.menu = Some(PlayerMenu::Audio);
        assert!(controls_shown(false, &s, false));
        assert!(!controls_shown(true, &s, false));
    }

    #[test]
    fn pointer_moved_ignores_subpixel_jitter() {
        assert!(pointer_moved(None, (10.0, 20.0)));
        assert!(!pointer_moved(Some((10.0, 20.0)), (10.4, 20.5)));
        assert!(pointer_moved(Some((10.0, 20.0)), (11.1, 20.0)));
    }

    #[test]
    fn video_scale_toggles_and_round_trips() {
        assert_eq!(VideoScale::FitWidth.toggle(), VideoScale::FitHeight);
        assert_eq!(VideoScale::FitHeight.toggle(), VideoScale::FitWidth);
        for mode in [VideoScale::FitWidth, VideoScale::FitHeight] {
            assert_eq!(VideoScale::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(VideoScale::parse("fill"), None);
    }

    #[test]
    fn sync_muted_reflects_sound_owner() {
        let member = |cid: &str| kino_core::SyncMemberInfo {
            client_id: cid.into(),
            device: cid.into(),
            ready: true,
            buffering: false,
        };
        let mut s = PlayerState {
            sync_client_id: "a".into(),
            sync_members: vec![member("a")],
            sync_sound_client_id: Some("b".into()),
            ..PlayerState::default()
        };
        // Solo playback is never sync-muted.
        assert!(!s.is_sync_muted());

        s.sync_members.push(member("b"));
        // Two devices, sound belongs to "b" -> "a" is sync-muted.
        assert!(s.is_sync_muted());

        // Switching sound to "a" unmutes "a".
        s.sync_sound_client_id = Some("a".into());
        assert!(!s.is_sync_muted());

        // Missing owner does not falsely mute.
        s.sync_sound_client_id = None;
        assert!(!s.is_sync_muted());
    }

    #[test]
    fn download_status_matches_case_insensitively() {
        let mut s = PlayerState::default();
        let hash = "AbCd".repeat(10);
        s.release_downloads.push(kino_core::PreloadItem {
            id: 7,
            kind: kino_core::MediaKind::Movie,
            tmdb_id: 603,
            season: 0,
            infohash: hash.to_lowercase(),
            title: Some("Матрица WEB-DL 1080p".into()),
            state: PreloadState::Downloading,
            progress: 0.42,
            size_bytes: Some(8_000_000_000),
            requested_at: chrono::Utc::now(),
            ready_at: None,
            delete_after: None,
            watched: false,
            poster_path: None,
            media_title: Some("Матрица".into()),
            quality: Some("1080p".into()),
            voice: Some("Дубляж".into()),
            speed_bps: 0,
            dl_phase: kino_core::PreloadPhase::Downloading,
            whole_torrent: true,
            episodes: Vec::new(),
            requested_by: None,
            can_delete: true,
        });
        assert_eq!(s.download_status(&hash), Some((PreloadState::Downloading, 0.42)));
        assert_eq!(s.download_status(&"f".repeat(40)), None);

        let prompt = super::DiskReadyPrompt {
            infohash: hash,
            season: 0,
            title: None,
            quality: Some("2160p".into()),
            voice: Some("Дубляж".into()),
        };
        assert_eq!(prompt.detail_label().as_deref(), Some("4K · Дубляж"));
    }
}
