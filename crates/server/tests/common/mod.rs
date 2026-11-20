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
        tmdb_language: "ru-RU".into(),
        static_dir: None,
        notify_every: std::time::Duration::from_secs(3600),
        preload_keep: chrono::Duration::days(7),
    };
    AppState {
        pool,
        config: Arc::new(config),
        catalog: Arc::new(DemoCatalog),
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
