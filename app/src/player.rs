//! Drives the `<video>` of [`PlayerView`]: starts playback on the server,
//! seeks, switches tracks, saves the position, moves to the next episode and
//! keeps multiple devices of the same account synchronized in real time.

use dioxus::prelude::*;
use kino_core::{
    episode_code, MediaKind, PlayInfo, PlayRequest, ProgressUpdate, ReleaseChoiceUpdate, ReleaseRef, StreamPhase,
    SyncClientMsg, SyncEventKind, SyncJoinRequest, SyncLeaveRequest, SyncServerMsg, SyncState, TracksUpdate,
};
use kino_ui::{PlayerEpisode, PlayerMenu, PlayerState, PlayerView};

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
}

/// How often the position is saved while playing, seconds of video.
const SAVE_EVERY: f64 = 10.0;

/// How often the player sends a sync heartbeat & checks drift, milliseconds.
const SYNC_TICK_MS: u32 = 2_000;

/// How long the controls stay on screen while a film plays, milliseconds.
const CONTROLS_MS: u32 = 3_500;

pub use crate::platform::device_name;

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
    on_close: EventHandler<()>,
    /// The next season has no remembered release: pick one for `(season, episode)`.
    on_need_release: EventHandler<(i32, i32)>,
) -> Element {
    let kind = start.kind;
    let id = start.tmdb_id;
    let client_id = use_hook(platform::random_id);
    let device = use_hook(device_name);

    let mut target = use_signal(|| (start.season, start.episode));
    let mut release = use_signal(|| start.release.clone());
    let mut info = use_signal(|| None::<PlayInfo>);
    let mut ui = use_signal(|| PlayerState {
        title: start.title.clone(),
        poster: start.poster.clone(),
        status: Some("Подключаемся к раздаче…".into()),
        sync_client_id: client_id.clone(),
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
            v.set_muted(muted());
        }
        ui.with_mut(|s| s.auto_muted = false);
    };

    let mut attempt_play = move |expected_gen: u32, from_user: bool| {
        let Some(v) = video() else { return };
        if from_user && auto_muted() {
            auto_muted.set(false);
            v.set_muted(muted());
            ui.with_mut(|s| s.auto_muted = false);
        }
        v.play_checked(move |outcome| {
            if closing() || play_gen() != expected_gen || desired_paused() {
                return;
            }
            match outcome {
                PlayOutcome::Ok | PlayOutcome::Aborted | PlayOutcome::Error => {}
                PlayOutcome::NotAllowed => {
                    let Some(vid) = video() else { return };
                    if !muted() && !auto_muted() {
                        auto_muted.set(true);
                        vid.set_muted(true);
                        ui.with_mut(|s| s.auto_muted = true);
                        vid.play_checked(move |muted_outcome| {
                            if closing() || play_gen() != expected_gen || desired_paused() {
                                return;
                            }
                            if matches!(muted_outcome, PlayOutcome::NotAllowed | PlayOutcome::Error) {
                                auto_muted.set(false);
                                if let Some(v2) = video() {
                                    v2.set_muted(muted());
                                }
                                let alone = sync_state.peek().as_ref().is_none_or(|st| st.members.len() <= 1);
                                if alone {
                                    desired_paused.set(true);
                                }
                                ui.with_mut(|s| {
                                    s.auto_muted = false;
                                    s.paused = true;
                                    s.waiting = false;
                                    s.status = None;
                                    s.progress = None;
                                });
                            }
                        });
                    } else {
                        let alone = sync_state.peek().as_ref().is_none_or(|st| st.members.len() <= 1);
                        if alone {
                            desired_paused.set(true);
                        }
                        ui.with_mut(|s| {
                            s.paused = true;
                            s.waiting = false;
                            s.status = None;
                            s.progress = None;
                        });
                    }
                }
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
        let native = platform::video::supports_native() && !force_transcode();
        let transcode = !native && (force_transcode() || !i.plays_directly(audio));
        let (src, off) = if transcode {
            last_reload_ms.set(platform::now_ms());
            pending_seek.set(None);
            seeking_target.set(None);
            (i.transcoded_url(audio, at), at.floor())
        } else {
            pending_seek.set((!native && at > 0.0).then_some(at));
            seeking_target.set(None);
            (i.stream.clone(), 0.0)
        };
        if transcode && ui.peek().src == src {
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
        if native && ui.peek().native_video && ui.peek().src == src {
            platform::video::native_set_audio(audio);
            if (ui.peek().position - at).abs() > 1.5 {
                if let Some(v) = video() {
                    awaiting_ready.set(true);
                    seeking_target.set(Some(at));
                    v.set_current_time(at);
                    ui.with_mut(|s| s.position = at);
                }
            }
            return;
        }
        let hold = sync_state.peek().as_ref().is_some_and(|st| st.paused || st.waiting);
        desired_paused.set(hold);
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
        stall_secs.set(0);
        played.set(false);
        let sub_track = if i.preloaded || native {
            ui.peek().subtitle_track
        } else {
            None
        };
        if native {
            platform::video::native_open(&src, at, f64::from(rate()), audio, sub_track, !hold);
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

    let mut mark_media_ready = move || {
        awaiting_ready.set(false);
        stall_secs.set(0);
        waiting_gen.set(waiting_gen() + 1);
        let st_opt = sync_state.peek().clone();
        let paused_by_user = st_opt.as_ref().is_some_and(|st| st.paused);
        let waiting_peer = st_opt.as_ref().is_some_and(|st| st.waiting);
        let hold = paused_by_user || waiting_peer;
        desired_paused.set(hold);
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
        } else {
            ui.with_mut(|s| {
                s.waiting = false;
                s.paused = false;
                s.progress = None;
                s.status = None;
            });
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

    // Fetches `/api/play` for `(season, episode, rel)` and loads the resulting stream.
    let start_play_fetch = {
        let token = token.clone();
        move |season: i32, episode: i32, rel: ReleaseChoiceUpdate, initial_pos: f64, my_gen: u32| {
            let t = token.clone();
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
                s.menu = None;
                s.position = initial_pos;
                s.duration = None;
                s.waiting = false;
                s.release = release_label(&rel);
                s.subtitle = (kind == MediaKind::Tv).then(|| episode_code(season, episode));
                s.status = Some("Подключаемся к раздаче…".into());
                s.progress = None;
            });
            stop_video_element();

            spawn(async move {
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
                connecting.set(false);
                match res {
                    Ok(i) => {
                        let synced_audio = sync_state
                            .peek()
                            .as_ref()
                            .and_then(|st| st.audio_track.as_deref())
                            .and_then(|lbl| i.audio.iter().find(|a| a.label() == lbl))
                            .map(|a| a.index)
                            .or(i.audio_track);
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
                            s.next = i.next.map(|(s, e)| episode_code(s, e));
                            s.preloaded = i.preloaded;
                        });
                        let at = sync_state
                            .peek()
                            .as_ref()
                            .map(|st| clock.peek().expected_position(st, platform::now_ms()))
                            .unwrap_or_else(|| f64::from(i.start_at));
                        last_saved.set(at);
                        info.set(Some(i));
                        load_source(at);
                    }
                    Err(e) => {
                        // Do not keep another peer blocked if this device failed to load the stream.
                        sync::send(&SyncClientMsg::Ready {
                            position_sec: 0.0,
                            duration_sec: None,
                            file_index: None,
                        });
                        ui.with_mut(|s| {
                            s.status = None;
                            s.progress = None;
                            s.error = Some(e.message);
                        });
                    }
                }
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
                                                ui.with_mut(|s| {
                                                    s.sync_members = st.members.clone();
                                                    s.sync_waiting_for = st.waiting_for.clone();
                                                    if st.members.len() <= 1 && s.menu == Some(PlayerMenu::Sync) {
                                                        s.menu = None;
                                                    }
                                                });
                                                if st.revision < last_revision() {
                                                    continue;
                                                }
                                                let is_self = st.updated_by.as_deref() == Some(ws_cid.as_str());
                                                last_revision.set(st.revision);
                                                sync_state.set(Some(st.clone()));

                                                let hold = st.paused || st.waiting;
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

                                                if video().is_some() {
                                                    if hold {
                                                        pause_video();
                                                    } else if !awaiting_ready()
                                                        && info.peek().is_some()
                                                        && video().is_some_and(|v| v.paused())
                                                    {
                                                        attempt_play(play_gen(), false);
                                                    }
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
                                                            ui.with_mut(|s| s.audio_track = target_idx);
                                                            load_source(at);
                                                            continue;
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

                            // Reconnect cleanly if the player is still open.
                            sync_connected.set(false);
                            if ws_gen() != my_ws || closing() {
                                break;
                            }
                            platform::sleep(1_500).await;
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
                                    ui.with_mut(|s| {
                                        s.sync_members = rejoin.state.members.clone();
                                        s.sync_waiting_for = rejoin.state.waiting_for.clone();
                                    });
                                    ticket = rejoin.ticket;
                                }
                                Err(_) => {
                                    platform::sleep(2_000).await;
                                }
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

    // While connecting or buffering, shows how far the torrent has got instead of a bare spinner.
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
                    let (busy, preloaded) = {
                        let s = ui.peek();
                        let waiting_for_peer = sync_state
                            .peek()
                            .as_ref()
                            .is_some_and(|st| st.waiting && !awaiting_ready() && !connecting());
                        (
                            s.error.is_none() && (connecting() || (s.waiting && !waiting_for_peer)),
                            s.preloaded,
                        )
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
            if info.peek().is_none() || connecting() {
                continue;
            }
            let Some(v) = video() else { continue };
            let local_pos = offset() + v.current_time();
            let dur = ui.peek().duration;
            sync::send(&SyncClientMsg::Report {
                position_sec: local_pos,
                duration_sec: dur,
                paused: v.paused(),
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

    let mut go_next = {
        let mut save = save.clone();
        let mut begin_playback = begin_playback.clone();
        let choices = start.choices.clone();
        move || {
            let Some((ns, ne)) = info.peek().as_ref().and_then(|i| i.next) else {
                return;
            };
            save(true);
            let (season, _) = target();
            let mut next_rel = None;
            if ns != season {
                match choices.iter().find(|c| c.season == ns) {
                    Some(c) => {
                        release.set(c.clone());
                        next_rel = Some(c.clone());
                    }
                    None => {
                        on_need_release.call((ns, ne));
                        return;
                    }
                }
            }
            target.set((ns, ne));
            sync::send(&SyncClientMsg::Episode {
                season: ns,
                episode: ne,
                release: next_rel,
            });
            begin_playback(false);
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
            let st_opt = sync_state.peek().clone();
            let waiting_peer = st_opt.as_ref().is_some_and(|st| st.waiting);
            let is_paused_now = ui.peek().paused || (!waiting_peer && (desired_paused() || v.paused()));
            if is_paused_now {
                desired_paused.set(false);
                if st_opt.as_ref().is_none_or(|st| st.paused || st.waiting) {
                    sync::send(&SyncClientMsg::Play { position_sec: cur_pos });
                }
                if !waiting_peer {
                    if let Some(st) = st_opt.as_ref().filter(|st| !st.paused && !st.waiting) {
                        let expected = clock.peek().expected_position(st, platform::now_ms());
                        if (cur_pos - expected).abs() > 1.5 && !ui.peek().transcoded {
                            v.set_current_time(expected);
                            ui.with_mut(|s| s.position = expected);
                        }
                    }
                    ui.with_mut(|s| s.paused = false);
                    attempt_play(play_gen(), true);
                }
            } else {
                desired_paused.set(true);
                pause_video();
                ui.with_mut(|s| s.paused = true);
                sync::send(&SyncClientMsg::Pause { position_sec: cur_pos });
                save(false);
            }
        }
    };

    let mut seek_to = move |at: f64| {
        let s = ui.peek().clone();
        let at = match s.duration {
            Some(d) => at.clamp(0.0, (d - 1.0).max(0.0)),
            None => at.max(0.0),
        };
        last_seek_cmd_ms.set(platform::now_ms());
        sync::send(&SyncClientMsg::Seek { position_sec: at });
        if s.transcoded {
            load_source(at);
        } else if let Some(v) = video() {
            if (v.current_time() - at).abs() > 0.25 {
                awaiting_ready.set(true);
                canplay_seen.set(false);
                seeking_target.set(Some(at));
                v.set_current_time(at);
                ui.with_mut(|s| s.position = at);
            } else {
                seeking_target.set(None);
                ui.with_mut(|s| s.position = at);
                if canplay_seen() || v.ready_state() >= 2 {
                    mark_media_ready();
                }
            }
        }
    };

    let on_video = {
        let mut save = save.clone();
        let mut go_next = go_next.clone();
        let err_token = token.clone();
        let mut mark_media_ready = mark_media_ready;
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
                    v.set_muted(muted() || auto_muted());
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
                        if ui.peek().native_video || (v.current_time() - tgt).abs() < 8.0 {
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
                    ui.with_mut(|s| s.paused = false);
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
                        ui.with_mut(|s| s.paused = sync_state.peek().as_ref().is_some_and(|st| st.paused));
                        return;
                    }
                    if !v.paused() || platform::now_ms() - last_prog_pause_ms() < 600 {
                        return;
                    }
                    // External pause trigger (e.g. MediaSession / headphone disconnect).
                    desired_paused.set(true);
                    ui.with_mut(|s| s.paused = true);
                    sync::send(&SyncClientMsg::Pause { position_sec: pos });
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

    let close = Callback::new({
        let mut save = save.clone();
        let token = token.clone();
        let client_id = client_id.clone();
        move |_: ()| {
            if closing() {
                return;
            }
            closing.set(true);
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
            spawn(async move {
                let _ = api::sync_leave(&t, &leave_req).await;
                on_close.call(());
            });
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
            vid.set_muted(is_muted);
        }
    };
    let mut toggle_mute = move || {
        if auto_muted() {
            unmute_auto();
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
    };
    let toggle_fullscreen = move || {
        platform::toggle_fullscreen();
    };

    // Desktop keyboard shortcuts (Space/K, ←/→/J/L, ↑/↓ volume, M mute, F fullscreen, Esc).
    {
        let mut flash_jump = flash_jump;
        let mut set_vol = set_vol;
        let mut toggle_mute = toggle_mute;
        let mut show_controls = show_controls;
        let mut toggle_play = toggle_play.clone();
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
                return;
            }
            save(false);
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
                    } else if finger {
                        if ui.peek().controls {
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
                if let Some(v) = video() {
                    v.set_rate(f64::from(r));
                }
                sync::send(&SyncClientMsg::Rate {
                    rate: r,
                    position_sec: ui.peek().position,
                });
            },
            on_episode: move |n: i32| play_episode(n),
            on_pick_release: move |_| {
                let (season, episode) = target();
                on_need_release.call((season, episode));
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
                }
            },
            on_audio: {
                let save_tracks = save_tracks.clone();
                move |a: i32| {
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
                    save_tracks();
                }
            },
            on_next: move |_| go_next(),
            on_fullscreen: move |_| toggle_fullscreen(),
            on_close: close,
            on_video,
            on_volume: move |v: f32| set_vol(v),
            on_mute: move |_| toggle_mute(),
            on_unmute: move |_| unmute_auto(),
        }
    }
}
