//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use kino_core::AuthResponse;
use kino_server::{app, catalog::DemoCatalog, config::Config, telegram_auth, AppState};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

pub const BOT_TOKEN: &str = "123456:TEST-token";
pub const ADMIN_TG: i64 = 1000;

pub fn test_state(pool: PgPool) -> AppState {
    test_state_with(pool, Default::default())
}

pub fn test_state_with(pool: PgPool, torrents: kino_server::config::TorrentConfig) -> AppState {
    let config = Config {
        database_url: String::new(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bot_token: BOT_TOKEN.into(),
        session_secret: b"test-secret-test-secret-test-secret".to_vec(),
        session_ttl: chrono::Duration::days(1),
        auth_max_age: chrono::Duration::days(1),
        admin_telegram_ids: vec![ADMIN_TG],
        bot_username: Some("kino_test_bot".into()),
        public_url: "https://kino.example.com".into(),
        tmdb_api_key: None,
        telegram_proxy: None,
        telegram_api_url: None,
        tmdb_proxy: None,
        tmdb_language: "ru-RU".into(),
        static_dir: None,
        image_cache_dir: None,
        image_cache_max_mb: 400,
        notify_every: std::time::Duration::from_secs(3600),
        preload_keep: chrono::Duration::days(7),
        torrents,
    };
    AppState {
        pool,
        media: Arc::new(kino_server::play::Media::new(&config.torrents)),
        sync: Arc::new(kino_server::sync::SyncHub::new()),
        config: Arc::new(config),
        catalog: Arc::new(DemoCatalog),
        telegram: reqwest::Client::new(),
    }
}

pub fn test_app(pool: PgPool) -> Router {
    app(test_state(pool))
}

pub async fn call(
    app: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req
            .header("Content-Type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

pub fn init_data(tg_id: i64, name: &str) -> String {
    let user = json!({ "id": tg_id, "first_name": name }).to_string();
    let now = chrono::Utc::now().timestamp().to_string();
    telegram_auth::sign_webapp(&[("auth_date", &now), ("user", &user)], BOT_TOKEN)
}

pub async fn login(app: &Router, tg_id: i64, invite: Option<&str>) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/api/auth/webapp",
        None,
        Some(json!({ "init_data": init_data(tg_id, "Тест"), "invite": invite })),
    )
    .await
}

pub async fn login_ok(app: &Router, tg_id: i64, invite: Option<&str>) -> AuthResponse {
    let (status, body) = login(app, tg_id, invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_value(body).unwrap()
}

/// A request with extra headers; returns status, headers and the raw body.
pub async fn raw(
    app: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes().to_vec();
    (parts.status, parts.headers, bytes)
}

/// Serves `router` on a free local port; returns its base URL.
pub async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}
