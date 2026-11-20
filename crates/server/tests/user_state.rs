//! User state: statuses, ratings, marks, history, prefs, preloads, notifications.
//! Needs `DATABASE_URL`, like `api.rs`.

#[path = "common/mod.rs"]
mod common;

use axum::http::{Method, StatusCode};
use common::*;
use kino_core::{ContinueItem, HistoryEntry, PreloadItem, PreloadState, TitleState, TitleStatus, UserPrefs};
use serde_json::json;
use sqlx::PgPool;

async fn status(app: &axum::Router, token: &str, path: &str) -> Option<TitleStatus> {
    let (code, body) = call(app, Method::GET, &format!("/api/title/{path}/state"), Some(token), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    serde_json::from_value::<TitleState>(body).unwrap().status
}

async fn play(app: &axum::Router, token: &str, body: serde_json::Value) {
    let (code, resp) = call(app, Method::PUT, "/api/progress", Some(token), Some(body)).await;
    assert_eq!(code, StatusCode::OK, "{resp}");
}

#[sqlx::test(migrations = "./migrations")]
async fn status_follows_watching(pool: PgPool) {
    let app = test_app(pool);
    let u = login_ok(&app, ADMIN_TG, None).await;
    let t = u.token.as_str();

    // Film: want → watching on play → watched when finished.
    call(&app, Method::PUT, "/api/watchlist/movie/603", Some(t), None).await;
    assert_eq!(status(&app, t, "movie/603").await, Some(TitleStatus::Want));
    play(
        &app,
        t,
        json!({"kind": "movie", "tmdb_id": 603, "position_sec": 60.0, "duration_sec": 8000.0}),
    )
    .await;
    assert_eq!(status(&app, t, "movie/603").await, Some(TitleStatus::Watching));
    play(
        &app,
        t,
        json!({"kind": "movie", "tmdb_id": 603, "position_sec": 7900.0, "duration_sec": 8000.0}),
    )
    .await;
    assert_eq!(status(&app, t, "movie/603").await, Some(TitleStatus::Watched));

    // Series "Тьма": 3 seasons, the last aired episode is S03E08.
    play(
        &app,
        t,
        json!({"kind": "tv", "tmdb_id": 70523, "season": 1, "episode": 4,
                         "position_sec": 100.0, "duration_sec": 3300.0}),
    )
    .await;
    assert_eq!(status(&app, t, "tv/70523").await, Some(TitleStatus::Watching));
    let mark = |watched: bool, eps: Vec<i32>| json!({"kind": "tv", "tmdb_id": 70523, "season": 3, "episodes": eps, "watched": watched});
    let (code, _) = call(
        &app,
        Method::POST,
        "/api/marks",
        Some(t),
        Some(mark(true, (1..=8).collect())),
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    assert_eq!(status(&app, t, "tv/70523").await, Some(TitleStatus::Watched));
    call(&app, Method::POST, "/api/marks", Some(t), Some(mark(false, vec![8]))).await;
    assert_eq!(status(&app, t, "tv/70523").await, Some(TitleStatus::Watching));
    let (code, _) = call(&app, Method::POST, "/api/marks", Some(t), Some(mark(true, vec![]))).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    // Dropped: hidden from "continue", stays dropped on marks, back to watching on play.
    let (code, _) = call(
        &app,
        Method::PUT,
        "/api/title/tv/70523/status",
        Some(t),
        Some(json!({"status": "dropped"})),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let (_, cont) = call(&app, Method::GET, "/api/continue", Some(t), None).await;
    let cont: Vec<ContinueItem> = serde_json::from_value(cont).unwrap();
    assert!(cont.iter().all(|c| c.last.tmdb_id != 70523), "dropped is hidden");
    assert!(cont.iter().all(|c| c.last.tmdb_id != 603), "watched film is hidden");
    call(&app, Method::POST, "/api/marks", Some(t), Some(mark(true, vec![1]))).await;
    assert_eq!(status(&app, t, "tv/70523").await, Some(TitleStatus::Dropped));
    play(
        &app,
        t,
        json!({"kind": "tv", "tmdb_id": 70523, "season": 1, "episode": 5,
                         "position_sec": 10.0, "duration_sec": 3300.0}),
    )
    .await;
    assert_eq!(status(&app, t, "tv/70523").await, Some(TitleStatus::Watching));

    // "Мои" lists by status with names.
    let (_, mine) = call(&app, Method::GET, "/api/my?status=watched", Some(t), None).await;
    let mine: Vec<TitleState> = serde_json::from_value(mine).unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].title.as_ref().unwrap().title, "Матрица");
    let (code, _) = call(&app, Method::GET, "/api/my?status=paused", Some(t), None).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    // Marking a film watched by status also counts it as finished.
    call(
        &app,
        Method::PUT,
        "/api/title/movie/550/status",
        Some(t),
        Some(json!({"status": "watched"})),
    )
    .await;
    let (_, p) = call(&app, Method::GET, "/api/progress/movie/550", Some(t), None).await;
    assert_eq!(p[0]["finished"], true);
}

#[sqlx::test(migrations = "./migrations")]
async fn rating_tracks_prefs(pool: PgPool) {
    let app = test_app(pool);
    let t = login_ok(&app, ADMIN_TG, None).await.token;
    for (r, code) in [
        (json!(5), StatusCode::OK),
        (json!(0), StatusCode::BAD_REQUEST),
        (json!(6), StatusCode::BAD_REQUEST),
    ] {
        let (c, _) = call(
            &app,
            Method::PUT,
            "/api/title/movie/27205/rating",
            Some(&t),
            Some(json!({"rating": r})),
        )
        .await;
        assert_eq!(c, code);
    }
    let (_, st) = call(
        &app,
        Method::PUT,
        "/api/title/movie/27205/tracks",
        Some(&t),
        Some(json!({"audio_track": "Дубляж", "subtitle_track": null})),
    )
    .await;
    let st: TitleState = serde_json::from_value(st).unwrap();
    assert_eq!(st.rating, Some(5));
    assert_eq!(st.audio_track.as_deref(), Some("Дубляж"));
    assert_eq!(st.subtitle_track, None);
    assert_eq!(st.status, None, "rating alone sets no status");

    let (_, p) = call(&app, Method::GET, "/api/prefs", Some(&t), None).await;
    assert_eq!(serde_json::from_value::<UserPrefs>(p).unwrap(), UserPrefs::default());
    let prefs = json!({"quality": "2160p", "voice": "LostFilm", "subtitle_lang": null});
    call(&app, Method::PUT, "/api/prefs", Some(&t), Some(prefs.clone())).await;
    let (_, p) = call(&app, Method::GET, "/api/prefs", Some(&t), None).await;
    assert_eq!(p, prefs);
}

#[sqlx::test(migrations = "./migrations")]
async fn history_groups_sessions(pool: PgPool) {
    let app = test_app(pool.clone());
    let t = login_ok(&app, ADMIN_TG, None).await.token;
    for pos in [10.0, 20.0, 30.0] {
        play(
            &app,
            &t,
            json!({"kind": "movie", "tmdb_id": 13, "position_sec": pos, "duration_sec": 8000.0, "device": "Телефон"}),
        )
        .await;
    }
    // A later sitting, after a break.
    sqlx::query("UPDATE watch_history SET last_at = now() - interval '2 hours'")
        .execute(&pool)
        .await
        .unwrap();
    play(
        &app,
        &t,
        json!({"kind": "movie", "tmdb_id": 13, "position_sec": 40.0, "duration_sec": 8000.0, "device": "ПК"}),
    )
    .await;

    let (_, h) = call(&app, Method::GET, "/api/history", Some(&t), None).await;
    let h: Vec<HistoryEntry> = serde_json::from_value(h).unwrap();
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].device.as_deref(), Some("ПК"));
    assert_eq!((h[1].position_from, h[1].position_to), (10.0, 30.0));
    assert_eq!(h[1].title.as_ref().unwrap().title, "Форрест Гамп");
}

#[sqlx::test(migrations = "./migrations")]
async fn preload_lifecycle(pool: PgPool) {
    let state = test_state(pool.clone());
    let app = kino_server::app(state.clone());
    let t = login_ok(&app, ADMIN_TG, None).await.token;
    let req = json!({"kind": "movie", "tmdb_id": 157336, "infohash": "AB".repeat(20), "title": "Интерстеллар 2160p"});
    let (code, item) = call(&app, Method::POST, "/api/preload", Some(&t), Some(req.clone())).await;
    assert_eq!(code, StatusCode::OK, "{item}");
    let item: PreloadItem = serde_json::from_value(item).unwrap();
    assert_eq!(item.state, PreloadState::Queued);
    assert_eq!(item.infohash, "ab".repeat(20));
    assert_eq!(item.delete_after, None);

    // Watching to the end schedules removal; the job then marks it deleted.
    play(
        &app,
        &t,
        json!({"kind": "movie", "tmdb_id": 157336, "position_sec": 9900.0, "duration_sec": 10000.0}),
    )
    .await;
    let (_, list) = call(&app, Method::GET, "/api/preload", Some(&t), None).await;
    let list: Vec<PreloadItem> = serde_json::from_value(list).unwrap();
    assert!(list[0].delete_after.is_some());
    assert_eq!(
        kino_server::jobs::expire_preloads(&state).await.unwrap(),
        0,
        "not due yet"
    );
    sqlx::query("UPDATE preload SET delete_after = now() - interval '1 minute'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(kino_server::jobs::expire_preloads(&state).await.unwrap(), 1);
    let (_, list) = call(&app, Method::GET, "/api/preload", Some(&t), None).await;
    assert_eq!(list, json!([]));

    // Asking again re-queues; deleting by hand works once.
    let (_, item) = call(&app, Method::POST, "/api/preload", Some(&t), Some(req)).await;
    let item: PreloadItem = serde_json::from_value(item).unwrap();
    assert_eq!(item.state, PreloadState::Queued);
    let path = format!("/api/preload/{}", item.id);
    assert_eq!(
        call(&app, Method::DELETE, &path, Some(&t), None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, Method::DELETE, &path, Some(&t), None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn new_episode_notification(pool: PgPool) {
    let state = test_state(pool.clone());
    let app = kino_server::app(state.clone());
    let t = login_ok(&app, ADMIN_TG, None).await.token;
    // "Во все тяжкие": last aired S05E16. The user has watched everything.
    call(
        &app,
        Method::POST,
        "/api/marks",
        Some(&t),
        Some(json!({"kind": "tv", "tmdb_id": 1396, "season": 5, "episodes": [16], "watched": true})),
    )
    .await;
    assert_eq!(status(&app, &t, "tv/1396").await, Some(TitleStatus::Watched));

    let sent = std::sync::Mutex::new(Vec::new());
    let send = |chat: i64, text: String| {
        sent.lock().unwrap().push((chat, text));
        async { Ok(()) }
    };
    // The first check only remembers where the series is.
    assert_eq!(kino_server::jobs::check_new_episodes(&state, send).await.unwrap(), 0);
    // Pretend the user was last told about S05E15: S05E16 is new for them.
    sqlx::query("UPDATE title_state SET notified_episode = 15")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(kino_server::jobs::check_new_episodes(&state, send).await.unwrap(), 1);
    assert_eq!(
        kino_server::jobs::check_new_episodes(&state, send).await.unwrap(),
        0,
        "only once"
    );
    let msgs = sent.lock().unwrap().clone();
    assert_eq!(msgs[0].0, ADMIN_TG);
    assert!(
        msgs[0].1.contains("S05E16") && msgs[0].1.contains("Во все тяжкие"),
        "{}",
        msgs[0].1
    );
    assert_eq!(
        status(&app, &t, "tv/1396").await,
        Some(TitleStatus::Watching),
        "back to watching"
    );
}
