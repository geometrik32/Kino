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
pub mod social;
pub mod sync;
pub mod telegram_auth;
pub mod torrent;
pub mod userdata;

use axum::{
    extract::Request,
    http::{header, HeaderName, HeaderValue},
    middleware::{self, Next},
    response::Response,
    Router,
};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

pub use api::AppState;

/// Extracts the hashed Dioxus web script asset filename (e.g. `kino-app-dxh....js`) from `index.html`.
fn extract_web_asset_tag(html: &str) -> Option<String> {
    let start = html.find("kino-app-dxh")?;
    let rest = &html[start..];
    let end = rest.find(".js")?;
    let tag = &rest[..end + 3];
    (tag.len() <= 96
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
    .then(|| tag.to_owned())
}

async fn response_headers(req: Request, next: Next, web_asset: Option<HeaderValue>) -> Response {
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

    if ok {
        if path.starts_with("/api/") {
            if let Some(tag) = web_asset {
                if !path.starts_with("/api/stream/")
                    && !path.starts_with("/api/img/")
                    && !path.starts_with("/api/avatar/")
                {
                    headers.insert(HeaderName::from_static("x-kino-web-asset"), tag);
                }
            }
        } else if !headers.contains_key(header::CACHE_CONTROL) {
            if path.starts_with("/assets/") {
                headers.insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                );
            } else {
                headers.insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("no-cache, no-store, must-revalidate"),
                );
            }
        }
    }
    resp
}

/// The whole HTTP app: `/api/*` plus, when configured, the web app at `/`.
pub fn app(state: AppState) -> Router {
    let mut router = Router::new().nest("/api", api::router());
    let mut web_asset: Option<HeaderValue> = None;
    if let Some(dir) = &state.config.static_dir {
        // Single-page app: unknown paths fall back to index.html.
        // Precompressed `.br`/`.gz` files (generated at Docker build time) are served automatically.
        let index = dir.join("index.html");
        if let Ok(html) = std::fs::read_to_string(&index) {
            web_asset = extract_web_asset_tag(&html).and_then(|tag| HeaderValue::from_str(&tag).ok());
        }
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
        .layer(middleware::from_fn(move |req, next| {
            response_headers(req, next, web_asset.clone())
        }))
        .layer(TraceLayer::new_for_http())
}

#[cfg(test)]
mod tests {
    use super::extract_web_asset_tag;

    #[test]
    fn extracts_hashed_dioxus_script_tag() {
        let html = r#"<script type="module" async src="/./assets/kino-app-dxheea1653c226931e5.js"></script>"#;
        assert_eq!(
            extract_web_asset_tag(html).as_deref(),
            Some("kino-app-dxheea1653c226931e5.js")
        );
        assert_eq!(extract_web_asset_tag("<script src=\"/app.js\"></script>"), None);
    }
}
