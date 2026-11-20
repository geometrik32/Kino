//! Drives the `<video>` of [`PlayerView`]: starts playback on the server,
//! seeks, switches tracks, saves the position and moves to the next episode.

use dioxus::prelude::*;
use kino_core::{
    episode_code, MediaKind, PlayInfo, PlayRequest, ProgressUpdate, ReleaseChoiceUpdate, ReleaseRef, TracksUpdate,
};
use kino_ui::{PlayerMenu, PlayerState, PlayerView, VIDEO_ID};
use wasm_bindgen::JsCast;

use crate::api;

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
}

/// How often the position is saved while playing, seconds of video.
const SAVE_EVERY: f64 = 10.0;

fn video() -> Option<web_sys::HtmlVideoElement> {
    web_sys::window()?
        .document()?
        .get_element_by_id(VIDEO_ID)?
        .dyn_into()
        .ok()
}

/// "Android", "iPhone", "Windows", ... for the watch history.
pub fn device_name() -> String {
    if crate::telegram::init_data().is_some() {
        return "Telegram".into();
    }
    let ua = web_sys::window()
        .and_then(|w| w.navigator().user_agent().ok())
        .unwrap_or_default();
    let os = [
        ("iPhone", "iPhone"),
        ("iPad", "iPad"),
        ("Android", "Android"),
        ("Windows", "Windows"),
        ("Mac OS", "Mac"),
        ("Linux", "Linux"),
    ]
    .iter()
    .find(|(k, _)| ua.contains(k))
    .map(|(_, v)| *v)
    .unwrap_or("Браузер");
    format!("Браузер, {os}")
}

fn release_label(r: &ReleaseChoiceUpdate) -> String {
    match (&r.voice, &r.quality) {
        (Some(v), Some(q)) => format!("{v} · {q}"),
        _ => r.title.clone().unwrap_or_default(),
    }
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
    let mut ui = use_signal(|| PlayerState {
        title: start.title.clone(),
        poster: start.poster.clone(),
        status: Some("Подключаемся к раздаче…".into()),
        ..PlayerState::default()
    });
    // Start of the current source within the file (re-encoded streams start at `offset`).
    let mut offset = use_signal(|| 0.0_f64);
    // Where to jump once a direct source has its metadata.
    let mut pending_seek = use_signal(|| None::<f64>);
    let mut force_transcode = use_signal(|| false);
    let mut last_saved = use_signal(|| 0.0_f64);
    let device = use_hook(device_name);

    // Points the video at the right source for the given audio track and second.
    let mut load_source = move |at: f64| {
        let Some(i) = info() else { return };
        let audio = ui.peek().audio_track;
        let transcode = force_transcode() || !i.plays_directly(audio);
        let (src, off) = if transcode {
            (i.transcoded_url(audio, at), at.floor())
        } else {
            pending_seek.set((at > 0.0).then_some(at));
            (i.stream.clone(), 0.0)
        };
        offset.set(off);
        ui.with_mut(|s| {
            s.src = src;
            s.transcoded = transcode;
            s.position = at;
            s.waiting = true;
            s.subtitles_src = s.subtitle_track.map(|t| i.subtitle_url(t, off));
        });
    };

    let save = {
        let token = token.clone();
        let device = device.clone();
        move |finished: bool| {
            let (Some(i), s) = (info.peek().clone(), ui.peek().clone()) else {
                return;
            };
            let duration = s.duration.unwrap_or(0.0);
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
                position_sec: if finished { duration } else { s.position },
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

    // Asks the server for the stream of the current target.
    let begin = {
        let token = token.clone();
        move || {
            let (season, episode) = target();
            let rel = release();
            let t = token.clone();
            info.set(None);
            force_transcode.set(false);
            ui.with_mut(|s| {
                s.src.clear();
                s.error = None;
                s.menu = None;
                s.position = 0.0;
                s.duration = None;
                s.release = release_label(&rel);
                s.subtitle = (kind == MediaKind::Tv).then(|| episode_code(season, episode));
                s.status = Some("Подключаемся к раздаче…".into());
            });
            spawn(async move {
                let req = PlayRequest {
                    kind,
                    tmdb_id: id,
                    season: if kind == MediaKind::Tv { season } else { 0 },
                    episode: if kind == MediaKind::Tv { episode } else { 0 },
                    release: rel.infohash.clone(),
                };
                match api::play(&t, &req).await {
                    Ok(i) => {
                        ui.with_mut(|s| {
                            s.status = None;
                            s.duration = i.duration;
                            s.audio = i.audio.clone();
                            s.subtitle_tracks = i.subtitles.clone();
                            s.audio_track = i.audio_track;
                            s.subtitle_track = i.subtitle_track;
                            s.next = i.next.map(|(s, e)| episode_code(s, e));
                            s.preloaded = i.preloaded;
                        });
                        let at = f64::from(i.start_at);
                        last_saved.set(at);
                        info.set(Some(i));
                        load_source(at);
                    }
                    Err(e) => ui.with_mut(|s| {
                        s.status = None;
                        s.error = Some(e.message);
                    }),
                }
            });
        }
    };
    use_hook(begin.clone());

    let mut go_next = {
        let mut save = save.clone();
        let mut begin = begin.clone();
        let choices = start.choices.clone();
        move || {
            let Some((ns, ne)) = info.peek().as_ref().and_then(|i| i.next) else {
                return;
            };
            save(true);
            let (season, _) = target();
            if ns != season {
                match choices.iter().find(|c| c.season == ns) {
                    Some(c) => release.set(c.clone()),
                    None => {
                        on_need_release.call((ns, ne));
                        return;
                    }
                }
            }
            target.set((ns, ne));
            begin();
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

    let on_video = {
        let mut save = save.clone();
        let mut go_next = go_next.clone();
        move |ev: &'static str| {
            let Some(v) = video() else { return };
            match ev {
                "time" => {
                    let pos = offset() + v.current_time();
                    let direct_len = (!ui.peek().transcoded && v.duration().is_finite()).then(|| v.duration());
                    ui.with_mut(|s| {
                        s.position = pos;
                        if s.duration.is_none() {
                            s.duration = direct_len;
                        }
                    });
                    if (pos - last_saved()).abs() >= SAVE_EVERY {
                        save(false);
                    }
                }
                "metadata" => {
                    if let Some(at) = pending_seek.take() {
                        v.set_current_time(at);
                    }
                }
                "waiting" => ui.with_mut(|s| s.waiting = true),
                "playing" | "play" => ui.with_mut(|s| {
                    s.waiting = false;
                    s.paused = false;
                }),
                "pause" => {
                    ui.with_mut(|s| s.paused = true);
                    save(false);
                }
                "ended" => {
                    save(true);
                    go_next();
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
                        ui.with_mut(|s| {
                            s.waiting = false;
                            s.error = Some("Видео не загружается. Возможно, у раздачи не осталось сидов: попробуйте другую раздачу или «Скачать заранее».".into());
                        });
                    }
                }
                _ => {}
            }
        }
    };

    let close = {
        let mut save = save.clone();
        move |_| {
            save(false);
            if let Some(v) = video() {
                let _ = v.pause();
            }
            on_close.call(());
        }
    };

    rsx! {
        PlayerView {
            state: ui(),
            on_toggle: move |_| {
                if let Some(v) = video() {
                    if v.paused() {
                        let _ = v.play();
                    } else {
                        let _ = v.pause();
                    }
                }
            },
            on_seek: move |at: f64| {
                let s = ui.peek().clone();
                let at = match s.duration {
                    Some(d) => at.clamp(0.0, (d - 1.0).max(0.0)),
                    None => at.max(0.0),
                };
                if s.transcoded {
                    load_source(at);
                } else if let Some(v) = video() {
                    v.set_current_time(at);
                    ui.with_mut(|s| s.position = at);
                }
            },
            on_menu: move |m: Option<PlayerMenu>| ui.with_mut(|s| s.menu = m),
            on_audio: {
                let save_tracks = save_tracks.clone();
                move |a: i32| {
                    let at = ui.peek().position;
                    ui.with_mut(|s| {
                        s.audio_track = Some(a);
                        s.menu = None;
                    });
                    load_source(at);
                    save_tracks();
                }
            },
            on_subtitle: {
                let save_tracks = save_tracks.clone();
                move |t: Option<i32>| {
                    let off = offset();
                    let url = info.peek().as_ref().and_then(|i| t.map(|t| i.subtitle_url(t, off)));
                    ui.with_mut(|s| {
                        s.subtitle_track = t;
                        s.subtitles_src = url;
                        s.menu = None;
                    });
                    save_tracks();
                }
            },
            on_next: move |_| go_next(),
            on_fullscreen: move |_| {
                if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                    if doc.fullscreen_element().is_some() {
                        doc.exit_fullscreen();
                    } else if let Some(el) = doc.document_element() {
                        let _ = el.request_fullscreen();
                    }
                }
            },
            on_close: close,
            on_video,
        }
    }
}
