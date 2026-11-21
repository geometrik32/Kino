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

/// Full screen on or off.
pub fn toggle_fullscreen() {
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        if doc.fullscreen_element().is_some() {
            doc.exit_fullscreen();
        } else if let Some(el) = doc.document_element() {
            let _ = el.request_fullscreen();
        }
    }
}

/// Opens a link outside the app.
pub fn open_external(url: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.open_with_url_and_target(url, "_blank");
    }
}
