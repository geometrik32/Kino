//! Kino server: Telegram login, watch progress and watchlist over a JSON API,
//! plus static hosting of the Dioxus web app.

pub mod api;
pub mod avatar;
pub mod catalog;
pub mod config;
pub mod db;
pub mod error;
pub mod jobs;
pub mod play;
pub mod session;
pub mod sync;
pub mod telegram_auth;
pub mod torrent;
pub mod userdata;

use axum::{
    extract::Request,
    http::{header, HeaderValue},
    middleware::{self, Next},
    response::Response,
    Router,
};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

pub use api::AppState;

async fn response_headers(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let mut resp = next.run(req).await;
    let ok = resp.status().is_success();
    let headers = resp.headers_mut();
    headers
        .entry(header::X_CONTENT_TYPE_OPTIONS)
        .or_insert_with(|| HeaderValue::from_static("nosniff"));
    headers
        .entry(header::REFERRER_POLICY)
        .or_insert_with(|| HeaderValue::from_static("strict-origin-when-cross-origin"));
    headers
        .entry(header::STRICT_TRANSPORT_SECURITY)
        .or_insert_with(|| HeaderValue::from_static("max-age=31536000; includeSubDomains"));
    headers
        .entry(header::HeaderName::from_static("permissions-policy"))
        .or_insert_with(|| HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"));
    headers.entry(header::CONTENT_SECURITY_POLICY).or_insert_with(|| {
        HeaderValue::from_static("frame-ancestors 'self' https://web.telegram.org https://*.telegram.org")
    });

    if ok && !headers.contains_key(header::CACHE_CONTROL) {
        if path.starts_with("/assets/") {
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            );
        } else if !path.starts_with("/api/") {
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        }
    }
    resp
}

/// The whole HTTP app: `/api/*` plus, when configured, the web app at `/`.
pub fn app(state: AppState) -> Router {
    let mut router = Router::new().nest("/api", api::router());
    if let Some(dir) = &state.config.static_dir {
        // Single-page app: unknown paths fall back to index.html.
        // Precompressed `.br`/`.gz` files (generated at Docker build time) are served automatically.
        let index = dir.join("index.html");
        let fallback = ServeFile::new(index).precompressed_br().precompressed_gzip();
        router = router.fallback_service(
            ServeDir::new(dir)
                .precompressed_br()
                .precompressed_gzip()
                .fallback(fallback),
        );
    }
    router
        .with_state(state)
        .layer(middleware::from_fn(response_headers))
        .layer(TraceLayer::new_for_http())
}
