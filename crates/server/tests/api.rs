//! End-to-end API tests against a real PostgreSQL.
//! Needs `DATABASE_URL` (each test gets a fresh database from `sqlx::test`).

#[path = "common/mod.rs"]
mod common;

use std::collections::BTreeMap;

use axum::http::{Method, StatusCode};
use common::*;
use kino_core::{ContinueItem, Me, Progress, ReleaseChoice, Role, WatchlistItem};
use kino_server::telegram_auth;
use serde_json::json;
use sqlx::PgPool;

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

    // Cast and suggestions come separately and never include the title itself.
    let (status, extras) = call(&app, Method::GET, "/api/catalog/tv/1396/extras", Some(&u.token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!extras["cast"].as_array().unwrap().is_empty());
    for row in ["recommendations", "similar"] {
        let ids: Vec<_> = extras[row]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["tmdb_id"].clone())
            .collect();
        assert!(!ids.contains(&json!(1396)), "{row}: {ids:?}");
    }
    // An actor from the cast opens with a filmography.
    let actor = extras["cast"][0]["id"].as_i64().unwrap();
    let (status, person) = call(&app, Method::GET, &format!("/api/person/{actor}"), Some(&u.token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(person["name"], extras["cast"][0]["name"]);
    assert_eq!(person["credits"][0]["tmdb_id"], 1396, "their own title first");
    let (status, _) = call(&app, Method::GET, "/api/person/1", Some(&u.token), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, Method::GET, &format!("/api/person/{actor}"), None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = call(&app, Method::GET, "/api/catalog/tv/1396/extras", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &app,
        Method::GET,
        "/api/catalog/movie/999999/extras",
        Some(&u.token),
        None,
    )
    .await;
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

#[sqlx::test(migrations = "./migrations")]
async fn login_through_telegram_app(pool: PgPool) {
    use kino_server::telegram_auth::TelegramUser;
    let app = test_app(pool.clone());

    let (status, link) = call(&app, Method::POST, "/api/auth/link", None, Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    let token = link["token"].as_str().unwrap().to_owned();
    assert_eq!(
        link["url"].as_str().unwrap(),
        format!("https://t.me/kino_test_bot?start=login_{token}")
    );
    // Telegram's /start payload allows at most 64 characters.
    assert!(format!("login_{token}").len() <= 64);

    let (_, s) = call(&app, Method::GET, &format!("/api/auth/link/{token}"), None, None).await;
    assert_eq!(s["status"], "pending");

    // The bot confirms it for the admin.
    let tg = TelegramUser {
        id: ADMIN_TG,
        first_name: Some("Daniil".into()),
        last_name: None,
        username: Some("dan".into()),
        photo_url: None,
    };
    assert!(kino_server::db::confirm_login_link(&pool, &token, &tg).await.unwrap());
    // A second press does nothing.
    assert!(!kino_server::db::confirm_login_link(&pool, &token, &tg).await.unwrap());

    let (status, s) = call(&app, Method::GET, &format!("/api/auth/link/{token}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s["status"], "done");
    let session = s["auth"]["token"].as_str().unwrap();
    let (status, me) = call(&app, Method::GET, "/api/me", Some(session), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["first_name"], "Daniil");

    // Used up.
    let (_, s) = call(&app, Method::GET, &format!("/api/auth/link/{token}"), None, None).await;
    assert_eq!(s["status"], "expired");
    let (_, s) = call(&app, Method::GET, "/api/auth/link/not-a-token!", None, None).await;
    assert_eq!(s["status"], "expired");
}

#[sqlx::test(migrations = "./migrations")]
async fn telegram_app_login_still_needs_an_invite(pool: PgPool) {
    use kino_server::telegram_auth::TelegramUser;
    let app = test_app(pool.clone());
    let (_, link) = call(&app, Method::POST, "/api/auth/link", None, Some(json!({}))).await;
    let token = link["token"].as_str().unwrap();
    let stranger = TelegramUser {
        id: 777,
        first_name: Some("X".into()),
        last_name: None,
        username: None,
        photo_url: None,
    };
    assert!(kino_server::db::confirm_login_link(&pool, token, &stranger)
        .await
        .unwrap());
    let (status, body) = call(&app, Method::GET, &format!("/api/auth/link/{token}"), None, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "not_invited");
}

/// What the bot's `/login` does: make a link, confirm it for the sender, hand the
/// code to the browser, which then logs in once with it.
#[sqlx::test(migrations = "./migrations")]
async fn bot_login_link_signs_the_browser_in_once(pool: PgPool) {
    use kino_server::telegram_auth::TelegramUser;
    let app = test_app(pool.clone());
    assert!(kino_server::db::user_by_telegram(&pool, ADMIN_TG)
        .await
        .unwrap()
        .is_none());
    let admin = TelegramUser {
        id: ADMIN_TG,
        first_name: Some("Админ".into()),
        last_name: None,
        username: None,
        photo_url: None,
    };
    let code = kino_server::db::create_login_link(&pool, None).await.unwrap();
    assert!(kino_server::db::confirm_login_link(&pool, &code, &admin).await.unwrap());

    let (status, done) = call(&app, Method::GET, &format!("/api/auth/link/{code}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(done["status"], "done");
    let me = kino_server::db::user_by_telegram(&pool, ADMIN_TG)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(me.first_name.as_deref(), Some("Админ"));

    // A second try with the same code finds nothing left.
    let (_, again) = call(&app, Method::GET, &format!("/api/auth/link/{code}"), None, None).await;
    assert_eq!(again["status"], "expired");
}

async fn start_mock_telegram(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

#[sqlx::test(migrations = "./migrations")]
async fn telegram_avatar_fetched_via_bot_api_and_served(pool: PgPool) {
    use axum::{http::header, routing::get, Json};
    const FAKE_JPEG: &[u8] = b"\xFF\xD8\xFF\xE0fake-telegram-avatar-jpeg";

    let mock_tg = axum::Router::new()
        .route(
            &format!("/bot{BOT_TOKEN}/getUserProfilePhotos"),
            get(|| async {
                Json(json!({
                    "ok": true,
                    "result": {
                        "total_count": 1,
                        "photos": [[
                            { "file_id": "small", "width": 160 },
                            { "file_id": "medium", "width": 320 }
                        ]]
                    }
                }))
            }),
        )
        .route(
            &format!("/bot{BOT_TOKEN}/getFile"),
            get(
                |axum::extract::Query(q): axum::extract::Query<BTreeMap<String, String>>| async move {
                    assert_eq!(q.get("file_id").map(String::as_str), Some("medium"));
                    Json(json!({ "ok": true, "result": { "file_path": "photos/ava.jpg" } }))
                },
            ),
        )
        .route(
            &format!("/file/bot{BOT_TOKEN}/photos/ava.jpg"),
            get(|| async { ([(header::CONTENT_TYPE, "image/jpeg")], FAKE_JPEG) }),
        );

    let mock_url = start_mock_telegram(mock_tg).await;
    let mut state = test_state(pool);
    let mut cfg = (*state.config).clone();
    cfg.telegram_api_url = Some(mock_url);
    state.config = std::sync::Arc::new(cfg);
    let app = kino_server::app(state);

    let auth = login_ok(&app, ADMIN_TG, None).await;
    let avatar_url = auth.me.avatar_url.expect("avatar_url should be populated");
    assert!(avatar_url.starts_with("/api/avatar/"), "{avatar_url}");

    // Subsequent /api/me keeps returning the same cached avatar URL.
    let (status, me_json) = call(&app, Method::GET, "/api/me", Some(&auth.token), None).await;
    assert_eq!(status, StatusCode::OK);
    let me: Me = serde_json::from_value(me_json).unwrap();
    assert_eq!(me.avatar_url.as_deref(), Some(avatar_url.as_str()));

    // Serving the avatar URL returns the JPEG bytes and strong security/cache headers.
    let (status, headers, body) = raw(&app, Method::GET, &avatar_url, None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "image/jpeg");
    assert_eq!(
        headers.get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
    assert_eq!(body, FAKE_JPEG);

    // Forged avatar token is rejected.
    let (status, _, _) = raw(&app, Method::GET, "/api/avatar/forged.token", None, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "./migrations")]
async fn telegram_avatar_falls_back_to_init_data_photo_url(pool: PgPool) {
    use axum::{http::header, routing::get, Json};
    const FAKE_PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake-png";

    let mock_tg = axum::Router::new()
        .route(
            &format!("/bot{BOT_TOKEN}/getUserProfilePhotos"),
            get(|| async { Json(json!({ "ok": true, "result": { "total_count": 0, "photos": [] } })) }),
        )
        .route(
            "/userpic/320/ava.png",
            get(|| async { ([(header::CONTENT_TYPE, "image/png")], FAKE_PNG) }),
        );

    let mock_url = start_mock_telegram(mock_tg).await;
    let mut state = test_state(pool);
    let mut cfg = (*state.config).clone();
    cfg.telegram_api_url = Some(mock_url.clone());
    state.config = std::sync::Arc::new(cfg);
    let app = kino_server::app(state);

    let photo_url = format!("{mock_url}/userpic/320/ava.png");
    let user = json!({ "id": ADMIN_TG, "first_name": "Даня", "photo_url": photo_url }).to_string();
    let now = chrono::Utc::now().timestamp().to_string();
    let init_data = telegram_auth::sign_webapp(&[("auth_date", &now), ("user", &user)], BOT_TOKEN);

    let (status, body) = call(
        &app,
        Method::POST,
        "/api/auth/webapp",
        None,
        Some(json!({ "init_data": init_data })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let auth: kino_core::AuthResponse = serde_json::from_value(body).unwrap();
    let avatar_url = auth.me.avatar_url.expect("fallback photo_url should be cached");

    let (status, headers, bytes) = raw(&app, Method::GET, &avatar_url, None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "image/png");
    assert_eq!(bytes, FAKE_PNG);
}

#[sqlx::test(migrations = "./migrations")]
async fn personalized_home_lazy_shelves_and_pagination(pool: PgPool) {
    let app = test_app(pool);
    let u = login_ok(&app, ADMIN_TG, None).await;
    let tok = u.token.as_str();

    // Mark "Матрица" (movie 603) watched + 5 stars, and drop "Тёмный рыцарь" (movie 155).
    call(
        &app,
        Method::PUT,
        "/api/title/movie/603/status",
        Some(tok),
        Some(json!({ "status": "watched" })),
    )
    .await;
    call(
        &app,
        Method::PUT,
        "/api/title/movie/603/rating",
        Some(tok),
        Some(json!({ "rating": 5 })),
    )
    .await;
    call(
        &app,
        Method::PUT,
        "/api/title/movie/155/status",
        Some(tok),
        Some(json!({ "status": "dropped" })),
    )
    .await;

    let (status, body) = call(&app, Method::GET, "/api/catalog/home", Some(tok), None).await;
    assert_eq!(status, StatusCode::OK);
    let feed: kino_core::HomeFeed = serde_json::from_value(body).unwrap();
    assert!(!feed.categories.is_empty());
    assert!(feed.rows.iter().any(|r| r.id == "because:movie:603" && r.loaded));
    assert!(
        feed.rows.iter().any(|r| !r.loaded),
        "should keep trailing shelves deferred"
    );

    // Excluded titles (watched 603 and dropped 155) must not appear in loaded shelves.
    for row in feed.rows.iter().filter(|r| r.loaded) {
        for item in &row.items {
            assert!(!(item.kind == kino_core::MediaKind::Movie && (item.tmdb_id == 603 || item.tmdb_id == 155)));
        }
    }

    // Load deferred shelves in batch.
    let deferred: Vec<String> = feed.rows.iter().filter(|r| !r.loaded).map(|r| r.id.clone()).collect();
    let ids = deferred.join(",");
    let (status, batch_body) = call(
        &app,
        Method::GET,
        &format!("/api/catalog/home/shelves?ids={ids}"),
        Some(tok),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let loaded_batch: Vec<kino_core::CatalogRow> = serde_json::from_value(batch_body).unwrap();
    assert_eq!(loaded_batch.len(), deferred.len());
    for row in &loaded_batch {
        assert!(row.loaded);
        assert!(row
            .items
            .iter()
            .all(|i| !(i.kind == kino_core::MediaKind::Movie && (i.tmdb_id == 603 || i.tmdb_id == 155))));
    }

    // Shelf pagination (`top_rated` has 12 remaining titles -> 2+ pages).
    let (status, p1_body) = call(
        &app,
        Method::GET,
        "/api/catalog/shelf/top_rated?page=1",
        Some(tok),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let p1: kino_core::ShelfPage = serde_json::from_value(p1_body).unwrap();
    assert!(p1.has_more);

    let (status, p2_body) = call(
        &app,
        Method::GET,
        "/api/catalog/shelf/top_rated?page=2",
        Some(tok),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let p2: kino_core::ShelfPage = serde_json::from_value(p2_body).unwrap();
    assert!(!p2.items.is_empty());
    assert!(p2
        .items
        .iter()
        .all(|i| !p1.items.iter().any(|x| x.kind == i.kind && x.tmdb_id == i.tmdb_id)));

    let (status, _) = call(
        &app,
        Method::GET,
        "/api/catalog/shelf/no_such_shelf?page=1",
        Some(tok),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "./migrations")]
async fn sync_room_joins_and_protects_timecode(pool: PgPool) {
    let app = test_app(pool);
    let u = login_ok(&app, ADMIN_TG, None).await;
    let tok = u.token.as_str();

    // Seed saved progress at 240s.
    let (status, _) = call(
        &app,
        Method::PUT,
        "/api/progress",
        Some(tok),
        Some(json!({
            "kind": "movie", "tmdb_id": 603, "position_sec": 240.0, "duration_sec": 8000.0,
            "release": { "infohash": HASH, "file_index": 0, "title": "Matrix A" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Device 1 joins sync room -> gets saved 240s and joined_existing = false.
    let (status, body1) = call(
        &app,
        Method::POST,
        "/api/sync/join",
        Some(tok),
        Some(json!({
            "client_id": "dev-1",
            "device": "ПК",
            "kind": "movie",
            "tmdb_id": 603,
            "season": 0,
            "episode": 0,
            "release": { "kind": "movie", "tmdb_id": 603, "season": 0, "infohash": HASH, "title": "Matrix A" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body1}");
    let j1: kino_core::SyncJoinResponse = serde_json::from_value(body1).unwrap();
    assert!(!j1.joined_existing);
    assert!((j1.state.position_sec - 240.0).abs() < 2.0);
    assert_eq!(j1.state.members.len(), 1);
    assert_eq!(j1.state.sound_client_id.as_deref(), Some("dev-1"));

    // Device 2 opens the same movie -> joins existing room, adopts its release, enters barrier.
    let other_hash = "f".repeat(40);
    let (status, body2) = call(
        &app,
        Method::POST,
        "/api/sync/join",
        Some(tok),
        Some(json!({
            "client_id": "dev-2",
            "device": "Телефон",
            "kind": "movie",
            "tmdb_id": 603,
            "season": 0,
            "episode": 0,
            "release": { "kind": "movie", "tmdb_id": 603, "season": 0, "infohash": other_hash, "title": "Matrix B" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body2}");
    let j2: kino_core::SyncJoinResponse = serde_json::from_value(body2).unwrap();
    assert!(j2.joined_existing);
    assert!(j2.state.waiting);
    assert_eq!(j2.state.release.infohash, HASH);
    assert_eq!(j2.state.members.len(), 2);
    assert_eq!(j2.state.sound_client_id.as_deref(), Some("dev-1"));

    // Stale HTTP progress (e.g. 5.0s while room is around 240s) must NOT overwrite DB progress.
    let (status, body_stale) = call(
        &app,
        Method::PUT,
        "/api/progress",
        Some(tok),
        Some(json!({
            "kind": "movie", "tmdb_id": 603, "position_sec": 5.0, "duration_sec": 8000.0
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let p_after: Progress = serde_json::from_value(body_stale).unwrap();
    assert!((p_after.position_sec - 240.0).abs() < 1.0);

    // Leave both devices cleanly (final flush updates DB).
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/sync/leave",
        Some(tok),
        Some(json!({
            "client_id": "dev-2",
            "kind": "movie",
            "tmdb_id": 603,
            "position_sec": 240.0,
            "duration_sec": 8000.0,
            "finished": false
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = call(
        &app,
        Method::POST,
        "/api/sync/leave",
        Some(tok),
        Some(json!({
            "client_id": "dev-1",
            "kind": "movie",
            "tmdb_id": 603,
            "position_sec": 300.0,
            "duration_sec": 8000.0,
            "finished": false
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, prog_body) = call(&app, Method::GET, "/api/progress/movie/603", Some(tok), None).await;
    let saved: Vec<Progress> = serde_json::from_value(prog_body).unwrap();
    assert_eq!(saved.len(), 1);
    assert!((saved[0].position_sec - 300.0).abs() < 1.0);
}
