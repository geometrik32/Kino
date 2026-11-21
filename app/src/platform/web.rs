//! The website: runs in a browser, talks to the server that served it.

pub fn is_web() -> bool {
    true
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// Called once before the first screen.
pub async fn init() {
    let origin = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default();
    super::set_server_base(&origin);
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
    if let Some(w) = web_sys::window() {
        let _ = w.open_with_url_and_target(url, "_blank");
    }
}
