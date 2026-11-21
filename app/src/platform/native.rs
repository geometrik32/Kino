//! The phone and desktop apps: our own code in a native process, the UI in the system WebView.
//! The WebView's `localStorage` is used for small settings (it is kept between launches).

use dioxus::document::eval;

/// Where the apps look for the server when nothing was chosen in the app.
/// Override at build time with `KINO_SERVER_URL=https://... dx build ...`.
const DEFAULT_SERVER: &str = match option_env!("KINO_SERVER_URL") {
    Some(url) => url,
    None => "https://kino.aat-terra.ru",
};

const SERVER_KEY: &str = "kino.server";

pub fn is_web() -> bool {
    false
}

/// Called once before the first screen.
pub async fn init() {
    let saved = storage_get(SERVER_KEY).await.filter(|s| s.starts_with("http"));
    super::set_server_base(saved.as_deref().unwrap_or(DEFAULT_SERVER));
    #[cfg(target_os = "android")]
    super::android::style_bars(false);
}

/// Remembers a server address chosen by the user; takes effect after restart of the app.
#[allow(dead_code)]
pub fn save_server(base: &str) {
    storage_set(SERVER_KEY, base.trim().trim_end_matches('/'));
}

pub async fn storage_get(key: &str) -> Option<String> {
    let js = format!("return localStorage.getItem({});", serde_json::to_string(key).ok()?);
    eval(&js).join::<Option<String>>().await.ok().flatten()
}

pub fn storage_set(key: &str, value: &str) {
    if let (Ok(k), Ok(v)) = (serde_json::to_string(key), serde_json::to_string(value)) {
        let _ = eval(&format!("localStorage.setItem({k}, {v});"));
    }
}

pub fn storage_remove(key: &str) {
    if let Ok(k) = serde_json::to_string(key) {
        let _ = eval(&format!("localStorage.removeItem({k});"));
    }
}

/// "Приложение, Android" for the watch history.
pub fn device_name() -> String {
    let os = if cfg!(target_os = "android") {
        "Android"
    } else if cfg!(target_os = "ios") {
        "iPhone"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "Mac"
    } else {
        "Linux"
    };
    format!("Приложение, {os}")
}

/// Current time and the offset from UTC in minutes.
pub fn now() -> (chrono::DateTime<chrono::Utc>, i32) {
    let local = chrono::Local::now();
    (
        local.with_timezone(&chrono::Utc),
        chrono::Offset::fix(local.offset()).local_minus_utc() / 60,
    )
}

/// Full screen on or off.
pub fn toggle_fullscreen() {
    let _ = eval(
        "if (document.fullscreenElement) { document.exitFullscreen(); } \
         else { document.documentElement.requestFullscreen(); }",
    );
}

/// Opens a link in the system browser (or the Telegram app).
pub fn open_external(url: &str) {
    if let Err(e) = open_url(url) {
        eprintln!("cannot open {url}: {e}");
    }
}

#[cfg(target_os = "android")]
fn open_url(url: &str) -> Result<(), String> {
    super::android::open_url(url)
}

#[cfg(not(target_os = "android"))]
fn open_url(url: &str) -> Result<(), String> {
    // Windows and macOS have a shell opener; Linux uses xdg-open.
    let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", "", url])
    } else if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else {
        ("xdg-open", vec![url])
    };
    std::process::Command::new(cmd)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
