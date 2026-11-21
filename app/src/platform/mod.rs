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

/// WebSocket URL (`ws://` or `wss://`) of a server path such as `/api/sync/ws?ticket=...`.
pub fn ws_url(path_and_query: &str) -> String {
    let full = abs(path_and_query);
    if let Some(rest) = full.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = full.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        full
    }
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

/// Applies the chosen colour theme to `<html data-theme>` and system/Telegram bars.
pub fn apply_theme(theme: kino_ui::Theme) {
    let js = match theme {
        kino_ui::Theme::Dark => "document.documentElement.dataset.theme = 'dark';",
        kino_ui::Theme::Light => "document.documentElement.dataset.theme = 'light';",
        kino_ui::Theme::Auto => "delete document.documentElement.dataset.theme;",
    };
    let _ = dioxus::document::eval(js);
    crate::telegram::paint_bars();
    #[cfg(target_os = "android")]
    android::style_bars(theme == kino_ui::Theme::Light);
}

/// The player takes the screen: hides the underlying page scrollbar everywhere,
/// and on a phone turns to landscape (with CSS rotation fallback inside Telegram
/// Mini App when the container refuses OS orientation lock); `false` restores everything.
pub fn player_mode(on: bool) {
    let root_js = if on {
        "document.documentElement.classList.add('kino-playing');"
    } else {
        "document.documentElement.classList.remove('kino-playing'); document.documentElement.classList.remove('tg-player-rot');"
    };
    let _ = dioxus::document::eval(root_js);

    #[cfg(target_os = "android")]
    android::player_mode(on);
    #[cfg(not(target_os = "android"))]
    {
        // Never call `tg.lockOrientation()` in portrait: in Bot API 8.0+ it locks
        // the *current* orientation (portrait). Always keep it unlocked in the player
        // and use `tg-player-rot` CSS landscape fallback when Telegram stays portrait.
        let js = if on {
            "try { if (window.matchMedia('(pointer: coarse)').matches) { \
               var root = document.documentElement; \
               var tg = window.Telegram && window.Telegram.WebApp; \
               var inTg = !!(tg && tg.initData); \
               var go = function () { if (screen.orientation && screen.orientation.lock) { screen.orientation.lock('landscape').catch(function () {}); } }; \
               if (inTg) { \
                 if (tg.unlockOrientation) { tg.unlockOrientation(); } \
                 root.classList.add('tg-player-rot'); \
                 go(); \
               } else { \
                 var el = root; \
                 if (el.requestFullscreen && !document.fullscreenElement) { el.requestFullscreen().then(go, go); } else { go(); } \
               } } } catch (e) {}"
        } else {
            "try { if (screen.orientation && screen.orientation.unlock) { screen.orientation.unlock(); } \
               var tg = window.Telegram && window.Telegram.WebApp; \
               if (tg && tg.unlockOrientation) { tg.unlockOrientation(); } \
               if (document.fullscreenElement && document.exitFullscreen) { document.exitFullscreen(); } } catch (e) {}"
        };
        let _ = dioxus::document::eval(js);
    }
}
