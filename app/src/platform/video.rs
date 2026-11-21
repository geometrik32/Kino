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
        pub fn ready_state(&self) -> u16 {
            self.0.ready_state()
        }
        pub fn set_current_time(&self, t: f64) {
            self.0.set_current_time(t);
        }
        pub fn play_checked(&self, on_done: impl FnOnce(PlayOutcome) + 'static) {
            let Ok(promise) = self.0.play() else {
                on_done(PlayOutcome::Error);
                return;
            };
            let done = std::rc::Rc::new(std::cell::RefCell::new(Some(on_done)));
            let done_err = done.clone();
            let on_ok = wasm_bindgen::closure::Closure::once(move |_: wasm_bindgen::JsValue| {
                if let Some(cb) = done.borrow_mut().take() {
                    cb(PlayOutcome::Ok);
                }
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
                if let Some(cb) = done_err.borrow_mut().take() {
                    cb(outcome);
                }
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

    /// Starts mirroring (nothing to do in a browser).
    pub fn use_bridge() {}
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::sync::Mutex;

    use dioxus::{document::eval, prelude::*};

    use super::{PlayOutcome, VIDEO_ID};

    #[derive(Clone, Copy)]
    struct Snapshot {
        time: f64,
        duration: f64,
        paused: bool,
        ready_state: u16,
    }

    static SNAPSHOT: Mutex<Snapshot> = Mutex::new(Snapshot {
        time: 0.0,
        duration: f64::NAN,
        paused: true,
        ready_state: 0,
    });

    pub struct Video;

    pub fn video() -> Option<Video> {
        Some(Video)
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
        pub fn ready_state(&self) -> u16 {
            snapshot().ready_state
        }
        pub fn set_current_time(&self, t: f64) {
            SNAPSHOT.lock().expect("video snapshot").time = t;
            run(format!(
                "var v=document.getElementById('{VIDEO_ID}'); if(v) v.currentTime={t};"
            ));
        }
        pub fn play_checked(&self, on_done: impl FnOnce(PlayOutcome) + 'static) {
            SNAPSHOT.lock().expect("video snapshot").paused = false;
            spawn(async move {
                let res = eval(&format!(
                    "var v=document.getElementById('{VIDEO_ID}'); if(!v) return 'none'; \
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
            run(format!("var v=document.getElementById('{VIDEO_ID}'); if(v) v.pause();"));
        }
        /// Playback speed (1.0 is normal).
        pub fn set_rate(&self, rate: f64) {
            run(format!(
                "var v=document.getElementById('{VIDEO_ID}'); if(v) {{ v.defaultPlaybackRate={rate}; v.playbackRate={rate}; }}"
            ));
        }
        pub fn set_volume(&self, vol: f64) {
            let vol = vol.clamp(0.0, 1.0);
            run(format!(
                "var v=document.getElementById('{VIDEO_ID}'); if(v) v.volume={vol};"
            ));
        }
        pub fn set_muted(&self, muted: bool) {
            run(format!(
                "var v=document.getElementById('{VIDEO_ID}'); if(v) v.muted={muted};"
            ));
        }
    }

    /// Reads the position and state of the `<video>` four times a second.
    pub fn use_bridge() {
        use_hook(|| {
            spawn(async move {
                let mut bridge = eval(&format!(
                    "setInterval(function () {{ \
                       var v = document.getElementById('{VIDEO_ID}'); \
                       if (v) dioxus.send([v.currentTime, isFinite(v.duration) ? v.duration : -1, v.paused, v.readyState]); \
                     }}, 250);"
                ));
                while let Ok((time, duration, paused, ready_state)) = bridge.recv::<(f64, f64, bool, u16)>().await {
                    *SNAPSHOT.lock().expect("video snapshot") = Snapshot {
                        time,
                        duration: if duration < 0.0 { f64::NAN } else { duration },
                        paused,
                        ready_state,
                    };
                }
            });
        });
    }
}

pub use imp::{use_bridge, video};
