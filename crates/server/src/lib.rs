//! Kino server: Telegram login, watch progress and watchlist over a JSON API,
//! plus static hosting of the Dioxus web app.

pub mod api;
pub mod catalog;
pub mod config;
pub mod db;
pub mod error;
pub mod jobs;
pub mod play;
pub mod session;
pub mod telegram_auth;
pub mod torrent;
pub mod userdata;

use axum::Router;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

pub use api::AppState;
pub use kino_core::LOGIN_PREFIX;

/// The whole HTTP app: `/api/*` plus, when configured, the web app at `/`.
pub fn app(state: AppState) -> Router {
    let mut router = Router::new().nest("/api", api::router());
    if let Some(dir) = &state.config.static_dir {
        // Single-page app: unknown paths fall back to index.html.
        let index = dir.join("index.html");
        router = router.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)));
    }
    router.with_state(state).layer(TraceLayer::new_for_http())
}
