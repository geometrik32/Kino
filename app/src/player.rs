//! Drives the `<video>` of [`PlayerView`]: starts playback on the server,
//! seeks, switches tracks, saves the position and moves to the next episode.

use dioxus::prelude::*;
use kino_core::{
    episode_code, MediaKind, PlayInfo, PlayRequest, ProgressUpdate, ReleaseChoiceUpdate, ReleaseRef, StreamPhase,
    TracksUpdate,
};
use kino_ui::{PlayerEpisode, PlayerMenu, PlayerState, PlayerView};

use crate::{api, native, platform, platform::video::video, telegram};

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

pub use crate::platform::device_name;

fn release_label(r: &ReleaseChoiceUpdate) -> String {
    match (&r.voice, &r.quality) {
        (Some(v), Some(q)) => format!("{v} · {q}"),
        _ => r.title.clone().unwrap_or_default(),
    }
}

/// Aborts the `<video>` element's open network fetch immediately.
fn stop_video_element() {
    let _ = dioxus::document::eval(
        "var v = document.getElementById('kino-video'); if (v) { v.pause(); v.removeAttribute('src'); v.load(); }",
    );
}

/// Gives the screen back (rotation, system bars) when the player goes away.
struct PlayerModeGuard;

impl Drop for PlayerModeGuard {
    fn drop(&mut self) {
        platform::player_mode(false);
    }
}

/// How long the controls stay on screen while a film plays, milliseconds.
const CONTROLS_MS: u32 = 3_500;

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
    platform::video::use_bridge();
    // Finger or mouse: a tap on the picture shows the controls on a phone, pauses on a computer.
    let touch = use_resource(platform::touch_screen);
    // Bumped on every touch so that a stale timer does not hide the controls.
    let mut interaction = use_signal(|| 0u32);
    let mut flash_gen = use_signal(|| 0u32);
    let mut play_gen = use_signal(|| 0u32);
    let mut stall_secs = use_signal(|| 0u32);
    let mut connecting = use_signal(|| true);
    let mut played = use_signal(|| false);
    let mut rate = use_signal(|| 1.0_f32);
    let mut volume = use_signal(|| 1.0_f32);
    let mut muted = use_signal(|| false);
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

    // Shows the controls and hides them again after a few seconds of playing.
    let mut show_controls = move || {
        ui.with_mut(|s| s.controls = true);
        let n = interaction() + 1;
        interaction.set(n);
        spawn(async move {
            platform::sleep(CONTROLS_MS).await;
            if interaction() == n {
                ui.with_mut(|s| {
                    if !s.paused && s.menu.is_none() {
                        s.controls = false;
                    }
                });
            }
        });
    };

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
        stall_secs.set(0);
        played.set(false);
        ui.with_mut(|s| {
            s.src = src;
            s.transcoded = transcode;
            s.position = at;
            s.waiting = true;
            s.status = Some(status.into());
            s.progress = None;
            s.subtitles_src = if i.preloaded {
                s.subtitle_track.map(|t| i.subtitle_url(t, off))
            } else {
                None
            };
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

    // While connecting or buffering, shows how far the torrent has got instead of a bare spinner.
    {
        let token = token.clone();
        use_future(move || {
            let token = token.clone();
            async move {
                loop {
                    platform::sleep(1_000).await;
                    let (busy, preloaded) = {
                        let s = ui.peek();
                        (s.error.is_none() && (connecting() || s.waiting), s.preloaded)
                    };
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

    // Asks the server for the stream of the current target.
    let begin = {
        let token = token.clone();
        move || {
            let (season, episode) = target();
            let rel = release();
            let t = token.clone();
            let my_gen = play_gen() + 1;
            play_gen.set(my_gen);
            stall_secs.set(0);
            connecting.set(true);
            played.set(false);
            info.set(None);
            force_transcode.set(false);
            ui.with_mut(|s| {
                s.src.clear();
                s.subtitles_src = None;
                s.error = None;
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
            spawn(async move {
                platform::sleep(95_000).await;
                if play_gen() != my_gen {
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
            spawn(async move {
                let req = PlayRequest {
                    kind,
                    tmdb_id: id,
                    season: if kind == MediaKind::Tv { season } else { 0 },
                    episode: if kind == MediaKind::Tv { episode } else { 0 },
                    release: rel.infohash.clone(),
                };
                let res = api::play(&t, &req).await;
                if play_gen() != my_gen {
                    return;
                }
                connecting.set(false);
                match res {
                    Ok(i) => {
                        ui.with_mut(|s| {
                            s.duration = i.duration;
                            s.audio = i.audio.clone();
                            s.subtitle_tracks = i.subtitles.clone();
                            s.audio_track = i.audio_track;
                            s.subtitle_track = if i.preloaded { i.subtitle_track } else { None };
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
                        s.progress = None;
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
        let err_token = token.clone();
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
                    // A new source starts at normal speed and full volume.
                    if (rate() - 1.0).abs() > 0.001 {
                        v.set_rate(f64::from(rate()));
                    }
                    v.set_volume(f64::from(volume()));
                    v.set_muted(muted());
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
                }
                "playing" => {
                    played.set(true);
                    stall_secs.set(0);
                    ui.with_mut(|s| {
                        s.waiting = false;
                        s.paused = false;
                        s.status = None;
                        s.progress = None;
                    });
                }
                "play" => ui.with_mut(|s| s.paused = false),
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
                        let my_gen = play_gen();
                        let t = err_token.clone();
                        let key = release.peek().infohash.clone();
                        let preloaded = s.preloaded;
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
                            if play_gen() != my_gen {
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

    let close = Callback::new({
        let mut save = save.clone();
        move |_: ()| {
            save(false);
            if let Some(v) = video() {
                v.pause();
            }
            ui.with_mut(|s| {
                s.src.clear();
                s.subtitles_src = None;
                s.waiting = false;
                s.status = None;
            });
            stop_video_element();
            on_close.call(());
        }
    });
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
    let mut jump = move |by: f64| {
        let s = ui.peek().clone();
        let at = s.position + by;
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
    };
    let mut flash_jump = move |forward: bool| {
        jump(if forward { 10.0 } else { -10.0 });
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
            vid.set_muted(is_muted);
        }
    };
    let mut toggle_mute = move || {
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
    };
    let toggle_fullscreen = move || {
        if telegram::set_fullscreen(!telegram::is_fullscreen()) {
            return;
        }
        platform::toggle_fullscreen();
    };

    // Desktop keyboard shortcuts (Space/K, ←/→/J/L, ↑/↓ volume, M mute, F fullscreen, Esc).
    {
        let mut flash_jump = flash_jump;
        let mut set_vol = set_vol;
        let mut toggle_mute = toggle_mute;
        let mut show_controls = show_controls;
        use_hook(move || {
            spawn(async move {
                let mut bridge = dioxus::document::eval(
                    "if (window.__kinoPlayerKeysStop) { try { window.__kinoPlayerKeysStop(); } catch (e) {} }
                     var handler = function(e) {
                       var tag = e.target && e.target.tagName;
                       if (tag === 'INPUT' || tag === 'TEXTAREA' || e.ctrlKey || e.metaKey || e.altKey) return;
                       var code = e.code;
                       if (code === 'Space' || code === 'KeyK') { e.preventDefault(); dioxus.send('toggle'); }
                       else if (code === 'ArrowLeft' || code === 'KeyJ') { e.preventDefault(); dioxus.send('back'); }
                       else if (code === 'ArrowRight' || code === 'KeyL') { e.preventDefault(); dioxus.send('fwd'); }
                       else if (code === 'ArrowUp') { e.preventDefault(); dioxus.send('vol_up'); }
                       else if (code === 'ArrowDown') { e.preventDefault(); dioxus.send('vol_dn'); }
                       else if (code === 'KeyM') { e.preventDefault(); dioxus.send('mute'); }
                       else if (code === 'KeyF') { e.preventDefault(); dioxus.send('full'); }
                       else if (code === 'Escape') { e.preventDefault(); dioxus.send('esc'); }
                     };
                     window.addEventListener('keydown', handler);
                     window.__kinoPlayerKeysStop = function() { window.removeEventListener('keydown', handler); };",
                );
                while let Ok(cmd) = bridge.recv::<String>().await {
                    match cmd.as_str() {
                        "toggle" => {
                            if let Some(v) = video() {
                                if v.paused() {
                                    v.play()
                                } else {
                                    v.pause()
                                }
                            }
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
        let mut begin = begin.clone();
        move |n: i32| {
            let (season, current) = target();
            if n == current {
                ui.with_mut(|s| s.menu = None);
                return;
            }
            save(false);
            target.set((season, n));
            begin();
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
        st.volume = volume();
        st.muted = muted();
        st.show_volume = !finger;
        st.episodes = season_list.read().clone().unwrap_or_default();
        st
    };

    rsx! {
        native::BackButton { on_back: move |_| close.call(()) }
        PlayerView {
            state: shown,
            on_tap: move |_| {
                if ui.peek().menu.is_some() {
                    ui.with_mut(|s| s.menu = None);
                } else if finger {
                    if ui.peek().controls {
                        ui.with_mut(|s| s.controls = false);
                    } else {
                        show_controls();
                    }
                } else {
                    // Mouse: a click on the picture pauses or resumes.
                    if let Some(v) = video() {
                        if v.paused() { v.play() } else { v.pause() }
                    }
                }
            },
            on_double_tap: move |forward: bool| flash_jump(forward),
            on_speed: move |r: f32| {
                rate.set(r);
                ui.with_mut(|s| s.menu = None);
                if let Some(v) = video() {
                    v.set_rate(f64::from(r));
                }
            },
            on_episode: move |n: i32| play_episode(n),
            on_pick_release: move |_| {
                let (season, episode) = target();
                on_need_release.call((season, episode));
            },
            on_toggle: move |_| {
                if let Some(v) = video() {
                    if v.paused() {
                        v.play();
                    } else {
                        v.pause();
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
                }
            },
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
                    let url = info
                        .peek()
                        .as_ref()
                        .filter(|i| i.preloaded)
                        .and_then(|i| t.map(|t| i.subtitle_url(t, off)));
                    ui.with_mut(|s| {
                        s.subtitle_track = t;
                        s.subtitles_src = url;
                        s.menu = None;
                    });
                    save_tracks();
                }
            },
            on_next: move |_| go_next(),
            on_fullscreen: move |_| toggle_fullscreen(),
            on_close: close,
            on_video,
            on_volume: move |v: f32| set_vol(v),
            on_mute: move |_| toggle_mute(),
        }
    }
}
