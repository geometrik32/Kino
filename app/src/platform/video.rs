//! The `<video>` of the player.
//!
//! In a browser it is driven directly through the DOM. In the apps the UI lives in
//! a WebView behind an asynchronous bridge, so the state is mirrored here a few times
//! a second and commands are sent as small scripts. A native player (libmpv, ExoPlayer)
//! replaces this module and nothing else.

use kino_ui::VIDEO_ID;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayOutcome {
    Ok,
    NotAllowed,
    Aborted,
    Error,
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{PlayOutcome, VIDEO_ID};
    use wasm_bindgen::JsCast;

    pub struct Video(web_sys::HtmlVideoElement);

    pub fn video() -> Option<Video> {
        web_sys::window()?
            .document()?
            .get_element_by_id(VIDEO_ID)?
            .dyn_into()
            .ok()
            .map(Video)
    }

    impl Video {
        pub fn current_time(&self) -> f64 {
            self.0.current_time()
        }
        /// Length of the loaded source; not finite while unknown.
        pub fn duration(&self) -> f64 {
            self.0.duration()
        }
        pub fn paused(&self) -> bool {
            self.0.paused()
        }
        pub fn ended(&self) -> bool {
            self.0.ended()
        }
        pub fn ready_state(&self) -> u16 {
            self.0.ready_state()
        }
        pub fn buffered_ahead(&self) -> Option<f64> {
            let cur = self.0.current_time();
            let ranges = self.0.buffered();
            for i in 0..ranges.length() {
                let Ok(start) = ranges.start(i) else { continue };
                let Ok(end) = ranges.end(i) else { continue };
                if start <= cur + 0.5 && end >= cur {
                    return Some((end - cur).max(0.0));
                }
            }
            Some(0.0)
        }
        pub fn set_current_time(&self, t: f64) {
            self.0.set_current_time(t);
        }
        pub fn play_checked(&self, on_done: impl FnOnce(PlayOutcome) + 'static) {
            let rt = dioxus::core::Runtime::try_current();
            let scope = rt.as_ref().and_then(|r| r.try_current_scope_id());
            let done = std::rc::Rc::new(std::cell::RefCell::new(Some(
                Box::new(on_done) as Box<dyn FnOnce(PlayOutcome)>
            )));
            let dispatch = move |outcome: PlayOutcome| {
                let Some(cb) = done.borrow_mut().take() else {
                    return;
                };
                match (&rt, scope) {
                    (Some(rt), Some(id)) => rt.in_scope(id, || cb(outcome)),
                    (Some(rt), None) => {
                        let _guard = dioxus::core::RuntimeGuard::new(rt.clone());
                        cb(outcome);
                    }
                    (None, _) => cb(outcome),
                }
            };
            let Ok(promise) = self.0.play() else {
                dispatch(PlayOutcome::Error);
                return;
            };
            let dispatch = std::rc::Rc::new(dispatch);
            let dispatch_err = dispatch.clone();
            let on_ok = wasm_bindgen::closure::Closure::once(move |_: wasm_bindgen::JsValue| {
                dispatch(PlayOutcome::Ok);
            });
            let on_err = wasm_bindgen::closure::Closure::once(move |err: wasm_bindgen::JsValue| {
                let name = js_sys::Reflect::get(&err, &"name".into())
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_default();
                let outcome = match name.as_str() {
                    "NotAllowedError" => PlayOutcome::NotAllowed,
                    "AbortError" => PlayOutcome::Aborted,
                    _ => PlayOutcome::Error,
                };
                dispatch_err(outcome);
            });
            let _ = promise.then2(&on_ok, &on_err);
            on_ok.forget();
            on_err.forget();
        }
        pub fn pause(&self) {
            let _ = self.0.pause();
        }
        /// Playback speed (1.0 is normal).
        pub fn set_rate(&self, rate: f64) {
            self.0.set_default_playback_rate(rate);
            self.0.set_playback_rate(rate);
        }
        pub fn set_volume(&self, vol: f64) {
            self.0.set_volume(vol.clamp(0.0, 1.0));
        }
        pub fn set_muted(&self, muted: bool) {
            self.0.set_muted(muted);
        }
    }

    pub fn supports_native() -> bool {
        false
    }
    #[allow(clippy::too_many_arguments)]
    pub fn native_open(
        _url: &str,
        _title: &str,
        _next: Option<&str>,
        _start_sec: f64,
        _rate: f64,
        _audio: Option<i32>,
        _subtitle: Option<i32>,
        _autoplay: bool,
        _scale_mode: u8,
    ) {
    }
    pub fn native_set_audio(_audio: Option<i32>) {}
    pub fn native_set_subtitle(_subtitle: Option<i32>) {}
    pub fn native_set_scale(_mode: u8) {}
    pub fn native_scale_mode() -> u8 {
        1
    }
    pub fn native_close() {}

    /// Starts mirroring (nothing to do in a browser).
    pub fn use_bridge() {}

    /// Native player polling loop (not used in browser).
    pub fn use_native_events(_on_video: dioxus::prelude::Callback<&'static str>) {}
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Mutex,
    };

    use dioxus::{document::eval, prelude::*};

    use super::{PlayOutcome, VIDEO_ID};

    #[cfg(any(target_os = "android", all(feature = "desktop", not(target_os = "android"))))]
    use std::{
        sync::atomic::AtomicU64,
        time::{Duration, Instant},
    };

    #[cfg(all(feature = "desktop", not(target_os = "android")))]
    use std::collections::VecDeque;

    #[derive(Clone, Copy)]
    struct Snapshot {
        time: f64,
        duration: f64,
        paused: bool,
        ready_state: u16,
        buffered_ahead: f64,
    }

    static SNAPSHOT: Mutex<Snapshot> = Mutex::new(Snapshot {
        time: 0.0,
        duration: f64::NAN,
        paused: true,
        ready_state: 0,
        buffered_ahead: f64::NAN,
    });

    static NATIVE_ACTIVE: AtomicBool = AtomicBool::new(false);
    static NATIVE_SCALE_MODE: AtomicU8 = AtomicU8::new(1);

    #[cfg(any(target_os = "android", all(feature = "desktop", not(target_os = "android"))))]
    static NATIVE_SEQ: AtomicU64 = AtomicU64::new(0);

    #[cfg(all(feature = "desktop", not(target_os = "android")))]
    struct DesktopMpv {
        session: Option<kino_mpv::Session>,
        events: VecDeque<&'static str>,
        seq: u64,
        paused: bool,
        buffering: bool,
        ready: bool,
        ignore_paused_until: Option<Instant>,
        last_volume: f64,
        last_muted: bool,
        last_rate: f64,
    }

    #[cfg(all(feature = "desktop", not(target_os = "android")))]
    static DESKTOP_MPV: Mutex<DesktopMpv> = Mutex::new(DesktopMpv {
        session: None,
        events: VecDeque::new(),
        seq: 0,
        paused: true,
        buffering: false,
        ready: false,
        ignore_paused_until: None,
        last_volume: 1.0,
        last_muted: false,
        last_rate: 1.0,
    });

    #[cfg(target_os = "android")]
    #[derive(Clone, Copy, Default)]
    struct PollState {
        seq: u64,
        seek_seq: u64,
        metadata_sent: bool,
        ready_sent: bool,
        played_sent: bool,
        waiting: bool,
        paused: bool,
        ended_sent: bool,
        error_sent: bool,
        last_scale_mode: u8,
        last_time_sec: f64,
        ignore_paused_until: Option<Instant>,
        ignore_seek_until: Option<Instant>,
    }

    #[cfg(target_os = "android")]
    static POLL_STATE: Mutex<PollState> = Mutex::new(PollState {
        seq: 0,
        seek_seq: 0,
        metadata_sent: false,
        ready_sent: false,
        played_sent: false,
        waiting: false,
        paused: true,
        ended_sent: false,
        error_sent: false,
        last_scale_mode: 0,
        last_time_sec: -1.0,
        ignore_paused_until: None,
        ignore_seek_until: None,
    });

    pub struct Video;

    pub fn video() -> Option<Video> {
        Some(Video)
    }

    pub fn supports_native() -> bool {
        #[cfg(target_os = "android")]
        {
            true
        }
        #[cfg(all(feature = "desktop", not(target_os = "android")))]
        {
            kino_mpv::is_available()
        }
        #[cfg(not(any(target_os = "android", feature = "desktop")))]
        {
            false
        }
    }

    pub fn is_native_active() -> bool {
        NATIVE_ACTIVE.load(Ordering::Relaxed)
    }

    #[cfg(target_os = "android")]
    #[allow(clippy::too_many_arguments)]
    pub fn native_open(
        url: &str,
        _title: &str,
        _next: Option<&str>,
        start_sec: f64,
        rate: f64,
        audio: Option<i32>,
        subtitle: Option<i32>,
        autoplay: bool,
        scale_mode: u8,
    ) {
        let start = start_sec.max(0.0);
        *SNAPSHOT.lock().expect("video snapshot") = Snapshot {
            time: start,
            duration: f64::NAN,
            paused: !autoplay,
            ready_state: 0,
            buffered_ahead: f64::NAN,
        };
        NATIVE_SCALE_MODE.store(if scale_mode == 2 { 2 } else { 1 }, Ordering::Relaxed);
        let seq = super::super::android::native_open(url, start, rate as f32, audio, subtitle, autoplay, scale_mode);
        NATIVE_SEQ.store(seq, Ordering::Relaxed);
        *POLL_STATE.lock().expect("poll state") = PollState {
            seq,
            paused: !autoplay,
            last_time_sec: -1.0,
            ..PollState::default()
        };
        NATIVE_ACTIVE.store(true, Ordering::Relaxed);
    }

    #[cfg(all(feature = "desktop", not(target_os = "android")))]
    #[allow(clippy::too_many_arguments)]
    pub fn native_open(
        url: &str,
        title: &str,
        next: Option<&str>,
        start_sec: f64,
        rate: f64,
        audio: Option<i32>,
        subtitle: Option<i32>,
        autoplay: bool,
        scale_mode: u8,
    ) {
        let start = start_sec.max(0.0);
        let eff_scale = if scale_mode == 2 { 2 } else { 1 };
        *SNAPSHOT.lock().expect("video snapshot") = Snapshot {
            time: start,
            duration: f64::NAN,
            paused: !autoplay,
            ready_state: 0,
            buffered_ahead: f64::NAN,
        };
        NATIVE_SCALE_MODE.store(eff_scale, Ordering::Relaxed);
        let seq = NATIVE_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
        let full_url = if url.starts_with('/') {
            super::super::abs(url)
        } else {
            url.to_owned()
        };

        let (opts, existing) = {
            let mut m = DESKTOP_MPV.lock().expect("desktop mpv");
            m.seq = seq;
            m.events.clear();
            m.paused = !autoplay;
            m.buffering = false;
            m.ready = false;
            m.ignore_paused_until = Some(Instant::now() + Duration::from_millis(600));
            m.last_rate = rate;
            let opts = kino_mpv::PlayOptions {
                url: full_url,
                title: title.to_owned(),
                start,
                audio: audio.and_then(|i| usize::try_from(i).ok()),
                subtitle: subtitle.and_then(|i| usize::try_from(i).ok()),
                next: next.map(str::to_owned),
                autoplay,
                rate,
                volume: m.last_volume,
                muted: m.last_muted,
                scale_mode: eff_scale,
            };
            let existing = m.session.take();
            (opts, existing)
        };

        NATIVE_ACTIVE.store(true, Ordering::Relaxed);

        if let Some(sess) = existing {
            if let Err(err) = sess.load(opts) {
                eprintln!("mpv load failed: {err}");
                NATIVE_ACTIVE.store(false, Ordering::Relaxed);
                DESKTOP_MPV.lock().expect("desktop mpv").events.push_back("error");
            } else {
                DESKTOP_MPV.lock().expect("desktop mpv").session = Some(sess);
            }
            return;
        }

        match kino_mpv::Session::start(opts, false, move |ev| {
            let mut m = DESKTOP_MPV.lock().expect("desktop mpv");
            if m.seq != seq {
                return;
            }
            match ev {
                kino_mpv::PlayerEvent::Loaded { duration, .. } => {
                    m.ready = true;
                    m.buffering = false;
                    let mut snap = SNAPSHOT.lock().expect("video snapshot");
                    snap.duration = duration.unwrap_or(f64::NAN);
                    snap.ready_state = 4;
                    m.events.push_back("metadata");
                    m.events.push_back("canplay");
                    if !m.paused {
                        m.events.push_back("playing");
                    }
                }
                kino_mpv::PlayerEvent::Position { position, duration } => {
                    let mut snap = SNAPSHOT.lock().expect("video snapshot");
                    snap.time = position;
                    if let Some(d) = duration {
                        snap.duration = d;
                    }
                    if m.ready {
                        m.events.push_back("time");
                    }
                }
                kino_mpv::PlayerEvent::Paused(p) => {
                    let guarded = m.ignore_paused_until.is_some_and(|u| Instant::now() < u);
                    if guarded {
                        if p == m.paused {
                            m.ignore_paused_until = None;
                        }
                        return;
                    }
                    m.ignore_paused_until = None;
                    if p != m.paused {
                        m.paused = p;
                        SNAPSHOT.lock().expect("video snapshot").paused = p;
                        m.events.push_back(if p { "pause" } else { "play" });
                    }
                }
                kino_mpv::PlayerEvent::Buffering(b) => {
                    if b != m.buffering {
                        m.buffering = b;
                        SNAPSHOT.lock().expect("video snapshot").ready_state = if b { 1 } else { 4 };
                        let ev = if b {
                            "waiting"
                        } else if !m.paused {
                            "playing"
                        } else {
                            "canplay"
                        };
                        m.events.push_back(ev);
                    }
                }
                kino_mpv::PlayerEvent::Seeked => {
                    m.events.push_back("seeked");
                }
                kino_mpv::PlayerEvent::NextRequested => {
                    m.events.push_back("next");
                }
                kino_mpv::PlayerEvent::Ended => {
                    SNAPSHOT.lock().expect("video snapshot").paused = true;
                    m.paused = true;
                    m.events.push_back("ended");
                }
                kino_mpv::PlayerEvent::Closed { .. } => {
                    m.session = None;
                    NATIVE_ACTIVE.store(false, Ordering::Relaxed);
                    m.events.push_back("closed");
                }
                kino_mpv::PlayerEvent::Error(err) => {
                    eprintln!("mpv playback error: {err}");
                    NATIVE_ACTIVE.store(false, Ordering::Relaxed);
                    m.events.push_back("error");
                }
                kino_mpv::PlayerEvent::Tracks { .. } => {}
            }
        }) {
            Ok(sess) => {
                let mut m = DESKTOP_MPV.lock().expect("desktop mpv");
                if m.seq == seq {
                    m.session = Some(sess);
                } else {
                    sess.close();
                }
            }
            Err(err) => {
                eprintln!("mpv start failed: {err}");
                NATIVE_ACTIVE.store(false, Ordering::Relaxed);
                DESKTOP_MPV.lock().expect("desktop mpv").events.push_back("error");
            }
        }
    }

    #[cfg(not(any(target_os = "android", feature = "desktop")))]
    #[allow(clippy::too_many_arguments)]
    pub fn native_open(
        _url: &str,
        _title: &str,
        _next: Option<&str>,
        _start_sec: f64,
        _rate: f64,
        _audio: Option<i32>,
        _subtitle: Option<i32>,
        _autoplay: bool,
        _scale_mode: u8,
    ) {
    }

    pub fn native_set_audio(audio: Option<i32>) {
        #[cfg(target_os = "android")]
        if is_native_active() {
            super::super::android::native_select_audio(audio);
        }
        #[cfg(all(feature = "desktop", not(target_os = "android")))]
        if is_native_active() {
            if let Some(sess) = DESKTOP_MPV.lock().expect("desktop mpv").session.as_ref() {
                sess.set_audio(audio.and_then(|i| usize::try_from(i).ok()));
            }
        }
        #[cfg(not(any(target_os = "android", feature = "desktop")))]
        let _ = audio;
    }

    pub fn native_set_subtitle(subtitle: Option<i32>) {
        #[cfg(target_os = "android")]
        if is_native_active() {
            super::super::android::native_select_subtitle(subtitle);
        }
        #[cfg(all(feature = "desktop", not(target_os = "android")))]
        if is_native_active() {
            if let Some(sess) = DESKTOP_MPV.lock().expect("desktop mpv").session.as_ref() {
                sess.set_subtitle(subtitle.and_then(|i| usize::try_from(i).ok()));
            }
        }
        #[cfg(not(any(target_os = "android", feature = "desktop")))]
        let _ = subtitle;
    }

    pub fn native_set_scale(mode: u8) {
        if mode == 1 || mode == 2 {
            NATIVE_SCALE_MODE.store(mode, Ordering::Relaxed);
        }
        #[cfg(target_os = "android")]
        if is_native_active() {
            super::super::android::native_set_scale(mode);
        }
        #[cfg(all(feature = "desktop", not(target_os = "android")))]
        if is_native_active() {
            if let Some(sess) = DESKTOP_MPV.lock().expect("desktop mpv").session.as_ref() {
                sess.set_scale(mode);
            }
        }
    }

    pub fn native_scale_mode() -> u8 {
        NATIVE_SCALE_MODE.load(Ordering::Relaxed)
    }

    pub fn native_close() {
        let was_active = NATIVE_ACTIVE.swap(false, Ordering::Relaxed);
        #[cfg(target_os = "android")]
        {
            NATIVE_SEQ.store(0, Ordering::Relaxed);
            if was_active {
                super::super::android::native_close();
            }
        }
        #[cfg(all(feature = "desktop", not(target_os = "android")))]
        {
            let _ = was_active;
            let sess = {
                let mut m = DESKTOP_MPV.lock().expect("desktop mpv");
                m.seq = NATIVE_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
                m.events.clear();
                m.session.take()
            };
            if let Some(s) = sess {
                s.close();
            }
        }
        #[cfg(not(any(target_os = "android", feature = "desktop")))]
        let _ = was_active;
    }

    fn snapshot() -> Snapshot {
        *SNAPSHOT.lock().expect("video snapshot")
    }

    fn run(js: String) {
        let _ = eval(&js);
    }

    impl Video {
        pub fn current_time(&self) -> f64 {
            snapshot().time
        }
        pub fn duration(&self) -> f64 {
            snapshot().duration
        }
        pub fn paused(&self) -> bool {
            snapshot().paused
        }
        pub fn ended(&self) -> bool {
            false
        }
        pub fn ready_state(&self) -> u16 {
            snapshot().ready_state
        }
        pub fn buffered_ahead(&self) -> Option<f64> {
            #[cfg(any(target_os = "android", all(feature = "desktop", not(target_os = "android"))))]
            if is_native_active() {
                return None;
            }
            let buf = snapshot().buffered_ahead;
            buf.is_finite().then_some(buf.max(0.0))
        }
        pub fn set_current_time(&self, t: f64) {
            if !t.is_finite() {
                return;
            }
            let t = t.max(0.0);
            SNAPSHOT.lock().expect("video snapshot").time = t;
            #[cfg(target_os = "android")]
            if is_native_active() {
                {
                    let mut st = POLL_STATE.lock().expect("poll state");
                    st.ignore_seek_until = Some(Instant::now() + Duration::from_millis(500));
                }
                super::super::android::native_seek(t);
                return;
            }
            #[cfg(all(feature = "desktop", not(target_os = "android")))]
            if is_native_active() {
                if let Some(sess) = DESKTOP_MPV.lock().expect("desktop mpv").session.as_ref() {
                    sess.seek(t);
                }
                return;
            }
            run(format!(
                "var v=document.getElementById('{VIDEO_ID}'); if(v && typeof v.play==='function') v.currentTime={t};"
            ));
        }
        pub fn play_checked(&self, on_done: impl FnOnce(PlayOutcome) + 'static) {
            SNAPSHOT.lock().expect("video snapshot").paused = false;
            #[cfg(target_os = "android")]
            if is_native_active() {
                {
                    let mut st = POLL_STATE.lock().expect("poll state");
                    st.paused = false;
                    st.ended_sent = false;
                    st.ignore_paused_until = Some(Instant::now() + Duration::from_millis(500));
                }
                super::super::android::native_play();
                on_done(PlayOutcome::Ok);
                return;
            }
            #[cfg(all(feature = "desktop", not(target_os = "android")))]
            if is_native_active() {
                {
                    let mut m = DESKTOP_MPV.lock().expect("desktop mpv");
                    m.paused = false;
                    m.ignore_paused_until = Some(Instant::now() + Duration::from_millis(500));
                    if let Some(sess) = m.session.as_ref() {
                        sess.pause(false);
                    }
                }
                on_done(PlayOutcome::Ok);
                return;
            }
            spawn(async move {
                let res = eval(&format!(
                    "var v=document.getElementById('{VIDEO_ID}'); if(!v || typeof v.play!=='function') return 'none'; \
                     return await v.play().then(function(){{return 'ok';}}, function(e){{return (e&&e.name)||'err';}});"
                ))
                .join::<String>()
                .await
                .unwrap_or_else(|_| "err".into());
                let outcome = match res.as_str() {
                    "ok" => PlayOutcome::Ok,
                    "NotAllowedError" => PlayOutcome::NotAllowed,
                    "AbortError" => PlayOutcome::Aborted,
                    _ => PlayOutcome::Error,
                };
                if outcome != PlayOutcome::Ok {
                    SNAPSHOT.lock().expect("video snapshot").paused = true;
                }
                on_done(outcome);
            });
        }
        pub fn pause(&self) {
            SNAPSHOT.lock().expect("video snapshot").paused = true;
            #[cfg(target_os = "android")]
            if is_native_active() {
                {
                    let mut st = POLL_STATE.lock().expect("poll state");
                    st.paused = true;
                    st.ignore_paused_until = Some(Instant::now() + Duration::from_millis(500));
                }
                super::super::android::native_pause();
                return;
            }
            #[cfg(all(feature = "desktop", not(target_os = "android")))]
            if is_native_active() {
                let mut m = DESKTOP_MPV.lock().expect("desktop mpv");
                m.paused = true;
                m.ignore_paused_until = Some(Instant::now() + Duration::from_millis(500));
                if let Some(sess) = m.session.as_ref() {
                    sess.pause(true);
                }
                return;
            }
            run(format!(
                "var v=document.getElementById('{VIDEO_ID}'); if(v && typeof v.pause==='function') v.pause();"
            ));
        }
        /// Playback speed (1.0 is normal).
        pub fn set_rate(&self, rate: f64) {
            if !rate.is_finite() {
                return;
            }
            let rate = rate.clamp(0.25, 4.0);
            #[cfg(target_os = "android")]
            if is_native_active() {
                super::super::android::native_set_rate(rate as f32);
                return;
            }
            #[cfg(all(feature = "desktop", not(target_os = "android")))]
            {
                let mut m = DESKTOP_MPV.lock().expect("desktop mpv");
                m.last_rate = rate;
                if is_native_active() {
                    if let Some(sess) = m.session.as_ref() {
                        sess.set_rate(rate);
                    }
                    return;
                }
            }
            run(format!(
                "var v=document.getElementById('{VIDEO_ID}'); if(v && typeof v.play==='function') {{ v.defaultPlaybackRate={rate}; v.playbackRate={rate}; }}"
            ));
        }
        pub fn set_volume(&self, vol: f64) {
            let vol = if vol.is_finite() { vol.clamp(0.0, 1.0) } else { 1.0 };
            #[cfg(target_os = "android")]
            if is_native_active() {
                super::super::android::native_set_volume(vol as f32);
                return;
            }
            #[cfg(all(feature = "desktop", not(target_os = "android")))]
            {
                let mut m = DESKTOP_MPV.lock().expect("desktop mpv");
                m.last_volume = vol;
                if is_native_active() {
                    if let Some(sess) = m.session.as_ref() {
                        sess.set_volume(vol);
                    }
                    return;
                }
            }
            run(format!(
                "var v=document.getElementById('{VIDEO_ID}'); if(v && typeof v.play==='function') v.volume={vol};"
            ));
        }
        pub fn set_muted(&self, muted: bool) {
            #[cfg(target_os = "android")]
            if is_native_active() {
                super::super::android::native_set_muted(muted);
                return;
            }
            #[cfg(all(feature = "desktop", not(target_os = "android")))]
            {
                let mut m = DESKTOP_MPV.lock().expect("desktop mpv");
                m.last_muted = muted;
                if is_native_active() {
                    if let Some(sess) = m.session.as_ref() {
                        sess.set_muted(muted);
                    }
                    return;
                }
            }
            run(format!(
                "var v=document.getElementById('{VIDEO_ID}'); if(v && typeof v.play==='function') v.muted={muted};"
            ));
        }
    }

    struct VideoBridgeGuard;
    impl Drop for VideoBridgeGuard {
        fn drop(&mut self) {
            run(
                "if (window.__kinoVideoBridgeTimer) { clearInterval(window.__kinoVideoBridgeTimer); window.__kinoVideoBridgeTimer = null; }"
                    .into(),
            );
        }
    }

    /// Reads the position and state of the `<video>` four times a second.
    pub fn use_bridge() {
        use_hook(|| {
            spawn(async move {
                let mut bridge = eval(&format!(
                    "if (window.__kinoVideoBridgeTimer) {{ clearInterval(window.__kinoVideoBridgeTimer); }} \
                     window.__kinoVideoBridgeTimer = setInterval(function () {{ \
                       var v = document.getElementById('{VIDEO_ID}'); \
                       if (v && typeof v.play === 'function') {{ \
                         var buf = -1; \
                         if (v.buffered && v.buffered.length) {{ \
                           var t = v.currentTime || 0; \
                           for (var i = 0; i < v.buffered.length; i++) {{ \
                             if (v.buffered.start(i) <= t + 0.5 && v.buffered.end(i) >= t) {{ \
                               buf = v.buffered.end(i) - t; \
                               break; \
                             }} \
                           }} \
                         }} \
                         dioxus.send([v.currentTime, isFinite(v.duration) ? v.duration : -1, v.paused, v.readyState, buf]); \
                       }} \
                     }}, 250);"
                ));
                while let Ok((time, duration, paused, ready_state, buffered_ahead)) =
                    bridge.recv::<(f64, f64, bool, u16, f64)>().await
                {
                    if !is_native_active() {
                        *SNAPSHOT.lock().expect("video snapshot") = Snapshot {
                            time,
                            duration: if duration < 0.0 { f64::NAN } else { duration },
                            paused,
                            ready_state,
                            buffered_ahead: if buffered_ahead < 0.0 { f64::NAN } else { buffered_ahead },
                        };
                    }
                }
            });
            std::rc::Rc::new(VideoBridgeGuard)
        });
    }

    #[cfg(target_os = "android")]
    fn poll_native_once() -> Vec<&'static str> {
        let mut events = Vec::new();
        if !is_native_active() {
            return events;
        }
        let expected_seq = NATIVE_SEQ.load(Ordering::Relaxed);
        if expected_seq == 0 {
            return events;
        }
        let Some(snap) = super::super::android::poll_native_player() else {
            return events;
        };
        if snap.open_seq != expected_seq {
            return events;
        }

        let now = Instant::now();
        let mut st = POLL_STATE.lock().expect("poll state");
        if st.seq != expected_seq {
            return events;
        }

        let mut cur = SNAPSHOT.lock().expect("video snapshot");
        cur.duration = snap.duration_sec;
        cur.ready_state = if snap.ready && !snap.buffering { 4 } else { 1 };

        let seeked = snap.seek_seq != st.seek_seq;
        if seeked {
            st.seek_seq = snap.seek_seq;
            st.ignore_seek_until = None;
        }
        let seek_guarded = st.ignore_seek_until.is_some_and(|u| now < u);
        if !seek_guarded {
            st.ignore_seek_until = None;
            cur.time = snap.position_sec;
        }

        let pause_guarded = match st.ignore_paused_until {
            Some(until) if now < until => {
                if snap.paused == st.paused {
                    st.ignore_paused_until = None;
                    false
                } else {
                    true
                }
            }
            _ => {
                st.ignore_paused_until = None;
                false
            }
        };

        if pause_guarded {
            cur.paused = st.paused;
        } else {
            cur.paused = snap.paused;
        }

        if snap.error {
            if !st.error_sent {
                st.error_sent = true;
                events.push("error");
            }
            return events;
        }

        if seeked {
            events.push("seeked");
        }

        if (snap.ready || snap.duration_sec.is_finite()) && !st.metadata_sent {
            st.metadata_sent = true;
            events.push("metadata");
        }

        if snap.ready && !st.ready_sent {
            st.ready_sent = true;
            st.waiting = false;
            events.push("canplay");
        }

        if st.ready_sent && snap.buffering && !st.waiting && !snap.ended {
            st.waiting = true;
            events.push("waiting");
        }

        if snap.ready && !snap.buffering && !snap.ended {
            if !snap.paused && (!st.played_sent || st.waiting) {
                st.played_sent = true;
                st.waiting = false;
                events.push("playing");
            } else if snap.paused && st.waiting {
                st.waiting = false;
                events.push("canplay");
            }
        }

        if !pause_guarded && snap.paused != st.paused && !snap.ended {
            st.paused = snap.paused;
            events.push(if snap.paused { "pause" } else { "play" });
        }

        if st.ready_sent && !seek_guarded && (snap.position_sec - st.last_time_sec).abs() >= 0.15 && !snap.ended {
            st.last_time_sec = snap.position_sec;
            events.push("time");
        }

        if !snap.ended && st.ended_sent {
            st.ended_sent = false;
        }
        if snap.ended && !st.ended_sent {
            st.ended_sent = true;
            cur.paused = true;
            events.push("ended");
        }

        if (snap.scale_mode == 1 || snap.scale_mode == 2) && snap.scale_mode != st.last_scale_mode {
            st.last_scale_mode = snap.scale_mode;
            NATIVE_SCALE_MODE.store(snap.scale_mode, Ordering::Relaxed);
            events.push("scale");
        }

        events
    }

    /// Polls native player (Android ExoPlayer or Desktop libmpv) and forwards transitions to `on_video`.
    pub fn use_native_events(on_video: Callback<&'static str>) {
        #[cfg(target_os = "android")]
        use_hook(|| {
            spawn(async move {
                loop {
                    let ms = if is_native_active() { 200 } else { 400 };
                    super::super::sleep(ms).await;
                    if !is_native_active() {
                        continue;
                    }
                    for ev in poll_native_once() {
                        on_video.call(ev);
                    }
                }
            });
        });
        #[cfg(all(feature = "desktop", not(target_os = "android")))]
        use_hook(|| {
            spawn(async move {
                loop {
                    let ms = if is_native_active() { 150 } else { 400 };
                    super::super::sleep(ms).await;
                    let pending: Vec<&'static str> = {
                        let mut m = DESKTOP_MPV.lock().expect("desktop mpv");
                        m.events.drain(..).collect()
                    };
                    for ev in pending {
                        on_video.call(ev);
                    }
                }
            });
        });
        #[cfg(not(any(target_os = "android", feature = "desktop")))]
        let _ = on_video;
    }
}

pub use imp::{
    native_close, native_open, native_scale_mode, native_set_audio, native_set_scale, native_set_subtitle,
    supports_native, use_bridge, use_native_events, video,
};
