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

/// Remembers a server address chosen by the user.
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

/// Current unix epoch milliseconds.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Unique per-player-mount identifier for sync rooms.
pub fn random_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let ts = chrono::Utc::now().timestamp_millis() as u64;
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("n-{ts:x}-{seq:x}")
}

/// Opens a link in the system browser (or the Telegram app).
pub fn open_external(url: &str) {
    if let Err(e) = open_url(url) {
        eprintln!("cannot open {url}: {e}");
    }
}

#[cfg(target_os = "android")]
fn open_url(url: &str) -> Result<(), String> {
    if !super::is_safe_external_url(url) {
        return Err("disallowed URL scheme".into());
    }
    super::android::open_url(url)
}

#[cfg(target_os = "windows")]
fn open_url(url: &str) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    if !super::is_safe_external_url(url) {
        return Err("disallowed URL scheme".into());
    }

    // Call ShellExecuteW directly so cmd.exe never parses shell metacharacters (&, |, ^, %).
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut std::ffi::c_void,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> isize;
    }

    let wide = |s: &str| -> Vec<u16> { OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect() };
    let op = wide("open");
    let file = wide(url);
    const SW_SHOWNORMAL: i32 = 1;
    // SAFETY: `op` and `file` are valid NUL-terminated UTF-16 buffers alive for the duration of the call.
    let rc = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if rc > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW failed with code {rc}"))
    }
}

#[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
fn open_url(url: &str) -> Result<(), String> {
    if !super::is_safe_external_url(url) {
        return Err("disallowed URL scheme".into());
    }
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg("--").arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}
