//! Everything that differs between the website (WebAssembly in a browser) and the
//! native apps (phone, desktop: the same Dioxus UI in a system WebView).
//!
//! The rest of the app calls only the functions below, so adding a platform means
//! adding one module here.

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub use web::*;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

#[cfg(target_os = "android")]
mod android;
pub mod video;

/// Absolute URL of a server path such as `/api/me`.
pub fn abs(path: &str) -> String {
    format!("{}{}", server_base(), path)
}

/// Where the server runs: the page's own origin in a browser, a configured address in apps.
/// Never ends with `/`.
pub fn server_base() -> String {
    SERVER_BASE.read().expect("server base lock").clone()
}

pub(crate) static SERVER_BASE: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// Sets the server address (used by apps that let the user change it).
pub fn set_server_base(base: &str) {
    let base = base.trim().trim_end_matches('/').to_owned();
    kino_core::set_asset_base(if is_web() { String::new() } else { base.clone() });
    *SERVER_BASE.write().expect("server base lock") = base;
}

/// Waits `ms` milliseconds without blocking the UI.
pub async fn sleep(ms: u32) {
    let _ = dioxus::document::eval(&format!("await new Promise(r => setTimeout(r, {ms})); return 0;"))
        .join::<i32>()
        .await;
}

/// Whether the main way to point is a finger (phone, tablet).
pub async fn touch_screen() -> bool {
    dioxus::document::eval("return window.matchMedia('(pointer: coarse)').matches;")
        .join::<bool>()
        .await
        .unwrap_or(false)
}

/// The player takes the screen: on a phone the picture turns to landscape and the
/// system bars go away; `false` gives everything back.
pub fn player_mode(on: bool) {
    #[cfg(target_os = "android")]
    android::player_mode(on);
    #[cfg(not(target_os = "android"))]
    {
        // A phone's browser (and Telegram) needs full screen before it lets the page rotate.
        let js = if on {
            "try { if (window.matchMedia('(pointer: coarse)').matches) { \
               var el = document.documentElement; \
               var go = function () { if (screen.orientation && screen.orientation.lock) { screen.orientation.lock('landscape').catch(function () {}); } }; \
               if (el.requestFullscreen && !document.fullscreenElement) { el.requestFullscreen().then(go, go); } else { go(); } \
               var tg = window.Telegram && window.Telegram.WebApp; \
               if (tg && tg.lockOrientation) { tg.lockOrientation(); } } } catch (e) {}"
        } else {
            "try { if (screen.orientation && screen.orientation.unlock) { screen.orientation.unlock(); } \
               var tg = window.Telegram && window.Telegram.WebApp; \
               if (tg && tg.unlockOrientation) { tg.unlockOrientation(); } \
               if (document.fullscreenElement && document.exitFullscreen) { document.exitFullscreen(); } } catch (e) {}"
        };
        let _ = dioxus::document::eval(js);
    }
}
