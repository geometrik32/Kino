//! The `<video>` of the player.
//!
//! In a browser it is driven directly through the DOM. In the apps the UI lives in
//! a WebView behind an asynchronous bridge, so the state is mirrored here a few times
//! a second and commands are sent as small scripts. A native player (libmpv, ExoPlayer)
//! replaces this module and nothing else.

use kino_ui::VIDEO_ID;

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::VIDEO_ID;
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
        pub fn set_current_time(&self, t: f64) {
            self.0.set_current_time(t);
        }
        pub fn play(&self) {
            let _ = self.0.play();
        }
        pub fn pause(&self) {
            let _ = self.0.pause();
        }
    }

    /// Starts mirroring (nothing to do in a browser).
    pub fn use_bridge() {}
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::sync::Mutex;

    use dioxus::{document::eval, prelude::*};

    use super::VIDEO_ID;

    #[derive(Clone, Copy)]
    struct Snapshot {
        time: f64,
        duration: f64,
        paused: bool,
    }

    static SNAPSHOT: Mutex<Snapshot> = Mutex::new(Snapshot {
        time: 0.0,
        duration: f64::NAN,
        paused: true,
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
        pub fn set_current_time(&self, t: f64) {
            SNAPSHOT.lock().expect("video snapshot").time = t;
            run(format!(
                "var v=document.getElementById('{VIDEO_ID}'); if(v) v.currentTime={t};"
            ));
        }
        pub fn play(&self) {
            SNAPSHOT.lock().expect("video snapshot").paused = false;
            run(format!("var v=document.getElementById('{VIDEO_ID}'); if(v) v.play();"));
        }
        pub fn pause(&self) {
            SNAPSHOT.lock().expect("video snapshot").paused = true;
            run(format!("var v=document.getElementById('{VIDEO_ID}'); if(v) v.pause();"));
        }
    }

    /// Reads the position and state of the `<video>` four times a second.
    pub fn use_bridge() {
        use_hook(|| {
            spawn(async move {
                let mut bridge = eval(&format!(
                    "setInterval(function () {{ \
                       var v = document.getElementById('{VIDEO_ID}'); \
                       if (v) dioxus.send([v.currentTime, isFinite(v.duration) ? v.duration : -1, v.paused]); \
                     }}, 250);"
                ));
                while let Ok((time, duration, paused)) = bridge.recv::<(f64, f64, bool)>().await {
                    *SNAPSHOT.lock().expect("video snapshot") = Snapshot {
                        time,
                        duration: if duration < 0.0 { f64::NAN } else { duration },
                        paused,
                    };
                }
            });
        });
    }
}

pub use imp::{use_bridge, video};
