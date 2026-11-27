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
pub(crate) mod android;
pub mod video;

/// Allows only `https://` or `tg://` external links without whitespace, control
/// characters or shell/markup metacharacters.
pub fn is_safe_external_url(url: &str) -> bool {
    if url.len() > 2048 {
        return false;
    }
    let rest = if let Some(r) = url.strip_prefix("https://") {
        r
    } else if let Some(r) = url.strip_prefix("tg://") {
        r
    } else {
        return false;
    };
    if rest.is_empty() || rest.starts_with(['/', '@', '-', '.']) {
        return false;
    }
    if !url
        .bytes()
        .all(|b| (0x21..=0x7e).contains(&b) && !matches!(b, b'"' | b'\'' | b'`' | b'\\' | b'<' | b'>'))
    {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty()
        && !authority.contains('@')
        && authority
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':' | b'[' | b']'))
}

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
/// Uses native Tokio timers on Desktop/Android (avoiding WebView IPC and background throttling)
/// and `setTimeout` on WebAssembly.
#[cfg(not(target_arch = "wasm32"))]
pub async fn sleep(ms: u32) {
    tokio::time::sleep(std::time::Duration::from_millis(u64::from(ms))).await;
}

/// Waits `ms` milliseconds without blocking the UI.
#[cfg(target_arch = "wasm32")]
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

/// Toggles Telegram's native fullscreen inside Mini App (avoiding Android WebView DOM
/// fullscreen that covers Telegram's header/BackButton) and DOM fullscreen in regular browsers
/// (including iOS Safari's `webkitEnterFullscreen` fallback on `<video>`).
pub fn toggle_fullscreen() {
    #[cfg(not(target_os = "android"))]
    {
        let js = "try { \
            var root = document.documentElement; \
            var vid = document.getElementById('kino-video'); \
            var tg = window.Telegram && window.Telegram.WebApp; \
            var inTg = root.classList.contains('tg') || !!(tg && tg.initData); \
            if (inTg) { \
              if (document.fullscreenElement && document.exitFullscreen) { \
                document.exitFullscreen().catch(function(){}); \
              } \
              if (tg && tg.isVersionAtLeast && tg.isVersionAtLeast('8.0')) { \
                if (tg.isFullscreen && tg.exitFullscreen) tg.exitFullscreen(); \
                else if (tg.requestFullscreen) tg.requestFullscreen(); \
              } \
            } else if (document.fullscreenElement || document.webkitFullscreenElement) { \
              window.__kinoFsUserExit = true; \
              if (document.exitFullscreen) document.exitFullscreen().catch(function(){}); \
              else if (document.webkitExitFullscreen) document.webkitExitFullscreen(); \
            } else if (vid && vid.webkitDisplayingFullscreen && vid.webkitExitFullscreen) { \
              window.__kinoFsUserExit = true; \
              vid.webkitExitFullscreen(); \
            } else if (root.requestFullscreen && document.fullscreenEnabled !== false) { \
              window.__kinoFsUserExit = false; \
              var go = function () { if (window.matchMedia('(pointer: coarse)').matches && screen.orientation && screen.orientation.lock) { screen.orientation.lock('landscape').catch(function () {}); } }; \
              try { root.requestFullscreen({ navigationUI: 'hide' }).then(go, function () { try { root.requestFullscreen().then(go, go); } catch (e) { go(); } }); } \
              catch (e) { try { root.requestFullscreen().then(go, go); } catch (e2) { go(); } } \
            } else if (root.webkitRequestFullscreen) { \
              window.__kinoFsUserExit = false; \
              root.webkitRequestFullscreen(); \
              if (window.matchMedia('(pointer: coarse)').matches && screen.orientation && screen.orientation.lock) { screen.orientation.lock('landscape').catch(function () {}); } \
            } else if (vid && vid.webkitEnterFullscreen) { \
              window.__kinoFsUserExit = false; \
              vid.webkitEnterFullscreen(); \
            } \
          } catch (e) {}";
        let _ = dioxus::document::eval(js);
    }
}

/// The player takes the screen: hides the underlying page scrollbar everywhere,
/// and on a phone hides system bars / turns to landscape (using Telegram's own
/// fullscreen + CSS rotation fallback inside Telegram Mini App without triggering
/// Android WebView DOM fullscreen); `false` restores everything.
pub fn player_mode(on: bool) {
    let root_js = if on {
        "document.documentElement.classList.add('kino-playing'); window.__kinoFsUserExit = false;"
    } else {
        "document.documentElement.classList.remove('kino-playing'); document.documentElement.classList.remove('tg-player-rot'); window.__kinoFsUserExit = false;"
    };
    let _ = dioxus::document::eval(root_js);

    if on {
        crate::telegram::paint_player_bars();
    } else {
        crate::telegram::paint_bars();
    }

    #[cfg(target_os = "android")]
    android::player_mode(on);
    #[cfg(not(target_os = "android"))]
    {
        // Never call `tg.lockOrientation()` in portrait: in Bot API 8.0+ it locks
        // the *current* orientation (portrait). Always keep it unlocked in the player
        // and use `tg-player-rot` CSS landscape fallback when Telegram stays portrait.
        // Inside Telegram Mini App, never call DOM `root.requestFullscreen()` or hook
        // tap retries: Android WebView's CustomView fullscreen hides Telegram's BackButton,
        // intercepts system Back, and swallows taps on the player UI.
        let js = if on {
            "try { \
               if (window.__kinoFsRetry) { \
                 ['pointerup', 'touchend', 'click'].forEach(function (t) { document.removeEventListener(t, window.__kinoFsRetry, true); }); \
                 window.__kinoFsRetry = null; \
               } \
               if (window.matchMedia('(pointer: coarse)').matches) { \
                 var root = document.documentElement; \
                 var tg = window.Telegram && window.Telegram.WebApp; \
                 var inTg = root.classList.contains('tg') || !!(tg && tg.initData); \
                 var lock = function () { if (screen.orientation && screen.orientation.lock) { screen.orientation.lock('landscape').catch(function () {}); } }; \
                 var isFs = function () { return !!(document.fullscreenElement || document.webkitFullscreenElement); }; \
                 if (inTg) { \
                   if (tg && tg.unlockOrientation) { tg.unlockOrientation(); } \
                   root.classList.add('tg-player-rot'); \
                   if (document.fullscreenElement && document.exitFullscreen) { document.exitFullscreen().catch(function () {}); } \
                   else if (document.webkitFullscreenElement && document.webkitExitFullscreen) { document.webkitExitFullscreen(); } \
                   lock(); \
                 } else { \
                   var fsBusy = false; \
                   var enterFs = function () { \
                     if (fsBusy) return; \
                     if (isFs()) { lock(); return; } \
                     if (root.requestFullscreen && document.fullscreenEnabled !== false) { \
                       fsBusy = true; \
                       var done = function () { fsBusy = false; lock(); }; \
                       try { \
                         root.requestFullscreen({ navigationUI: 'hide' }).then(done, function () { \
                           try { root.requestFullscreen().then(done, done); } catch (e) { done(); } \
                         }); \
                       } catch (e) { \
                         try { root.requestFullscreen().then(done, done); } catch (e2) { done(); } \
                       } \
                     } else if (root.webkitRequestFullscreen) { \
                       try { root.webkitRequestFullscreen(); } catch (e) {} \
                       lock(); \
                     } else { lock(); } \
                   }; \
                   enterFs(); \
                   window.__kinoFsRetry = function (ev) { \
                     if (!document.documentElement.classList.contains('kino-playing')) return; \
                     if (window.__kinoFsUserExit) return; \
                     if (ev && ev.target && ev.target.closest && ev.target.closest('.p-fs-btn, button[aria-label=\"Назад\"]')) return; \
                     if (!isFs()) { enterFs(); } \
                     else { lock(); } \
                   }; \
                   ['pointerup', 'touchend', 'click'].forEach(function (t) { document.addEventListener(t, window.__kinoFsRetry, true); }); \
                 } \
               } \
             } catch (e) {}"
        } else {
            "try { \
               if (window.__kinoFsRetry) { \
                 ['pointerup', 'touchend', 'click'].forEach(function (t) { document.removeEventListener(t, window.__kinoFsRetry, true); }); \
                 window.__kinoFsRetry = null; \
               } \
               if (screen.orientation && screen.orientation.unlock) { screen.orientation.unlock(); } \
               var tg = window.Telegram && window.Telegram.WebApp; \
               if (tg && tg.unlockOrientation) { tg.unlockOrientation(); } \
               if (document.fullscreenElement && document.exitFullscreen) { document.exitFullscreen().catch(function () {}); } \
               else if (document.webkitFullscreenElement && document.webkitExitFullscreen) { document.webkitExitFullscreen(); } \
             } catch (e) {}"
        };
        let _ = dioxus::document::eval(js);
    }
}

#[cfg(test)]
mod tests {
    use super::is_safe_external_url;

    #[test]
    fn external_url_validation_allows_https_and_tg_only() {
        assert!(is_safe_external_url(
            "https://t.me/Orlovski_Kinopokaz_bot?start=login_abc123"
        ));
        assert!(is_safe_external_url(
            "tg://resolve?domain=Orlovski_Kinopokaz_bot&start=login_abc123"
        ));
        assert!(!is_safe_external_url("http://t.me/bot"));
        assert!(!is_safe_external_url("javascript:alert(1)"));
        assert!(!is_safe_external_url("file:///etc/passwd"));
        assert!(!is_safe_external_url("intent://scan/#Intent;end"));
        assert!(!is_safe_external_url("https://t.me/bot?a=1 \"calc.exe\""));
        assert!(!is_safe_external_url("https://t.me/bot\ncalc.exe"));
        assert!(!is_safe_external_url("https://user@evil.example/"));
        assert!(!is_safe_external_url("https:///etc/passwd"));
    }
}
