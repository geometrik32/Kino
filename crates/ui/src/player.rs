//! The video player: a `<video>` with our own controls, so seeking works the
//! same whether the browser plays the file itself or the server re-encodes it.

use dioxus::prelude::*;
use kino_core::{clock, MediaTrack};

/// `id` of the `<video>` element; the app drives it through the DOM.
pub const VIDEO_ID: &str = "kino-video";

/// Which menu of the player is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerMenu {
    Audio,
    Subtitles,
}

/// Everything the player shows. The app owns the state and the `<video>`.
#[derive(Debug, Clone, PartialEq, Default)]
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
    /// Played from the server's disk.
    pub preloaded: bool,
}

/// Seconds before the end when "next episode" is offered.
pub const NEXT_PROMPT_SEC: f64 = 40.0;

fn track_name(list: &[MediaTrack], index: Option<i32>, none: &str) -> String {
    index
        .and_then(|i| list.iter().find(|t| t.index == i))
        .map(|t| t.label())
        .unwrap_or_else(|| none.to_owned())
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
    /// `<video>` events: time update, metadata, waiting, playing, pause, play, ended, error.
    on_video: EventHandler<&'static str>,
) -> Element {
    let s = state;
    let dur = s.duration.unwrap_or(0.0);
    let pos = s.position.clamp(0.0, if dur > 0.0 { dur } else { f64::MAX });
    let pct = if dur > 0.0 { pos / dur * 100.0 } else { 0.0 };
    let near_end = dur > 0.0 && dur - pos < NEXT_PROMPT_SEC;
    let audio_name = track_name(&s.audio, s.audio_track, "—");
    let sub_name = track_name(&s.subtitle_tracks, s.subtitle_track, "Выкл");
    let remaining = (dur - pos).max(0.0);
    rsx! {
        div { class: if s.paused || s.menu.is_some() { "player show" } else { "player" },
            video {
                id: VIDEO_ID,
                src: if s.src.is_empty() { None } else { Some(s.src.clone()) },
                poster: s.poster.clone().unwrap_or_default(),
                autoplay: true,
                playsinline: true,
                preload: "auto",
                onclick: move |_| on_toggle.call(()),
                ontimeupdate: move |_| on_video.call("time"),
                onloadedmetadata: move |_| on_video.call("metadata"),
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
            div { class: "p-top",
                button { class: "p-icon", title: "Назад", onclick: move |_| on_close.call(()), "←" }
                div { class: "p-title",
                    div { class: "t", "{s.title}" }
                    div { class: "s",
                        if let Some(sub) = &s.subtitle { "{sub} · " }
                        "{s.release}"
                        if s.preloaded { span { class: "tag ok", "с диска сервера" } }
                        if s.transcoded { span { class: "tag", "перекодируется" } }
                    }
                }
            }
            if s.waiting || s.status.is_some() {
                div { class: "p-center",
                    div { class: "spinner" }
                    if let Some(st) = &s.status { p { "{st}" } }
                }
            } else if s.paused && s.menu.is_none() {
                button { class: "p-center p-big", onclick: move |_| on_toggle.call(()), "▶" }
            }
            if let Some(err) = &s.error {
                div { class: "p-center p-error",
                    p { "{err}" }
                    button { class: "btn ghost", onclick: move |_| on_close.call(()), "Закрыть" }
                }
            }
            if let (true, Some(next)) = (near_end, &s.next) {
                div { class: "p-next",
                    div { class: "muted small", "Через {clock(remaining)}" }
                    button { class: "btn primary", onclick: move |_| on_next.call(()), "Следующая серия · {next}" }
                }
            }
            div { class: "p-bottom",
                div { class: "p-seek",
                    span { class: "p-time", "{clock(pos)}" }
                    div { class: "p-track",
                        div { class: "p-fill", style: "width: {pct:.2}%" }
                        input {
                            r#type: "range",
                            min: "0",
                            max: "{dur.max(1.0)}",
                            step: "1",
                            value: "{pos.floor()}",
                            "aria-label": "Перемотка",
                            onchange: move |e| {
                                if let Ok(v) = e.value().parse::<f64>() { on_seek.call(v) }
                            },
                        }
                    }
                    span { class: "p-time", if dur > 0.0 { "−{clock(remaining)}" } else { "--:--" } }
                }
                div { class: "p-buttons",
                    button { class: "p-icon", title: "Назад на 10 секунд", onclick: move |_| on_seek.call((pos - 10.0).max(0.0)), "↺10" }
                    button { class: "p-icon p-play", onclick: move |_| on_toggle.call(()), if s.paused { "▶" } else { "❚❚" } }
                    button { class: "p-icon", title: "Вперёд на 30 секунд", onclick: move |_| on_seek.call(pos + 30.0), "30↻" }
                    div { class: "p-spacer" }
                    button {
                        class: if s.menu == Some(PlayerMenu::Audio) { "p-pill on" } else { "p-pill" },
                        onclick: move |_| on_menu.call(if s.menu == Some(PlayerMenu::Audio) { None } else { Some(PlayerMenu::Audio) }),
                        span { class: "muted long", "Звук " }
                        span { class: "muted short", "♪ " }
                        "{audio_name}"
                    }
                    button {
                        class: if s.menu == Some(PlayerMenu::Subtitles) { "p-pill on" } else { "p-pill" },
                        onclick: move |_| on_menu.call(if s.menu == Some(PlayerMenu::Subtitles) { None } else { Some(PlayerMenu::Subtitles) }),
                        span { class: "muted long", "Субтитры " }
                        span { class: "muted short", "CC " }
                        "{sub_name}"
                    }
                    if let Some(next) = &s.next {
                        button { class: "p-pill", onclick: move |_| on_next.call(()), span { class: "long", "Далее · " } "{next} ›" }
                    }
                    button { class: "p-icon", title: "Во весь экран", onclick: move |_| on_fullscreen.call(()), "⛶" }
                }
            }
            if let Some(menu) = s.menu {
                div { class: "p-menu",
                    h4 { if menu == PlayerMenu::Audio { "Звуковая дорожка" } else { "Субтитры" } }
                    if menu == PlayerMenu::Audio {
                        if s.audio.is_empty() { p { class: "muted small", "Сервер не смог прочитать дорожки файла." } }
                        for t in s.audio.clone() {
                            button {
                                key: "a{t.index}",
                                class: if s.audio_track == Some(t.index) { "p-opt on" } else { "p-opt" },
                                onclick: move |_| on_audio.call(t.index),
                                span { "{t.label()}" }
                                span { class: "muted small", "{t.codec.to_uppercase()}" }
                            }
                        }
                    } else {
                        button {
                            class: if s.subtitle_track.is_none() { "p-opt on" } else { "p-opt" },
                            onclick: move |_| on_subtitle.call(None),
                            "Выключены"
                        }
                        for t in s.subtitle_tracks.clone() {
                            button {
                                key: "s{t.index}",
                                class: if s.subtitle_track == Some(t.index) { "p-opt on" } else { "p-opt" },
                                onclick: move |_| on_subtitle.call(Some(t.index)),
                                span { "{t.label()}" }
                            }
                        }
                    }
                    p { class: "muted small", "Выбор запоминается для этого фильма или сериала на всех устройствах." }
                }
            }
        }
    }
}
