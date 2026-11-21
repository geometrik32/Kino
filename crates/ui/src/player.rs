//! The video player: a `<video>` with our own cinema floating glass controls,
//! unified Audio/Subtitles menu, screen lock, live scrub tooltip and desktop volume.

use std::rc::Rc;

use dioxus::html::MountedData;
use dioxus::prelude::*;
use kino_core::{clock, MediaTrack, StreamProgress, SyncMemberInfo};

use crate::icons::{self, Icon};

/// `id` of the `<video>` element; the app drives it through the DOM.
pub const VIDEO_ID: &str = "kino-video";

/// Which menu of the player is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerMenu {
    Audio,
    Subtitles,
    Speed,
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
    /// Volume 0.0..=1.0 and mute flag (desktop).
    pub volume: f32,
    pub muted: bool,
    /// Whether to show the inline desktop volume slider.
    pub show_volume: bool,
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
            volume: 1.0,
            muted: false,
            show_volume: true,
            season: 0,
            episode: 0,
            episodes: Vec::new(),
            flash: None,
            autoplay: true,
            auto_muted: false,
            sync_members: Vec::new(),
            sync_waiting_for: Vec::new(),
            sync_client_id: String::new(),
        }
    }
}

/// Seconds before the end when "next episode" is offered.
pub const NEXT_PROMPT_SEC: f64 = 40.0;

fn track_name(list: &[MediaTrack], index: Option<i32>, none: &str) -> String {
    index
        .and_then(|i| list.iter().find(|t| t.index == i))
        .map(|t| t.label())
        .unwrap_or_else(|| none.to_owned())
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
    on_fullscreen: EventHandler<()>,
    on_close: EventHandler<()>,
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
) -> Element {
    let mut locked = use_signal(|| false);
    let mut lock_hint = use_signal(|| false);
    let mut scrub = use_signal(|| None::<f64>);
    let mut track_el = use_signal(|| None::<Rc<MountedData>>);
    let mut drag_rect = use_signal(|| None::<(f64, f64)>);
    let mut dismissed_next = use_signal(|| None::<String>);

    let s = state;
    let dur = s.duration.unwrap_or(0.0);
    let pos = s.position.clamp(0.0, if dur > 0.0 { dur } else { f64::MAX });
    let display_pos = scrub()
        .unwrap_or(pos)
        .clamp(0.0, if dur > 0.0 { dur } else { f64::MAX });
    let pct = if dur > 0.0 { display_pos / dur * 100.0 } else { 0.0 };
    let remaining = (dur - display_pos).max(0.0);
    let near_end = dur > 0.0 && dur - pos < NEXT_PROMPT_SEC;
    let show_next = near_end && s.next.is_some() && dismissed_next().as_deref() != s.next.as_deref() && !locked();
    let sub_name = track_name(&s.subtitle_tracks, s.subtitle_track, "Выкл");
    let audio_sub_open = matches!(s.menu, Some(PlayerMenu::Audio | PlayerMenu::Subtitles));
    let shown = !locked() && (s.controls || s.paused || s.menu.is_some() || scrub().is_some());
    let busy = s.error.is_none() && (s.waiting || s.status.is_some());
    let rate = if s.rate > 0.0 { s.rate } else { 1.0 };
    let vol_pct = if s.muted {
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
        div { class: "{player_cls}",
            if s.native_video {
                div { id: VIDEO_ID, class: "p-native-slot" }
            } else {
                video {
                    id: VIDEO_ID,
                    src: if s.src.is_empty() { None } else { Some(s.src.clone()) },
                    poster: s.poster.clone().unwrap_or_default(),
                    autoplay: s.autoplay,
                    playsinline: true,
                    preload: "auto",
                    ontimeupdate: move |_| on_video.call("time"),
                    onloadedmetadata: move |_| on_video.call("metadata"),
                    oncanplay: move |_| on_video.call("canplay"),
                    onseeked: move |_| on_video.call("seeked"),
                    onwaiting: move |_| on_video.call("waiting"),
                    onplaying: move |_| on_video.call("playing"),
                    onpause: move |_| on_video.call("pause"),
                    onplay: move |_| on_video.call("play"),
                    onended: move |_| on_video.call("ended"),
                    onerror: move |_| on_video.call("error"),
                    if let Some(vtt) = &s.subtitles_src {
                        track { kind: "subtitles", src: "{vtt}", srclang: "ru", label: "{sub_name}", default: true }
                    }
                }
            }
            div { class: "p-vignette" }

            if locked() {
                div {
                    class: "p-lock-screen",
                    onclick: move |_| lock_hint.toggle(),
                    if lock_hint() {
                        button {
                            class: "p-glass-btn p-unlock-btn",
                            onclick: move |e| {
                                e.stop_propagation();
                                locked.set(false);
                                lock_hint.set(false);
                            },
                            Icon { svg: icons::UNLOCK, class: "ic sm" }
                            "Разблокировать экран"
                        }
                    }
                }
            } else {
                // Taps on the picture: once shows/hides controls (touch) or toggles play (mouse), twice jumps 10s.
                div { class: "p-zones",
                    div {
                        class: "p-zone",
                        onclick: move |_| on_tap.call(()),
                        ondoubleclick: move |_| on_double_tap.call(false),
                    }
                    div {
                        class: "p-zone",
                        onclick: move |_| on_tap.call(()),
                        ondoubleclick: move |_| on_double_tap.call(true),
                    }
                }

                if let Some((forward, secs)) = s.flash {
                    div { class: if forward { "p-flash right" } else { "p-flash left" },
                        if forward { "{secs} с ›››" } else { "‹‹‹ {secs} с" }
                    }
                }

                div { class: "p-top",
                    div { class: "p-top-l",
                        button {
                            class: "p-glass-btn icon",
                            title: "К странице тайтла",
                            "aria-label": "Назад",
                            onclick: move |_| on_close.call(()),
                            Icon { svg: icons::BACK, class: "ic" }
                        }
                        button {
                            class: "p-title-link",
                            title: "Открыть страницу тайтла",
                            onclick: move |_| on_close.call(()),
                            div { class: "p-title-t",
                                span { "{s.title}" }
                                Icon { svg: icons::CHEV_R, class: "ic sm muted" }
                            }
                            div { class: "p-title-s",
                                if let Some(sub) = &s.subtitle { span { "{sub} · " } }
                                span { "{s.release}" }
                                if s.preloaded { span { class: "tag ok", "с диска" } }
                                if s.transcoded { span { class: "tag warn", "перекодируется" } }
                            }
                        }
                    }
                    div { class: "p-top-r",
                        button {
                            class: "p-glass-btn",
                            title: "Выбрать другую раздачу",
                            onclick: move |_| on_pick_release.call(()),
                            Icon { svg: icons::SWAP, class: "ic sm" }
                            span { class: "long", "Сменить раздачу" }
                            span { class: "short", "Раздача" }
                        }
                        button {
                            class: "p-glass-btn icon",
                            title: "Заблокировать экран от случайных нажатий",
                            "aria-label": "Заблокировать экран",
                            onclick: move |_| {
                                on_menu.call(None);
                                locked.set(true);
                                lock_hint.set(true);
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
                                rsx! {
                                    button {
                                        class: if s.menu == Some(PlayerMenu::Sync) {
                                            "p-glass-btn icon p-sync-btn active"
                                        } else if sync_wait {
                                            "p-glass-btn icon p-sync-btn wait"
                                        } else {
                                            "p-glass-btn icon p-sync-btn"
                                        },
                                        title: "{sync_title}",
                                        "aria-label": "{sync_title}",
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
                } else if s.paused && s.menu.is_none() && s.error.is_none() {
                    // Unobstructed center during playback; big Play appears ONLY while paused.
                    button {
                        class: "p-center p-big",
                        "aria-label": "Продолжить воспроизведение",
                        onclick: move |_| on_toggle.call(()),
                        Icon { svg: icons::PLAY, class: "ic xl" }
                    }
                }

                if let Some(err) = &s.error {
                    div { class: "p-center p-error",
                        p { "{err}" }
                        div { class: "p-err-acts",
                            button { class: "btn primary sm", onclick: move |_| on_pick_release.call(()),
                                Icon { svg: icons::SWAP, class: "ic sm" }
                                "Другая раздача"
                            }
                            button { class: "btn glass sm", onclick: move |_| on_close.call(()), "Закрыть" }
                        }
                    }
                }

                if s.auto_muted && !s.paused && s.error.is_none() {
                    button {
                        class: "p-glass-btn p-unmute-btn",
                        title: "Нажмите, чтобы включить звук",
                        "aria-label": "Включить звук",
                        onclick: move |_| {
                            if let Some(cb) = on_unmute {
                                cb.call(());
                            }
                        },
                        Icon { svg: icons::VOL_MUTE, class: "ic sm" }
                        "Включить звук"
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
                            title: "Скрыть",
                            "aria-label": "Скрыть",
                            onclick: move |_| dismissed_next.set(next_for_dismiss.clone()),
                            Icon { svg: icons::CLOSE, class: "ic sm" }
                        }
                    }
                }

                div { class: "p-dock",
                    div { class: "p-seek-row",
                        span { class: "p-time", "{clock(display_pos)}" }
                        div {
                            class: "p-track",
                            onmounted: move |e| track_el.set(Some(e.data())),
                            onpointerdown: move |e| {
                                let Some(el) = track_el() else { return };
                                let cy = e.client_coordinates().y;
                                spawn(async move {
                                    if let Ok(rect) = el.get_client_rect().await {
                                        if rect.height() > rect.width() && rect.height() > 1.0 {
                                            let top = rect.min_y();
                                            let h = rect.height();
                                            drag_rect.set(Some((top, h)));
                                            let frac = ((cy - top) / h).clamp(0.0, 1.0);
                                            scrub.set(Some(frac * dur.max(1.0)));
                                        }
                                    }
                                });
                            },
                            onpointermove: move |e| {
                                if let Some((top, h)) = drag_rect() {
                                    let frac = ((e.client_coordinates().y - top) / h).clamp(0.0, 1.0);
                                    scrub.set(Some(frac * dur.max(1.0)));
                                }
                            },
                            onpointerup: move |e| {
                                if let Some((top, h)) = drag_rect.take() {
                                    let frac = ((e.client_coordinates().y - top) / h).clamp(0.0, 1.0);
                                    let v = frac * dur.max(1.0);
                                    scrub.set(None);
                                    on_seek.call(v);
                                }
                            },
                            onpointercancel: move |_| {
                                drag_rect.set(None);
                                scrub.set(None);
                            },
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
                                "aria-label": "Перемотка",
                                oninput: move |e| {
                                    if let Ok(v) = e.value().parse::<f64>() {
                                        scrub.set(Some(v));
                                    }
                                },
                                onchange: move |e| {
                                    scrub.set(None);
                                    if let Ok(v) = e.value().parse::<f64>() {
                                        on_seek.call(v);
                                    }
                                },
                            }
                        }
                        span { class: "p-time r", if dur > 0.0 { "−{clock(remaining)}" } else { "--:--" } }
                    }
                    div { class: "p-ctrl-row",
                        button {
                            class: "p-glass-btn icon",
                            title: "Назад на 10 секунд",
                            "aria-label": "Назад на 10 секунд",
                            onclick: move |_| on_seek.call((pos - 10.0).max(0.0)),
                            Icon { svg: icons::BACK10, class: "ic" }
                        }
                        button {
                            class: "p-play-btn",
                            title: if s.paused { "Смотреть" } else { "Пауза" },
                            "aria-label": if s.paused { "Смотреть" } else { "Пауза" },
                            onclick: move |_| on_toggle.call(()),
                            Icon { svg: if s.paused { icons::PLAY } else { icons::PAUSE }, class: "ic" }
                        }
                        button {
                            class: "p-glass-btn icon",
                            title: "Вперёд на 10 секунд",
                            "aria-label": "Вперёд на 10 секунд",
                            onclick: move |_| on_seek.call(pos + 10.0),
                            Icon { svg: icons::FWD10, class: "ic" }
                        }
                        if s.show_volume {
                            div { class: "p-vol desktop-only",
                                button {
                                    class: "p-vol-btn",
                                    title: if s.muted || s.volume <= 0.001 { "Включить звук" } else { "Выключить звук" },
                                    "aria-label": "Громкость",
                                    onclick: move |_| {
                                        if let Some(cb) = on_mute {
                                            cb.call(());
                                        }
                                    },
                                    Icon { svg: if s.muted || s.volume <= 0.001 { icons::VOL_MUTE } else { icons::VOL }, class: "ic sm" }
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
                        div { class: "p-spacer" }
                        if !s.episodes.is_empty() {
                            button {
                                class: if s.menu == Some(PlayerMenu::Episodes) { "p-glass-btn active" } else { "p-glass-btn" },
                                onclick: move |_| on_menu.call(if s.menu == Some(PlayerMenu::Episodes) { None } else { Some(PlayerMenu::Episodes) }),
                                Icon { svg: icons::LIST, class: "ic sm" }
                                span { class: "long", "Серии" }
                            }
                        }
                        // Unified Audio + Subtitles button (icon-only on phones so all controls fit cleanly).
                        button {
                            class: if audio_sub_open { "p-glass-btn audio-btn active" } else { "p-glass-btn audio-btn" },
                            title: "Звук и субтитры",
                            "aria-label": "Звук и субтитры",
                            onclick: move |_| on_menu.call(if audio_sub_open { None } else { Some(PlayerMenu::Audio) }),
                            Icon { svg: icons::AUDIO_CC, class: "ic sm" }
                            span { class: "long", "Звук и субтитры" }
                        }
                        button {
                            class: if s.menu == Some(PlayerMenu::Speed) { "p-glass-btn active" } else { "p-glass-btn" },
                            title: "Скорость воспроизведения",
                            onclick: move |_| on_menu.call(if s.menu == Some(PlayerMenu::Speed) { None } else { Some(PlayerMenu::Speed) }),
                            "{speed_label(rate)}"
                        }
                        button {
                            class: "p-glass-btn icon p-fs-btn",
                            title: "Во весь экран",
                            "aria-label": "Во весь экран",
                            onclick: move |_| on_fullscreen.call(()),
                            Icon { svg: icons::FULL, class: "ic sm" }
                        }
                    }
                }

                if let Some(menu) = s.menu {
                    div {
                        class: match menu {
                            PlayerMenu::Speed => "p-popover speed",
                            PlayerMenu::Sync => "p-popover sync",
                            _ => "p-popover",
                        },
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
                                    } else if s.subtitle_tracks.is_empty() {
                                        "Субтитры (Нет)"
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
                            p { class: "muted small p-pop-note", "Выбор запоминается для этого тайтла на всех устройствах." }
                        } else if menu == PlayerMenu::Speed {
                            h4 { "Скорость воспроизведения" }
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
                        } else if menu == PlayerMenu::Sync {
                            h4 { "Синхронизация · {s.sync_members.len()} устр." }
                            p { class: "muted small p-pop-note p-sync-note",
                                "Воспроизведение, пауза, скорость и перемотка синхронизируются."
                            }
                            div { class: "p-sync-list",
                                for m in s.sync_members.clone() {
                                    {
                                        let is_self = !s.sync_client_id.is_empty() && m.client_id == s.sync_client_id;
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
                                        rsx! {
                                            div {
                                                key: "{m.client_id}",
                                                class: if is_self { "p-sync-row self" } else { "p-sync-row" },
                                                Icon { svg: icon_svg, class: "ic sm muted" }
                                                div { class: "p-sync-meta",
                                                    div { class: "p-sync-name", "{m.device}" }
                                                    if is_self {
                                                        div { class: "p-sync-sub", "Это устройство" }
                                                    }
                                                }
                                                span { class: "{status_cls}", "{status_txt}" }
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            h4 { "Серии · сезон {s.season}" }
                            for e in s.episodes.clone() {
                                button {
                                    key: "e{e.number}",
                                    class: if e.number == s.episode { "p-opt ep on" } else { "p-opt ep" },
                                    onclick: move |_| on_episode.call(e.number),
                                    span { "{e.number}. {e.title}" }
                                    if e.watched {
                                        span { class: "p-seen done", title: "Просмотрена", Icon { svg: icons::CHECK, class: "ic xs" } }
                                    } else {
                                        span { class: "p-seen", title: "Не просмотрена", "○" }
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
