//! The desktop player: the video plays in a native libmpv window, so MKV, HEVC, AC3 and DTS
//! play as they are, without the server re-encoding them. This screen starts the playback on
//! the server, hands the stream to mpv and keeps the viewer's place saved on the server.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use kino_core::{
    episode_code, MediaKind, MediaTrack, PlayInfo, PlayRequest, ProgressUpdate, ReleaseRef, StreamPhase,
    StreamProgress, TracksUpdate,
};
use kino_mpv::{PlayOptions, PlayerEvent, Session};
use kino_ui::ConnectProgress;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::player::{release_label, PlayStart, SAVE_EVERY};
use crate::{api, platform};

/// What the screen shows while the mpv window is open.
#[derive(Debug, Clone, PartialEq, Default)]
struct Panel {
    title: String,
    subtitle: Option<String>,
    release: String,
    status: Option<String>,
    error: Option<String>,
    paused: bool,
    position: f64,
    duration: Option<f64>,
    next: Option<String>,
    /// How far the torrent has got while connecting.
    progress: Option<StreamProgress>,
}

fn track_label(list: &[MediaTrack], index: Option<usize>) -> Option<String> {
    let index = i32::try_from(index?).ok()?;
    list.iter().find(|t| t.index == index).map(|t| t.label())
}

#[component]
pub fn Player(
    token: String,
    start: PlayStart,
    on_close: EventHandler<()>,
    /// The next season has no remembered release: pick one for `(season, episode)`.
    on_need_release: EventHandler<(i32, i32)>,
) -> Element {
    let kind = start.kind;
    let id = start.tmdb_id;
    let mut target = use_signal(|| (start.season, start.episode));
    let mut release = use_signal(|| start.release.clone());
    let mut info = use_signal(|| None::<PlayInfo>);
    let mut panel = use_signal(|| Panel {
        title: start.title.clone(),
        release: release_label(&start.release),
        status: Some("Подключаемся к раздаче…".into()),
        ..Panel::default()
    });
    let mut last_saved = use_signal(|| 0.0_f64);
    let device = use_hook(platform::device_name);
    // The mpv window; closed when this screen goes away.
    let session = use_hook(|| Rc::new(RefCell::new(None::<Session>)));
    let events = use_hook(|| Rc::new(RefCell::new(None::<UnboundedSender<PlayerEvent>>)));
    {
        let session = session.clone();
        use_drop(move || {
            if let Some(s) = session.borrow_mut().take() {
                s.close();
            }
        });
    }

    let save = {
        let token = token.clone();
        let device = device.clone();
        move |position: f64, duration: Option<f64>| {
            let (Some(i), Some(duration)) = (info.peek().clone(), duration) else {
                return;
            };
            if duration <= 0.0 {
                return;
            }
            let (season, episode) = target();
            let rel = release.peek().clone();
            let u = ProgressUpdate {
                kind,
                tmdb_id: id,
                season: if kind == MediaKind::Tv { season } else { 0 },
                episode: if kind == MediaKind::Tv { episode } else { 0 },
                position_sec: position.min(duration),
                duration_sec: duration,
                release: Some(ReleaseRef {
                    infohash: i.infohash.clone(),
                    file_index: i.file_index,
                    title: rel.title.clone(),
                }),
                device: Some(device.clone()),
            };
            last_saved.set(position);
            let t = token.clone();
            spawn(async move {
                let _ = api::save_progress(&t, &u).await;
            });
        }
    };

    // Asks the server for the stream of the current target and plays it in mpv.
    let begin = {
        let token = token.clone();
        let session = session.clone();
        let events = events.clone();
        move || {
            let (season, episode) = target();
            let rel = release();
            let t = token.clone();
            let session = session.clone();
            let events = events.clone();
            info.set(None);
            let code = (kind == MediaKind::Tv).then(|| episode_code(season, episode));
            panel.with_mut(|p| {
                p.release = release_label(&rel);
                p.subtitle = code.clone();
                p.error = None;
                p.status = Some("Подключаемся к раздаче…".into());
                p.position = 0.0;
                p.duration = None;
            });
            spawn(async move {
                let req = PlayRequest {
                    kind,
                    tmdb_id: id,
                    season: if kind == MediaKind::Tv { season } else { 0 },
                    episode: if kind == MediaKind::Tv { episode } else { 0 },
                    release: rel.infohash.clone(),
                };
                let i = match api::play(&t, &req).await {
                    Ok(i) => i,
                    Err(e) => {
                        panel.with_mut(|p| {
                            p.status = None;
                            p.error = Some(e.message);
                        });
                        return;
                    }
                };
                let next = i.next.map(|(s, e)| episode_code(s, e));
                let title = kino_title(&panel.peek().title, code);
                // `PlayInfo::stream` is already absolute (see `api::play`); no `audio`/`start`
                // parameters: mpv plays the original file with all its tracks.
                let opts = PlayOptions {
                    url: i.stream.clone(),
                    title,
                    start: f64::from(i.start_at),
                    audio: i.audio_track.and_then(|a| usize::try_from(a).ok()),
                    subtitle: i.subtitle_track.and_then(|s| usize::try_from(s).ok()),
                    next: next.clone(),
                };
                panel.with_mut(|p| {
                    p.next = next;
                    p.status = Some("Открываем окно плеера…".into());
                });
                last_saved.set(f64::from(i.start_at));
                info.set(Some(i));
                let result = match session.borrow().as_ref() {
                    // The window is already open: the next episode plays in it.
                    Some(s) => s.load(opts),
                    None => {
                        let tx = events.borrow().clone();
                        match tx {
                            Some(tx) => Session::start(opts, false, move |e| {
                                let _ = tx.send(e);
                            })
                            .map(|s| {
                                *session.borrow_mut() = Some(s);
                            }),
                            None => Ok(()),
                        }
                    }
                };
                if let Err(e) = result {
                    panel.with_mut(|p| {
                        p.status = None;
                        p.error = Some(e.to_string());
                    });
                }
            });
        }
    };

    let go_next = {
        let mut begin = begin.clone();
        let mut save = save.clone();
        let choices = start.choices.clone();
        let session = session.clone();
        move || {
            let Some((ns, ne)) = info.peek().as_ref().and_then(|i| i.next) else {
                return;
            };
            let p = panel.peek().clone();
            save(p.position, p.duration);
            let (season, _) = target();
            if ns != season {
                match choices.iter().find(|c| c.season == ns) {
                    Some(c) => release.set(c.clone()),
                    None => {
                        if let Some(s) = session.borrow_mut().take() {
                            s.close();
                        }
                        on_need_release.call((ns, ne));
                        return;
                    }
                }
            }
            target.set((ns, ne));
            begin();
        }
    };

    // Events of the mpv window arrive from its thread; handle them here.
    use_hook({
        let events = events.clone();
        let mut begin = begin.clone();
        let mut go_next = go_next.clone();
        let mut save = save.clone();
        let token = token.clone();
        let session = session.clone();
        move || {
            let (tx, mut rx) = unbounded_channel::<PlayerEvent>();
            *events.borrow_mut() = Some(tx);
            spawn(async move {
                while let Some(ev) = rx.recv().await {
                    match ev {
                        PlayerEvent::Loaded { duration, .. } => panel.with_mut(|p| {
                            p.status = None;
                            p.duration = duration;
                        }),
                        PlayerEvent::Position { position, duration } => {
                            panel.with_mut(|p| {
                                p.position = position;
                                p.duration = duration.or(p.duration);
                            });
                            if (position - last_saved()).abs() >= SAVE_EVERY {
                                save(position, duration);
                            }
                        }
                        PlayerEvent::Paused(paused) => {
                            panel.with_mut(|p| p.paused = paused);
                            if paused {
                                let p = panel.peek().clone();
                                save(p.position, p.duration);
                            }
                        }
                        PlayerEvent::Tracks { audio, subtitle } => {
                            let Some(i) = info.peek().clone() else { continue };
                            let t = TracksUpdate {
                                audio_track: track_label(&i.audio, audio),
                                subtitle_track: Some(
                                    track_label(&i.subtitles, subtitle).unwrap_or_else(|| "off".into()),
                                ),
                            };
                            let token = token.clone();
                            spawn(async move {
                                let _ = api::set_tracks(&token, kind, id, &t).await;
                            });
                        }
                        PlayerEvent::NextRequested => go_next(),
                        PlayerEvent::Ended => {
                            let p = panel.peek().clone();
                            save(p.duration.unwrap_or(p.position), p.duration);
                            if info.peek().as_ref().is_some_and(|i| i.next.is_some()) {
                                go_next();
                            } else if let Some(s) = session.borrow_mut().take() {
                                // Last episode: closing the window ends this screen.
                                s.close();
                            }
                        }
                        PlayerEvent::Closed { position } => {
                            let duration = panel.peek().duration;
                            save(position, duration);
                            session.borrow_mut().take();
                            on_close.call(());
                            break;
                        }
                        PlayerEvent::Error(msg) => panel.with_mut(|p| {
                            p.status = None;
                            p.error = Some(msg);
                        }),
                    }
                }
            });
            begin();
        }
    });

    // While connecting, shows how far the torrent has got instead of a bare spinner.
    {
        let token = token.clone();
        use_future(move || {
            let token = token.clone();
            async move {
                loop {
                    platform::sleep(1_000).await;
                    let busy = {
                        let p = panel.peek();
                        p.error.is_none() && p.status.is_some()
                    };
                    if !busy {
                        if panel.peek().progress.is_some() {
                            panel.with_mut(|p| p.progress = None);
                        }
                        continue;
                    }
                    let key = release.peek().infohash.clone();
                    if let Ok(pr) = api::play_progress(&token, &key).await {
                        if panel.peek().status.is_some() {
                            let shown = (pr.phase != StreamPhase::Ready).then_some(pr);
                            panel.with_mut(|p| p.progress = shown);
                        }
                    }
                }
            }
        });
    }

    let p = panel();
    let hint = "Пробел — пауза, ← → — перемотка, F — на весь экран, A и J — звук и субтитры, N — следующая серия";
    rsx! {
        div { class: "player",
            div { class: "p-top",
                button {
                    class: "p-icon",
                    title: "Закрыть плеер",
                    onclick: {
                        let session = session.clone();
                        move |_| match session.borrow_mut().take() {
                            Some(s) => s.close(),
                            None => on_close.call(()),
                        }
                    },
                    "←"
                }
                div { class: "p-title",
                    div { class: "t", "{p.title}" }
                    div { class: "s",
                        if let Some(sub) = &p.subtitle { "{sub} · " }
                        "{p.release}"
                    }
                }
            }
            div { class: "p-center",
                if let Some(err) = &p.error {
                    div { class: "p-error",
                        p { "{err}" }
                        button { class: "btn ghost", onclick: move |_| on_close.call(()), "Закрыть" }
                    }
                } else if let Some(st) = &p.status {
                    if let Some(pr) = &p.progress {
                        ConnectProgress { progress: pr.clone() }
                    } else {
                        div { class: "spinner" }
                        p { "{st}" }
                    }
                } else {
                    p { if p.paused { "Пауза" } else { "Идёт воспроизведение в окне плеера" } }
                    if let Some(d) = p.duration {
                        p { class: "muted", "{kino_core::clock(p.position)} из {kino_core::clock(d)}" }
                    }
                    p { class: "muted small", "{hint}" }
                }
            }
        }
    }
}

/// Window title: "Во все тяжкие · S02E06".
fn kino_title(title: &str, code: Option<String>) -> String {
    match code {
        Some(c) => format!("{title} · {c}"),
        None => title.to_owned(),
    }
}
