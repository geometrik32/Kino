//! End-to-end API tests against a real PostgreSQL.
//! Needs `DATABASE_URL` (each test gets a fresh database from `sqlx::test`).

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use kino_core::{AuthResponse, ContinueItem, Me, Progress, ReleaseChoice, Role, WatchlistItem};
use kino_server::{app, catalog::DemoCatalog, config::Config, telegram_auth, AppState};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

const BOT_TOKEN: &str = "123456:TEST-token";
const ADMIN_TG: i64 = 1000;

fn test_app(pool: PgPool) -> Router {
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
    };
    app(AppState {
        pool,
        config: Arc::new(config),
        catalog: Arc::new(DemoCatalog),
    })
}

async fn call(
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

fn init_data(tg_id: i64, name: &str) -> String {
    let user = json!({ "id": tg_id, "first_name": name }).to_string();
    let now = chrono::Utc::now().timestamp().to_string();
    telegram_auth::sign_webapp(&[("auth_date", &now), ("user", &user)], BOT_TOKEN)
}

async fn login(app: &Router, tg_id: i64, invite: Option<&str>) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/api/auth/webapp",
        None,
        Some(json!({ "init_data": init_data(tg_id, "Тест"), "invite": invite })),
    )
    .await
}

async fn login_ok(app: &Router, tg_id: i64, invite: Option<&str>) -> AuthResponse {
    let (status, body) = login(app, tg_id, invite).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_value(body).unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn invite_only_access(pool: PgPool) {
    let app = test_app(pool);

    let (status, body) = login(&app, 5, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "not_invited");

    let admin = login_ok(&app, ADMIN_TG, None).await;
    assert_eq!(admin.me.role, Role::Admin);

    // A normal user can't create invites; the admin can.
    let (status, body) = call(&app, Method::POST, "/api/invites", Some(&admin.token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let code = body["code"].as_str().unwrap().to_owned();
    assert_eq!(body["link"], format!("https://kino.example.com/?invite={code}"));

    let user = login_ok(&app, 5, Some(&code)).await;
    assert_eq!(user.me.role, Role::User);
    let (status, _) = call(&app, Method::POST, "/api/invites", Some(&user.token), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The invite is single-use, but the invited user can log in again without it.
    let (status, _) = login(&app, 6, Some(&code)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    login_ok(&app, 5, None).await;

    let (status, body) = call(&app, Method::GET, "/api/me", Some(&user.token), None).await;
    assert_eq!(status, StatusCode::OK);
    let me: Me = serde_json::from_value(body).unwrap();
    assert_eq!(me.telegram_id, 5);
}

#[sqlx::test(migrations = "./migrations")]
async fn rejects_bad_auth(pool: PgPool) {
    let app = test_app(pool);
    let (status, _) = call(&app, Method::GET, "/api/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&app, Method::GET, "/api/me", Some("forged.token"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let tampered = init_data(ADMIN_TG, "Тест").replace("1000", "1001");
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/auth/webapp",
        None,
        Some(json!({ "init_data": tampered })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn login_widget(pool: PgPool) {
    let app = test_app(pool);
    let mut fields = BTreeMap::from([
        ("id".to_owned(), ADMIN_TG.to_string()),
        ("first_name".to_owned(), "Даня".to_owned()),
        ("auth_date".to_owned(), chrono::Utc::now().timestamp().to_string()),
    ]);
    telegram_auth::sign_widget(&mut fields, BOT_TOKEN);
    let (status, body) = call(
        &app,
        Method::POST,
        "/api/auth/widget",
        None,
        Some(json!({ "fields": fields })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["me"]["first_name"], "Даня");

    let (status, body) = call(&app, Method::GET, "/api/public-config", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["bot_username"], "kino_test_bot");
}

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

#[sqlx::test(migrations = "./migrations")]
async fn progress_resumes_same_release_on_any_device(pool: PgPool) {
    let app = test_app(pool);
    let phone = login_ok(&app, ADMIN_TG, None).await;

    // Phone: start a film from a chosen release.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/api/progress",
        Some(&phone.token),
        Some(json!({
            "kind": "movie", "tmdb_id": 603, "position_sec": 120.0, "duration_sec": 8160.0,
            "release": { "infohash": HASH.to_uppercase(), "file_index": 1, "title": "The Matrix 1080p" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Later updates don't resend the release; it must be kept.
    let (status, _) = call(
        &app,
        Method::PUT,
        "/api/progress",
        Some(&phone.token),
        Some(json!({
            "kind": "movie", "tmdb_id": 603, "position_sec": 2472.5, "duration_sec": 8160.0, "release": null
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Computer: a fresh login sees the same spot and the same release.
    let computer = login_ok(&app, ADMIN_TG, None).await;
    let (_, body) = call(&app, Method::GET, "/api/continue", Some(&computer.token), None).await;
    let list: Vec<ContinueItem> = serde_json::from_value(body).unwrap();
    assert_eq!(list.len(), 1);
    let last = &list[0].last;
    assert_eq!(last.position_sec, 2472.5);
    let release = last.release.as_ref().unwrap();
    assert_eq!(release.infohash, HASH);
    assert_eq!(release.file_index, 1);
    assert_eq!(release.title.as_deref(), Some("The Matrix 1080p"));

    // Watching to the end removes the film from "Continue watching".
    call(
        &app,
        Method::PUT,
        "/api/progress",
        Some(&computer.token),
        Some(json!({
            "kind": "movie", "tmdb_id": 603, "position_sec": 7800.0, "duration_sec": 8160.0
        })),
    )
    .await;
    let (_, body) = call(&app, Method::GET, "/api/continue", Some(&computer.token), None).await;
    assert_eq!(body, json!([]));

    // Seeking back after finishing keeps it marked as watched.
    let (_, body) = call(
        &app,
        Method::PUT,
        "/api/progress",
        Some(&computer.token),
        Some(json!({
            "kind": "movie", "tmdb_id": 603, "position_sec": 10.0, "duration_sec": 8160.0
        })),
    )
    .await;
    assert_eq!(body["finished"], true);
}

#[sqlx::test(migrations = "./migrations")]
async fn series_points_to_next_episode(pool: PgPool) {
    let app = test_app(pool);
    let u = login_ok(&app, ADMIN_TG, None).await;
    for (ep, pos) in [(5, 3000.0), (6, 600.0)] {
        let (status, body) = call(
            &app,
            Method::PUT,
            "/api/progress",
            Some(&u.token),
            Some(json!({
                "kind": "tv", "tmdb_id": 1399, "season": 2, "episode": ep, "position_sec": pos, "duration_sec": 3200.0,
                "release": { "infohash": HASH, "file_index": ep - 1 }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (_, body) = call(&app, Method::GET, "/api/continue", Some(&u.token), None).await;
    let list: Vec<ContinueItem> = serde_json::from_value(body).unwrap();
    assert_eq!(list.len(), 1, "one card per series");
    assert_eq!((list[0].last.season, list[0].last.episode), (2, 6));
    assert_eq!(list[0].next_episode, None);

    // Finish episode 6: the card now points at episode 7.
    call(
        &app,
        Method::PUT,
        "/api/progress",
        Some(&u.token),
        Some(json!({
            "kind": "tv", "tmdb_id": 1399, "season": 2, "episode": 6, "position_sec": 3100.0, "duration_sec": 3200.0
        })),
    )
    .await;
    let (_, body) = call(&app, Method::GET, "/api/continue", Some(&u.token), None).await;
    let list: Vec<ContinueItem> = serde_json::from_value(body).unwrap();
    assert_eq!(list[0].next_episode, Some((2, 7)));

    // Per-episode marks for the series page.
    let (_, body) = call(&app, Method::GET, "/api/progress/tv/1399", Some(&u.token), None).await;
    let eps: Vec<Progress> = serde_json::from_value(body).unwrap();
    assert_eq!(
        eps.iter().map(|p| (p.episode, p.finished)).collect::<Vec<_>>(),
        vec![(5, true), (6, true)]
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn progress_is_private_and_validated(pool: PgPool) {
    let app = test_app(pool);
    let admin = login_ok(&app, ADMIN_TG, None).await;
    let (_, inv) = call(&app, Method::POST, "/api/invites", Some(&admin.token), None).await;
    let other = login_ok(&app, 7, inv["code"].as_str()).await;

    call(
        &app,
        Method::PUT,
        "/api/progress",
        Some(&admin.token),
        Some(json!({
            "kind": "movie", "tmdb_id": 603, "position_sec": 100.0, "duration_sec": 8000.0
        })),
    )
    .await;
    let (_, body) = call(&app, Method::GET, "/api/continue", Some(&other.token), None).await;
    assert_eq!(body, json!([]), "users don't see each other's progress");

    for bad in [
        json!({ "kind": "movie", "tmdb_id": 603, "position_sec": -1.0, "duration_sec": 10.0 }),
        json!({ "kind": "tv", "tmdb_id": 1399, "season": 1, "episode": 0, "position_sec": 1.0, "duration_sec": 10.0 }),
        json!({ "kind": "movie", "tmdb_id": 603, "position_sec": 1.0, "duration_sec": 10.0,
                "release": { "infohash": "not-a-hash", "file_index": 0 } }),
    ] {
        let (status, _) = call(
            &app,
            Method::PUT,
            "/api/progress",
            Some(&admin.token),
            Some(bad.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn watchlist_add_list_remove(pool: PgPool) {
    let app = test_app(pool);
    let u = login_ok(&app, ADMIN_TG, None).await;
    for path in [
        "/api/watchlist/movie/603",
        "/api/watchlist/tv/1399",
        "/api/watchlist/movie/603",
    ] {
        let (status, _) = call(&app, Method::PUT, path, Some(&u.token), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (_, body) = call(&app, Method::GET, "/api/watchlist", Some(&u.token), None).await;
    let list: Vec<WatchlistItem> = serde_json::from_value(body).unwrap();
    assert_eq!(list.len(), 2, "adding twice is a no-op");

    let (status, _) = call(&app, Method::DELETE, "/api/watchlist/movie/603", Some(&u.token), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, Method::DELETE, "/api/watchlist/movie/603", Some(&u.token), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, Method::PUT, "/api/watchlist/anime/1", Some(&u.token), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "./migrations")]
async fn catalog_browse_and_continue_has_titles(pool: PgPool) {
    let app = test_app(pool);
    let (status, _) = call(&app, Method::GET, "/api/catalog/home", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "catalog needs login");

    let u = login_ok(&app, ADMIN_TG, None).await;
    let (status, home) = call(&app, Method::GET, "/api/catalog/home", Some(&u.token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(home["demo"], true);
    assert!(home["rows"].as_array().unwrap().len() >= 2);

    let (_, found) = call(
        &app,
        Method::GET,
        "/api/catalog/search?q=%D0%BC%D0%B0%D1%82%D1%80",
        Some(&u.token),
        None,
    )
    .await;
    assert_eq!(found["items"][0]["tmdb_id"], 603, "search finds «Матрица» by «матр»");

    let (status, tv) = call(&app, Method::GET, "/api/catalog/tv/1396", Some(&u.token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tv["seasons"].as_array().unwrap().len(), 5);
    let (_, season) = call(&app, Method::GET, "/api/catalog/tv/1396/season/2", Some(&u.token), None).await;
    assert_eq!(season["episodes"].as_array().unwrap().len(), 13);
    let (status, _) = call(&app, Method::GET, "/api/catalog/tv/1396/season/9", Some(&u.token), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, Method::GET, "/api/catalog/movie/999999", Some(&u.token), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    call(
        &app,
        Method::PUT,
        "/api/progress",
        Some(&u.token),
        Some(json!({
            "kind": "tv", "tmdb_id": 1396, "season": 2, "episode": 6, "position_sec": 600.0, "duration_sec": 2800.0
        })),
    )
    .await;
    let (_, cont) = call(&app, Method::GET, "/api/continue", Some(&u.token), None).await;
    assert_eq!(cont[0]["title"]["title"], "Во все тяжкие");
}

#[sqlx::test(migrations = "./migrations")]
async fn release_choice_is_remembered_per_season(pool: PgPool) {
    let app = test_app(pool);
    let u = login_ok(&app, ADMIN_TG, None).await;
    let pick = |season: i32, hash: char, voice: &str| {
        json!({"kind": "tv", "tmdb_id": 1396, "season": season, "infohash": hash.to_string().repeat(40).to_uppercase(),
               "title": format!("S{season:02} pack"), "quality": "1080p", "voice": voice})
    };
    for body in [
        pick(1, 'a', "LostFilm"),
        pick(2, 'b', "LostFilm"),
        pick(1, 'c', "NewStudio"),
    ] {
        let (status, _) = call(&app, Method::PUT, "/api/release-choice", Some(&u.token), Some(body)).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, body) = call(&app, Method::GET, "/api/release-choice/tv/1396", Some(&u.token), None).await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<ReleaseChoice> = serde_json::from_value(body).unwrap();
    assert_eq!(list.len(), 2, "one choice per season");
    assert_eq!(list[0].release.season, 1, "newest first");
    assert_eq!(list[0].release.infohash, "c".repeat(40), "replaced and lowercased");
    assert_eq!(list[0].release.voice.as_deref(), Some("NewStudio"));

    let bad = json!({"kind": "movie", "tmdb_id": 603, "season": 2, "infohash": "a".repeat(40)});
    let (status, _) = call(&app, Method::PUT, "/api/release-choice", Some(&u.token), Some(bad)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let bad = json!({"kind": "movie", "tmdb_id": 603, "infohash": "xyz"});
    let (status, _) = call(&app, Method::PUT, "/api/release-choice", Some(&u.token), Some(bad)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, inv) = call(&app, Method::POST, "/api/invites", Some(&u.token), None).await;
    let other = login_ok(&app, 7, inv["code"].as_str()).await;
    let (_, body) = call(
        &app,
        Method::GET,
        "/api/release-choice/tv/1396",
        Some(&other.token),
        None,
    )
    .await;
    assert_eq!(body, json!([]), "choices are per user");
}
