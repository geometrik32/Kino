//! User state: statuses, ratings, marks, history, prefs, preloads, notifications.
//! Needs `DATABASE_URL`, like `api.rs`.

#[path = "common/mod.rs"]
mod common;

use axum::http::{Method, StatusCode};
use common::*;
use kino_core::{
    CalendarEntry, ContinueItem, HistoryEntry, HomeFeed, PreloadItem, PreloadState, Progress, TitleState, TitleStatus,
    UserPrefs, UserPublicProfile, UserSocialLibrary, VideoScale,
};
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
    assert_eq!(st.video_scale, None);
    let (code, st) = call(
        &app,
        Method::PUT,
        "/api/title/movie/27205/scale",
        Some(&t),
        Some(json!({"video_scale": "height"})),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let st: TitleState = serde_json::from_value(st).unwrap();
    assert_eq!(st.video_scale, Some(VideoScale::FitHeight));
    let (_, st) = call(&app, Method::GET, "/api/title/movie/27205/state", Some(&t), None).await;
    let st: TitleState = serde_json::from_value(st).unwrap();
    assert_eq!(st.video_scale, Some(VideoScale::FitHeight));
    assert_eq!(st.status, Some(TitleStatus::Watched), "rating marks the title watched");
    let (_, p) = call(&app, Method::GET, "/api/progress/movie/27205", Some(&t), None).await;
    assert_eq!(p[0]["finished"], true);

    let (_, p) = call(&app, Method::GET, "/api/prefs", Some(&t), None).await;
    assert_eq!(
        serde_json::from_value::<UserPrefs>(p).unwrap(),
        UserPrefs::recommended_defaults()
    );
    let prefs = json!({
        "quality": "2160p", "voice": null, "subtitle_lang": null, "language": "Русский",
        "hdr": false, "dolby_vision": null, "subtitles": true, "codec": "H.264", "direct_play": true,
        "min_seeders": 5, "max_size_gb": 40, "max_bitrate_mbps": 30, "sort": "bitrate",
        "desktop_quality": null, "mobile_quality": null
    });
    let (code, _) = call(&app, Method::PUT, "/api/prefs", Some(&t), Some(prefs.clone())).await;
    assert_eq!(code, StatusCode::OK);
    let (_, p) = call(&app, Method::GET, "/api/prefs", Some(&t), None).await;
    assert_eq!(p, prefs);
    // Explicitly clearing language and max_bitrate_mbps to null ("Любой") persists and is not overwritten.
    let cleared = json!({
        "quality": null, "voice": null, "subtitle_lang": null, "language": null,
        "hdr": null, "dolby_vision": null, "subtitles": null, "codec": null, "direct_play": null,
        "min_seeders": null, "max_size_gb": null, "max_bitrate_mbps": null, "sort": null,
        "desktop_quality": null, "mobile_quality": null
    });
    let (code, _) = call(&app, Method::PUT, "/api/prefs", Some(&t), Some(cleared)).await;
    assert_eq!(code, StatusCode::OK);
    let (_, p) = call(&app, Method::GET, "/api/prefs", Some(&t), None).await;
    assert_eq!(serde_json::from_value::<UserPrefs>(p).unwrap(), UserPrefs::default());
    // An old client that knows only the first three fields still works, and clears the rest.
    let old = json!({"quality": "1080p", "voice": "LostFilm", "subtitle_lang": null});
    call(&app, Method::PUT, "/api/prefs", Some(&t), Some(old)).await;
    let (_, p) = call(&app, Method::GET, "/api/prefs", Some(&t), None).await;
    let p: UserPrefs = serde_json::from_value(p).unwrap();
    assert_eq!(
        (p.quality.as_deref(), p.voice.as_deref(), p.language, p.max_bitrate_mbps),
        (Some("1080p"), Some("LostFilm"), None, None)
    );
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
    let send = |chat: i64, text: String, url: String| {
        sent.lock().unwrap().push((chat, text, url));
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
    assert!(
        msgs[0].2.ends_with("/?open=tv_1396"),
        "the button opens the series: {}",
        msgs[0].2
    );
    assert_eq!(
        status(&app, &t, "tv/1396").await,
        Some(TitleStatus::Watching),
        "back to watching"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn rating_marks_film_and_all_series_episodes_watched(pool: PgPool) {
    let app = test_app(pool);
    let t = login_ok(&app, ADMIN_TG, None).await.token;

    // Half-watched movie dropped out of "continue" and marked finished once rated.
    play(
        &app,
        &t,
        json!({"kind": "movie", "tmdb_id": 603, "position_sec": 1200.0, "duration_sec": 8000.0}),
    )
    .await;
    let (code, body) = call(
        &app,
        Method::PUT,
        "/api/title/movie/603/rating",
        Some(&t),
        Some(json!({"rating": 4})),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let st: TitleState = serde_json::from_value(body).unwrap();
    assert_eq!(st.status, Some(TitleStatus::Watched));
    assert_eq!(st.rating, Some(4));

    let (_, prog) = call(&app, Method::GET, "/api/progress/movie/603", Some(&t), None).await;
    let prog: Vec<Progress> = serde_json::from_value(prog).unwrap();
    assert_eq!(prog.len(), 1);
    assert!(prog[0].finished);

    // Series "Тьма" (70523): 3 seasons of 10, 8, 8 episodes = 26 episodes.
    play(
        &app,
        &t,
        json!({"kind": "tv", "tmdb_id": 70523, "season": 1, "episode": 2,
               "position_sec": 300.0, "duration_sec": 3200.0}),
    )
    .await;
    call(
        &app,
        Method::PUT,
        "/api/title/tv/70523/status",
        Some(&t),
        Some(json!({"status": "dropped"})),
    )
    .await;
    call(
        &app,
        Method::POST,
        "/api/preload",
        Some(&t),
        Some(json!({
            "kind": "tv",
            "tmdb_id": 70523,
            "season": 2,
            "infohash": "cd".repeat(20),
            "title": "S02"
        })),
    )
    .await;

    let (code, body) = call(
        &app,
        Method::PUT,
        "/api/title/tv/70523/rating",
        Some(&t),
        Some(json!({"rating": 5})),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let st: TitleState = serde_json::from_value(body).unwrap();
    assert_eq!(st.status, Some(TitleStatus::Watched));
    assert_eq!(st.rating, Some(5));

    let (_, prog) = call(&app, Method::GET, "/api/progress/tv/70523", Some(&t), None).await;
    let prog: Vec<Progress> = serde_json::from_value(prog).unwrap();
    let expected: Vec<(i32, i32)> = [(1, 10), (2, 8), (3, 8)]
        .into_iter()
        .flat_map(|(s, count)| (1..=count).map(move |e| (s, e)))
        .collect();
    assert_eq!(
        prog.iter()
            .filter(|p| p.finished)
            .map(|p| (p.season, p.episode))
            .collect::<Vec<_>>(),
        expected
    );

    let (_, cont) = call(&app, Method::GET, "/api/continue", Some(&t), None).await;
    assert_eq!(cont, json!([]), "rated titles leave continue watching");

    let (_, preloads) = call(&app, Method::GET, "/api/preload", Some(&t), None).await;
    let preloads: Vec<PreloadItem> = serde_json::from_value(preloads).unwrap();
    assert!(
        preloads[0].delete_after.is_some(),
        "completed season preload scheduled for removal"
    );

    // Removing the rating keeps status and episode marks intact.
    let (code, body) = call(
        &app,
        Method::PUT,
        "/api/title/tv/70523/rating",
        Some(&t),
        Some(json!({"rating": null})),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let st: TitleState = serde_json::from_value(body).unwrap();
    assert_eq!(st.rating, None);
    assert_eq!(st.status, Some(TitleStatus::Watched));
    let (_, prog) = call(&app, Method::GET, "/api/progress/tv/70523", Some(&t), None).await;
    let prog: Vec<Progress> = serde_json::from_value(prog).unwrap();
    assert_eq!(prog.iter().filter(|p| p.finished).count(), 26);
}

#[sqlx::test(migrations = "./migrations")]
async fn hierarchical_ratings_80_percent_and_no_top_down_overwrite(pool: PgPool) {
    let app = test_app(pool);
    let admin = login_ok(&app, ADMIN_TG, None).await;
    let t = admin.token.as_str();

    // Rate S02 manually 3: marks S02 episodes watched, series becomes Watching, no episode ratings overwritten.
    let (code, body) = call(
        &app,
        Method::PUT,
        "/api/title/tv/70523/season/2/rating",
        Some(t),
        Some(json!({ "rating": 3 })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let st: TitleState = serde_json::from_value(body).unwrap();
    assert_eq!(st.status, Some(TitleStatus::Watching));
    assert!(st.episode_ratings.is_empty());
    let s2 = st.season_ratings.iter().find(|r| r.season == 2).unwrap();
    assert_eq!(s2.manual_rating, Some(3));
    assert_eq!(s2.effective_rating, Some(3.0));

    // Rate 6/8 episodes of S02 (75% < 80%): manual season rating 3 still dominates.
    for ep in 1..=6 {
        let (code, _) = call(
            &app,
            Method::PUT,
            &format!("/api/title/tv/70523/season/2/episode/{ep}/rating"),
            Some(t),
            Some(json!({ "rating": 5 })),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
    }
    let (_, body) = call(&app, Method::GET, "/api/title/tv/70523/state", Some(t), None).await;
    let st: TitleState = serde_json::from_value(body).unwrap();
    let s2 = st.season_ratings.iter().find(|r| r.season == 2).unwrap();
    assert_eq!(s2.effective_rating, Some(3.0));

    // Rate 7th episode (7/8 = 87.5% >= 80%): mean 5.0 dominates manual 3; 8th ep = 3 -> (35+3)/8 = 4.75.
    call(
        &app,
        Method::PUT,
        "/api/title/tv/70523/season/2/episode/7/rating",
        Some(t),
        Some(json!({ "rating": 5 })),
    )
    .await;
    let (_, body) = call(&app, Method::GET, "/api/title/tv/70523/state", Some(t), None).await;
    let st: TitleState = serde_json::from_value(body).unwrap();
    let s2 = st.season_ratings.iter().find(|r| r.season == 2).unwrap();
    assert_eq!(s2.manual_rating, Some(3));
    assert_eq!(s2.effective_rating, Some(5.0));

    call(
        &app,
        Method::PUT,
        "/api/title/tv/70523/season/2/episode/8/rating",
        Some(t),
        Some(json!({ "rating": 3 })),
    )
    .await;

    // Rate S01 & S03 manually -> 3/3 seasons >= 80% -> overrides manual series rating of 2.
    call(
        &app,
        Method::PUT,
        "/api/title/tv/70523/status",
        Some(t),
        Some(json!({ "status": "watching" })),
    )
    .await;
    call(
        &app,
        Method::PUT,
        "/api/title/tv/70523/season/1/rating",
        Some(t),
        Some(json!({ "rating": 4 })),
    )
    .await;
    call(
        &app,
        Method::PUT,
        "/api/title/tv/70523/season/3/rating",
        Some(t),
        Some(json!({ "rating": 3 })),
    )
    .await;

    let (_, body) = call(&app, Method::GET, "/api/title/tv/70523/state", Some(t), None).await;
    let st: TitleState = serde_json::from_value(body).unwrap();
    let eff = st.effective_rating.expect("80%+ seasons gives mean");
    let expected_mean = (4.0 + 4.75 + 3.0) / 3.0;
    assert!(
        (eff - expected_mean).abs() < 0.01,
        "got {eff}, expected {expected_mean}"
    );

    // Top-down manual rating on a fresh series does NOT populate season or episode ratings.
    let (code, body) = call(
        &app,
        Method::PUT,
        "/api/title/tv/1396/rating",
        Some(t),
        Some(json!({ "rating": 4 })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let st2: TitleState = serde_json::from_value(body).unwrap();
    assert_eq!(st2.rating, Some(4));
    assert_eq!(st2.effective_rating, Some(4.0));
    assert!(st2.season_ratings.is_empty());
    assert!(st2.episode_ratings.is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn social_users_calendar_and_rewatch_excludes_unfinished(pool: PgPool) {
    let app = test_app(pool);
    let admin = login_ok(&app, ADMIN_TG, None).await;
    let (_, inv) = call(&app, Method::POST, "/api/invites", Some(&admin.token), None).await;
    let peer = login_ok(&app, 2002, inv["code"].as_str()).await;

    // Movie 603 finished + 5★ -> eligible for rewatch; Movie 27205 watching + 5★ -> excluded from rewatch.
    call(
        &app,
        Method::PUT,
        "/api/title/movie/603/status",
        Some(&admin.token),
        Some(json!({ "status": "watched" })),
    )
    .await;
    call(
        &app,
        Method::PUT,
        "/api/title/movie/603/rating",
        Some(&admin.token),
        Some(json!({ "rating": 5 })),
    )
    .await;
    call(
        &app,
        Method::PUT,
        "/api/title/movie/27205/rating",
        Some(&admin.token),
        Some(json!({ "rating": 5 })),
    )
    .await;
    call(
        &app,
        Method::PUT,
        "/api/title/movie/27205/status",
        Some(&admin.token),
        Some(json!({ "status": "watching" })),
    )
    .await;

    let (code, home_body) = call(&app, Method::GET, "/api/catalog/home", Some(&admin.token), None).await;
    assert_eq!(code, StatusCode::OK);
    let home: HomeFeed = serde_json::from_value(home_body).unwrap();
    let rewatch = home.rows.iter().find(|r| r.id == "rewatch").expect("rewatch shelf");
    assert!(rewatch.items.iter().any(|i| i.tmdb_id == 603));
    assert!(!rewatch.items.iter().any(|i| i.tmdb_id == 27205));

    // Peer sees admin in /api/users and /api/users/{id}/titles and sees peer badges on title state & home.
    let (code, users_body) = call(&app, Method::GET, "/api/users", Some(&peer.token), None).await;
    assert_eq!(code, StatusCode::OK);
    let users: Vec<UserPublicProfile> = serde_json::from_value(users_body).unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].id, admin.me.id);
    assert_eq!(users[0].watched_count, 1);

    let (code, lib_body) = call(
        &app,
        Method::GET,
        &format!("/api/users/{}/titles", admin.me.id),
        Some(&peer.token),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let lib: UserSocialLibrary = serde_json::from_value(lib_body).unwrap();
    assert_eq!(lib.items.len(), 2);

    let (_, st_body) = call(&app, Method::GET, "/api/title/movie/603/state", Some(&peer.token), None).await;
    let peer_st: TitleState = serde_json::from_value(st_body).unwrap();
    assert_eq!(peer_st.peers.len(), 1);
    assert_eq!(peer_st.peers[0].rating, Some(5.0));

    // WaitingRelease is forbidden on TV series.
    let (code, _) = call(
        &app,
        Method::PUT,
        "/api/title/tv/1396/status",
        Some(&peer.token),
        Some(json!({ "status": "waiting_release" })),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    let (code, cal_body) = call(&app, Method::GET, "/api/calendar", Some(&peer.token), None).await;
    assert_eq!(code, StatusCode::OK);
    let _: Vec<CalendarEntry> = serde_json::from_value(cal_body).unwrap();
}

fn state_with_preload_keep_2d(pool: PgPool) -> kino_server::AppState {
    let base = test_state(pool.clone());
    let mut cfg = (*base.config).clone();
    cfg.preload_keep = chrono::Duration::days(2);
    let cfg = std::sync::Arc::new(cfg);
    kino_server::AppState {
        pool,
        media: std::sync::Arc::new(kino_server::play::Media::new(&cfg.torrents)),
        sync: std::sync::Arc::new(kino_server::sync::SyncHub::new()),
        config: cfg,
        catalog: std::sync::Arc::new(kino_server::catalog::DemoCatalog),
        telegram: reqwest::Client::new(),
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn preload_retention_follows_owner_then_extends_for_active_peers(pool: PgPool) {
    let state = state_with_preload_keep_2d(pool.clone());
    let app = kino_server::app(state.clone());
    let owner = login_ok(&app, ADMIN_TG, None).await;
    let (_, inv) = call(&app, Method::POST, "/api/invites", Some(&owner.token), None).await;
    let peer = login_ok(&app, 3003, inv["code"].as_str()).await;

    // Peer already rated and finished the movie before the owner even queued a download.
    call(
        &app,
        Method::PUT,
        "/api/title/movie/603/rating",
        Some(&peer.token),
        Some(json!({ "rating": 2 })),
    )
    .await;

    let hash = "bc".repeat(20);
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/preload",
        Some(&owner.token),
        Some(json!({ "kind": "movie", "tmdb_id": 603, "infohash": hash, "title": "Матрица 1080p" })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");

    // Simulate download completion: 7-day retention, not watched by owner yet.
    kino_server::userdata::preload_progress(&pool, &hash, PreloadState::Ready, 1.0, Some(1_000_000))
        .await
        .unwrap();

    let (_, list) = call(&app, Method::GET, "/api/preload", Some(&owner.token), None).await;
    let list: Vec<PreloadItem> = serde_json::from_value(list).unwrap();
    assert!(!list[0].watched);
    let initial_delete = list[0].delete_after.expect("ready preload gets 7-day deadline");
    assert!(
        (initial_delete - chrono::Utc::now()).num_hours() > 150,
        "expected ~7 days, got {initial_delete}"
    );

    // Peer starts rewatching (their watch_progress has sticky finished=true from rating),
    // and owner only watched 18 seconds: neither may shorten the 7-day timer or mark watched.
    play(
        &app,
        &peer.token,
        json!({"kind": "movie", "tmdb_id": 603, "position_sec": 1200.0, "duration_sec": 8000.0}),
    )
    .await;
    play(
        &app,
        &owner.token,
        json!({"kind": "movie", "tmdb_id": 603, "position_sec": 18.0, "duration_sec": 8000.0}),
    )
    .await;

    let (_, list) = call(&app, Method::GET, "/api/preload", Some(&owner.token), None).await;
    let list: Vec<PreloadItem> = serde_json::from_value(list).unwrap();
    assert!(!list[0].watched, "peer activity must not mark owner preload watched");
    assert_eq!(
        list[0].delete_after,
        Some(initial_delete),
        "peer watch must not shorten owner's 7-day retention"
    );

    // Now the owner watches to completion -> watched=true and timer drops to ~2 days.
    play(
        &app,
        &owner.token,
        json!({"kind": "movie", "tmdb_id": 603, "position_sec": 7900.0, "duration_sec": 8000.0}),
    )
    .await;

    let (_, list) = call(&app, Method::GET, "/api/preload", Some(&owner.token), None).await;
    let list: Vec<PreloadItem> = serde_json::from_value(list).unwrap();
    assert!(list[0].watched);
    let post_watch_del = list[0].delete_after.unwrap();
    assert!(
        (post_watch_del - chrono::Utc::now()).num_hours() <= 49,
        "expected ~2 days after owner finished, got {post_watch_del}"
    );

    // Move deadline close to expiry (2 hours left), then peer actively watches mid-film:
    // Variant A extends the 2-day timer back out so the peer isn't cut off mid-watch.
    sqlx::query("UPDATE preload SET delete_after = now() + interval '2 hours' WHERE infohash = $1")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();

    play(
        &app,
        &peer.token,
        json!({"kind": "movie", "tmdb_id": 603, "position_sec": 2400.0, "duration_sec": 8000.0}),
    )
    .await;

    let (_, list) = call(&app, Method::GET, "/api/preload", Some(&owner.token), None).await;
    let list: Vec<PreloadItem> = serde_json::from_value(list).unwrap();
    assert!(list[0].watched);
    let extended_del = list[0].delete_after.unwrap();
    assert!(
        (extended_del - chrono::Utc::now()).num_hours() >= 46,
        "expected peer active watch to extend post-watch timer to ~2 days, got {extended_del}"
    );

    // Rewatching a movie the owner already finished earlier: starting a fresh preload at 10s
    // must keep 7 days until the owner actually finishes again.
    let hash2 = "bd".repeat(20);
    let (code, _) = call(
        &app,
        Method::POST,
        "/api/preload",
        Some(&owner.token),
        Some(json!({ "kind": "movie", "tmdb_id": 603, "infohash": hash2, "title": "Матрица 4K" })),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    kino_server::userdata::preload_progress(&pool, &hash2, PreloadState::Ready, 1.0, Some(2_000_000))
        .await
        .unwrap();
    play(
        &app,
        &owner.token,
        json!({"kind": "movie", "tmdb_id": 603, "position_sec": 10.0, "duration_sec": 8000.0}),
    )
    .await;
    let (_, list) = call(&app, Method::GET, "/api/preload", Some(&owner.token), None).await;
    let list: Vec<PreloadItem> = serde_json::from_value(list).unwrap();
    let rewatch_item = list.iter().find(|p| p.infohash == hash2).unwrap();
    assert!(!rewatch_item.watched);
    assert!((rewatch_item.delete_after.unwrap() - chrono::Utc::now()).num_hours() > 150);
}

#[sqlx::test(migrations = "./migrations")]
async fn partial_episode_retention_follows_owner_then_extends_for_peers(pool: PgPool) {
    let state = state_with_preload_keep_2d(pool.clone());
    let app = kino_server::app(state.clone());
    let owner = login_ok(&app, ADMIN_TG, None).await;
    let (_, inv) = call(&app, Method::POST, "/api/invites", Some(&owner.token), None).await;
    let peer = login_ok(&app, 3004, inv["code"].as_str()).await;

    let hash = "de".repeat(20);
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/preload",
        Some(&owner.token),
        Some(json!({
            "kind": "tv",
            "tmdb_id": 1396,
            "season": 1,
            "episode": 1,
            "infohash": hash,
            "title": "Breaking Bad S01"
        })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let item: PreloadItem = serde_json::from_value(body).unwrap();
    kino_server::userdata::upsert_preload_episode_progress(
        &pool,
        item.id,
        1,
        1,
        Some(1),
        Some("S01E01.mkv"),
        Some(500_000),
        PreloadState::Ready,
        1.0,
        false,
    )
    .await
    .unwrap();
    kino_server::userdata::update_preload_row_progress(&pool, item.id, PreloadState::Ready, 1.0, Some(500_000))
        .await
        .unwrap();

    // Peer finishes S01E01 before the owner: episode stays unwatched with 7-day retention.
    play(
        &app,
        &peer.token,
        json!({"kind": "tv", "tmdb_id": 1396, "season": 1, "episode": 1, "position_sec": 2900.0, "duration_sec": 3000.0}),
    )
    .await;
    let (_, list) = call(&app, Method::GET, "/api/preload", Some(&owner.token), None).await;
    let list: Vec<PreloadItem> = serde_json::from_value(list).unwrap();
    let ep = list[0].episode_for(1, 1).unwrap();
    assert!(!ep.watched);
    assert!((ep.delete_after.unwrap() - chrono::Utc::now()).num_hours() > 150);

    // Owner finishes S01E01: episode becomes watched with ~2-day retention.
    play(
        &app,
        &owner.token,
        json!({"kind": "tv", "tmdb_id": 1396, "season": 1, "episode": 1, "position_sec": 2950.0, "duration_sec": 3000.0}),
    )
    .await;
    let (_, list) = call(&app, Method::GET, "/api/preload", Some(&owner.token), None).await;
    let list: Vec<PreloadItem> = serde_json::from_value(list).unwrap();
    let ep = list[0].episode_for(1, 1).unwrap();
    assert!(ep.watched);
    assert!((ep.delete_after.unwrap() - chrono::Utc::now()).num_hours() <= 49);

    // Peer starts watching S01E01 when only 1h remains -> extended back to ~2 days.
    sqlx::query("UPDATE preload_episode SET delete_after = now() + interval '1 hour' WHERE preload_id = $1")
        .bind(item.id)
        .execute(&pool)
        .await
        .unwrap();
    play(
        &app,
        &peer.token,
        json!({"kind": "tv", "tmdb_id": 1396, "season": 1, "episode": 1, "position_sec": 500.0, "duration_sec": 3000.0}),
    )
    .await;
    let (_, list) = call(&app, Method::GET, "/api/preload", Some(&owner.token), None).await;
    let list: Vec<PreloadItem> = serde_json::from_value(list).unwrap();
    let ep = list[0].episode_for(1, 1).unwrap();
    assert!(ep.watched);
    assert!((ep.delete_after.unwrap() - chrono::Utc::now()).num_hours() >= 46);
}
