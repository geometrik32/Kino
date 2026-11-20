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
