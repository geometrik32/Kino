//! The website: runs in a browser, talks to the server that served it.

pub fn is_web() -> bool {
    true
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// Loads Telegram's Mini App SDK on demand only when launched inside Telegram,
/// so regular browsers never block on `telegram.org`.
async fn ensure_telegram_sdk() {
    let js = r#"
        if (window.Telegram && window.Telegram.WebApp && window.Telegram.WebApp.initData) {
            return true;
        }
        var h = window.location.hash || '';
        var s = window.location.search || '';
        var cached = false;
        try { cached = !!window.sessionStorage.getItem('__telegram__initParams'); } catch (e) {}
        var inTg = cached
            || h.indexOf('tgWebAppData=') !== -1
            || h.indexOf('tgWebAppPlatform=') !== -1
            || s.indexOf('tgWebAppData=') !== -1
            || !!window.TelegramWebviewProxy
            || !!window.TelegramGameProxy;
        if (!inTg) return false;
        return await new Promise(function(resolve) {
            var done = false;
            var finish = function(ok) { if (!done) { done = true; resolve(ok); } };
            var existing = document.querySelector('script[data-kino-tg-sdk]');
            if (existing) {
                if (window.Telegram && window.Telegram.WebApp) { finish(true); return; }
                existing.addEventListener('load', function() { finish(true); }, { once: true });
                existing.addEventListener('error', function() { finish(false); }, { once: true });
                setTimeout(function() { finish(false); }, 5000);
                return;
            }
            var el = document.createElement('script');
            el.src = 'https://telegram.org/js/telegram-web-app.js';
            el.async = true;
            el.setAttribute('data-kino-tg-sdk', '1');
            el.onload = function() { finish(true); };
            el.onerror = function() { finish(false); };
            setTimeout(function() { finish(false); }, 5000);
            document.head.appendChild(el);
        });
    "#;
    let _ = dioxus::document::eval(js).join::<bool>().await;
}

/// Called once before the first screen.
pub async fn init() {
    let origin = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default();
    super::set_server_base(&origin);
    ensure_telegram_sdk().await;
}

/// Web always talks to its own origin.
pub fn save_server(_base: &str) {}

pub async fn storage_get(key: &str) -> Option<String> {
    storage()?.get_item(key).ok()?
}

pub fn storage_set(key: &str, value: &str) {
    if let Some(s) = storage() {
        let _ = s.set_item(key, value);
    }
}

pub fn storage_remove(key: &str) {
    if let Some(s) = storage() {
        let _ = s.remove_item(key);
    }
}

/// "Браузер, Android", "Telegram", ... for the watch history.
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

/// Current time and the offset from UTC in minutes.
pub fn now() -> (chrono::DateTime<chrono::Utc>, i32) {
    let d = js_sys::Date::new_0();
    let now = chrono::DateTime::from_timestamp_millis(d.get_time() as i64).unwrap_or_default();
    (now, -(d.get_timezone_offset() as i32))
}

/// Current unix epoch milliseconds.
pub fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

/// Unique per-player-mount identifier for sync rooms.
pub fn random_id() -> String {
    let a = (js_sys::Math::random() * 1e16) as u64;
    let b = (js_sys::Math::random() * 1e16) as u64;
    let ts = js_sys::Date::now() as u64;
    format!("w-{ts:x}-{a:x}-{b:x}")
}

/// Opens a link outside the app.
pub fn open_external(url: &str) {
    if !super::is_safe_external_url(url) {
        return;
    }
    if let Some(w) = web_sys::window() {
        let _ = w.open_with_url_and_target_and_features(url, "_blank", "noopener,noreferrer");
    }
}
