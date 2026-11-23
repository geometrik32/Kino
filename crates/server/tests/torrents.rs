//! Releases, playback, streaming and preloads against fake Jackett, TorrServer
//! and qBittorrent. Needs `DATABASE_URL`, like `api.rs`.

#[path = "common/mod.rs"]
mod common;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
};

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Redirect},
    routing::{get, post},
    Json, Router,
};
use common::*;
use kino_core::{
    PlayInfo, PreloadItem, PreloadState, ReleaseList, ResolveReleaseResponse, StreamPhase, StreamProgress, UserPrefs,
};
use kino_server::{app, config::TorrentConfig, jobs};
use serde_json::json;
use sqlx::PgPool;

const EPISODES: [&str; 2] = [
    "Breaking Bad S02/Breaking.Bad.S02E01.mp4",
    "Breaking Bad S02/Breaking.Bad.S02E02.mp4",
];

fn bstr(s: &str) -> String {
    format!("{}:{s}", s.len())
}

/// A multi-file `.torrent` with the two episodes.
fn torrent() -> Vec<u8> {
    let files: String = EPISODES
        .iter()
        .map(|p| {
            let file = p.split_once('/').unwrap().1;
            format!("d6:lengthi1000e4:pathl{}ee", bstr(file))
        })
        .collect();
    format!(
        "d8:announce{}4:infod5:filesl{files}e4:name{}12:piece lengthi16384e6:pieces0:ee",
        bstr("http://tracker/announce"),
        bstr("Breaking Bad S02")
    )
    .into_bytes()
}

/// Jackett: the fixture feed with links pointing back at itself, the
/// `.torrent` behind one link and a redirect to a magnet behind another.
async fn start_jackett() -> String {
    start_jackett_counted(Arc::new(AtomicU32::new(0))).await
}

/// The same, counting every search it is asked for.
async fn start_jackett_counted(hits: Arc<AtomicU32>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let feed = include_str!("fixtures/torznab.xml").replace("http://jackett:9117", &base);
    let router = Router::new()
        .route(
            "/api/v2.0/indexers/all/results/torznab/api",
            get(move |Query(q): Query<Vec<(String, String)>>| async move {
                assert!(q.iter().any(|(k, v)| k == "apikey" && v == "secret"));
                hits.fetch_add(1, Ordering::SeqCst);
                feed
            }),
        )
        .route("/dl/rutracker/", get(|| async { torrent() }))
        .route(
            "/dl/kinozal/",
            get(|| async { Redirect::to("magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a") }),
        );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

fn file_stats() -> serde_json::Value {
    json!({
        "hash": "",
        // TorrServer is preloading: 8 of 32 MB, nine peers.
        "stat": 2, "preloaded_bytes": 8 << 20, "preload_size": 32 << 20,
        "active_peers": 9, "connected_seeders": 4, "download_speed": 1_048_576.0,
        "file_stats": EPISODES.iter().enumerate()
            .map(|(i, p)| json!({ "id": i + 1, "path": p, "length": 1000 }))
            .collect::<Vec<_>>()
    })
}

/// TorrServer: knows the two files and streams `EPISODE-<index>` bytes with Range.
fn torrserver() -> Router {
    async fn stream(Query(q): Query<Vec<(String, String)>>, headers: HeaderMap) -> impl IntoResponse {
        let index = q.iter().find(|(k, _)| k == "index").map(|(_, v)| v.clone()).unwrap();
        let body = format!("EPISODE-{index}").repeat(10).into_bytes();
        match headers.get(header::RANGE).and_then(|r| r.to_str().ok()) {
            Some(r) => {
                let from: usize = r.trim_start_matches("bytes=").trim_end_matches('-').parse().unwrap();
                let range = format!("bytes {from}-{}/{}", body.len() - 1, body.len());
                (
                    StatusCode::PARTIAL_CONTENT,
                    [(header::CONTENT_RANGE, range)],
                    body[from..].to_vec(),
                )
                    .into_response()
            }
            None => (StatusCode::OK, body).into_response(),
        }
    }
    Router::new()
        .route("/torrent/upload", post(|| async { Json(file_stats()) }))
        .route("/torrents", post(|| async { Json(file_stats()) }))
        .route("/stream/video", get(stream))
}

#[derive(Default)]
struct Qbit {
    progress: Mutex<f32>,
    added: AtomicU32,
    deleted: Mutex<Vec<String>>,
    /// File list to report instead of the two episodes.
    files: Mutex<Option<serde_json::Value>>,
}

/// qBittorrent: login cookie, add, progress, files, delete.
fn qbittorrent(q: Arc<Qbit>) -> Router {
    qbittorrent_with_cookie(q, "SID", "s1", StatusCode::OK)
}

fn qbittorrent_with_cookie(
    q: Arc<Qbit>,
    cookie_name: &'static str,
    cookie_val: &'static str,
    login_status: StatusCode,
) -> Router {
    let expected = format!("{cookie_name}={cookie_val}");
    let set_cookie = format!("{cookie_name}={cookie_val}; HttpOnly; path=/");
    Router::new()
        .route(
            "/api/v2/auth/login",
            post(move || {
                let set_cookie = set_cookie.clone();
                async move {
                    (
                        login_status,
                        [(header::SET_COOKIE, set_cookie)],
                        if login_status == StatusCode::NO_CONTENT {
                            ""
                        } else {
                            "Ok."
                        },
                    )
                }
            }),
        )
        .route(
            "/api/v2/torrents/add",
            post({
                let expected = expected.clone();
                move |State(q): State<Arc<Qbit>>, h: HeaderMap| {
                    let expected = expected.clone();
                    async move {
                        if h.get(header::COOKIE).and_then(|c| c.to_str().ok()) != Some(expected.as_str()) {
                            return StatusCode::FORBIDDEN;
                        }
                        q.added.fetch_add(1, Ordering::SeqCst);
                        StatusCode::OK
                    }
                }
            }),
        )
        .route(
            "/api/v2/torrents/info",
            get(
                |State(q): State<Arc<Qbit>>, Query(p): Query<Vec<(String, String)>>| async move {
                    let hash = p[0].1.clone();
                    let progress = *q.progress.lock().unwrap();
                    Json(json!([{ "hash": hash, "progress": progress, "state": "downloading", "size": 2000 }]))
                },
            ),
        )
        .route(
            "/api/v2/torrents/files",
            get(|State(q): State<Arc<Qbit>>| async move {
                Json(q.files.lock().unwrap().clone().unwrap_or_else(|| {
                    json!([
                        { "index": 0, "name": EPISODES[0], "size": 1000 },
                        { "index": 1, "name": EPISODES[1], "size": 1000 }
                    ])
                }))
            }),
        )
        .route(
            "/api/v2/torrents/delete",
            post(|State(q): State<Arc<Qbit>>, body: String| async move {
                q.deleted.lock().unwrap().push(body);
                StatusCode::OK
            }),
        )
        .with_state(q)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kino-test-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[sqlx::test(migrations = "./migrations")]
async fn demo_releases_without_jackett(pool: PgPool) {
    let app = test_app(pool);
    let t = login_ok(&app, ADMIN_TG, None).await.token;
    let (code, body) = call(&app, Method::GET, "/api/releases/tv/1396?season=2", Some(&t), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    assert!(!list.configured);
    assert!(!list.releases.is_empty());

    // Nothing to play without TorrServer, and the answer says so.
    let key = list.releases[0].infohash.clone();
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/play",
        Some(&t),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "episode": 1, "release": key })),
    )
    .await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "player_unavailable");
}

#[sqlx::test(migrations = "./migrations")]
async fn search_play_and_stream(pool: PgPool) {
    let jackett = start_jackett().await;
    let ts = serve(torrserver()).await;
    let app = app(test_state_with(
        pool,
        TorrentConfig {
            jackett_url: Some(jackett),
            jackett_api_key: Some("secret".into()),
            torrserver_url: Some(ts),
            ffprobe: "kino-test-no-ffprobe".into(),
            ..Default::default()
        },
    ));
    let t = login_ok(&app, ADMIN_TG, None).await.token;

    let (code, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&t), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    assert!(list.configured);
    assert_eq!(list.releases.len(), 3);
    let lostfilm = list.releases.iter().find(|r| r.voice == "LostFilm").unwrap().clone();
    assert_eq!(
        (lostfilm.tracker.as_str(), lostfilm.season, lostfilm.seeders),
        ("RuTracker", Some(2), 214)
    );
    assert!(
        lostfilm.infohash.starts_with('u'),
        "no hash in the feed, so a placeholder key"
    );
    // 28.4 GB over 13 episodes of 47 minutes; a pack of several seasons has no estimate.
    assert_eq!((lostfilm.bitrate_kbps, lostfilm.bitrate_approx), (Some(6197), true));
    let pack = list.releases.iter().find(|r| r.season.is_none()).unwrap();
    assert_eq!(pack.bitrate_kbps, None);

    let (code, body) = call(
        &app,
        Method::POST,
        "/api/play",
        Some(&t),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "episode": 2, "release": lostfilm.infohash })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let info: PlayInfo = serde_json::from_value(body).unwrap();
    assert!(kino_core::is_infohash(&info.infohash));
    assert_eq!(info.file_index, 2);
    assert_eq!(info.file_name, "Breaking.Bad.S02E02.mp4");
    assert!(info.direct, "an .mp4 is tried as it is when ffprobe is missing");
    assert_eq!(info.next, Some((2, 3)));

    // While connecting, the player asks how far the torrent got: 8 of 32 MB is a quarter.
    let (code, body) = call(
        &app,
        Method::GET,
        &format!("/api/play/progress/{}", lostfilm.infohash),
        Some(&t),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let p: StreamProgress = serde_json::from_value(body).unwrap();
    assert_eq!(
        (p.phase, p.percent(), p.peers, p.seeders),
        (StreamPhase::Buffering, Some(25), 9, 4)
    );
    assert_eq!(p.speed_bps, 1_048_576);
    // A release key the server never heard of is simply "connecting".
    let unknown = format!("u{}", "d".repeat(39));
    let (code, body) = call(
        &app,
        Method::GET,
        &format!("/api/play/progress/{unknown}"),
        Some(&t),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["phase"], "connecting");
    let (code, _) = call(&app, Method::GET, "/api/play/progress/not-a-key", Some(&t), None).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    // The stream needs no Bearer header, keeps Range, and refuses a forged link.
    let (code, headers, bytes) = raw(&app, Method::GET, &info.stream, None, &[("Range", "bytes=9-")]).await;
    assert_eq!(code, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 9-89/90");
    assert!(bytes.starts_with(b"EPISODE-2"));
    let forged = format!("{}x", info.stream);
    assert_eq!(
        raw(&app, Method::GET, &forged, None, &[]).await.0,
        StatusCode::UNAUTHORIZED
    );

    // Next time the release is listed by its real hash, so the choice sticks.
    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&t), None).await;
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    assert!(list.releases.iter().any(|r| r.infohash == info.infohash));

    // A Jackett link that redirects to a magnet works too.
    let kinozal = list.releases.iter().find(|r| r.tracker == "Kinozal").unwrap();
    assert_eq!(kinozal.infohash, "c12fe1c06bba254a9dc9f519b335aa7c1367a88a");
}

#[sqlx::test(migrations = "./migrations")]
async fn preload_then_play_from_disk(pool: PgPool) {
    let jackett = start_jackett().await;
    let q = Arc::new(Qbit::default());
    let qb = serve(qbittorrent(q.clone())).await;
    let dir = temp_dir("preload");
    std::fs::create_dir_all(dir.join("Breaking Bad S02")).unwrap();
    std::fs::write(dir.join(EPISODES[0]), b"LOCAL-FILE-BYTES").unwrap();
    let state = test_state_with(
        pool,
        TorrentConfig {
            jackett_url: Some(jackett),
            jackett_api_key: Some("secret".into()),
            qbittorrent_url: Some(qb),
            qbittorrent_password: "pw".into(),
            preload_dir: Some(dir.clone()),
            ffprobe: "kino-test-no-ffprobe".into(),
            ..Default::default()
        },
    );
    let app = app(state.clone());
    let t = login_ok(&app, ADMIN_TG, None).await.token;
    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&t), None).await;
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    let key = list
        .releases
        .iter()
        .find(|r| r.voice == "LostFilm")
        .unwrap()
        .infohash
        .clone();

    let (code, body) = call(
        &app,
        Method::POST,
        "/api/preload",
        Some(&t),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "infohash": key })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let item: PreloadItem = serde_json::from_value(body).unwrap();
    assert!(kino_core::is_infohash(&item.infohash), "stored by the real hash");
    assert_eq!(q.added.load(Ordering::SeqCst), 1);

    *q.progress.lock().unwrap() = 0.4;
    jobs::sync_preloads(&state).await.unwrap();
    let (_, body) = call(&app, Method::GET, "/api/preload", Some(&t), None).await;
    let items: Vec<PreloadItem> = serde_json::from_value(body).unwrap();
    assert_eq!((items[0].state, items[0].progress), (PreloadState::Downloading, 0.4));

    *q.progress.lock().unwrap() = 1.0;
    jobs::sync_preloads(&state).await.unwrap();
    let (_, body) = call(&app, Method::GET, "/api/preload", Some(&t), None).await;
    let items: Vec<PreloadItem> = serde_json::from_value(body).unwrap();
    assert_eq!(items[0].state, PreloadState::Ready);
    assert_eq!(items[0].size_bytes, Some(2000));

    // Played from disk, no TorrServer configured at all.
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/play",
        Some(&t),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "episode": 1, "release": item.infohash })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let info: PlayInfo = serde_json::from_value(body).unwrap();
    assert!(info.preloaded);
    let (code, _, bytes) = raw(&app, Method::GET, &info.stream, None, &[("Range", "bytes=6-")]).await;
    assert_eq!(code, StatusCode::PARTIAL_CONTENT);
    assert_eq!(bytes, b"FILE-BYTES");

    // Removing it deletes the files in qBittorrent.
    let (code, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/preload/{}", item.id),
        Some(&t),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let deleted = q.deleted.lock().unwrap().clone();
    assert_eq!(deleted.len(), 1);
    assert!(deleted[0].contains(&item.infohash) && deleted[0].contains("deleteFiles=true"));
}

#[sqlx::test(migrations = "./migrations")]
async fn qbittorrent_v5_cookie_auth_works(pool: PgPool) {
    let jackett = start_jackett().await;
    let q = Arc::new(Qbit::default());
    let qb = serve(qbittorrent_with_cookie(
        q.clone(),
        "QBT_SID_8085",
        "v5sid",
        StatusCode::NO_CONTENT,
    ))
    .await;
    let app = app(test_state_with(
        pool,
        TorrentConfig {
            jackett_url: Some(jackett),
            jackett_api_key: Some("secret".into()),
            qbittorrent_url: Some(qb),
            ..Default::default()
        },
    ));
    let t = login_ok(&app, ADMIN_TG, None).await.token;
    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396?season=2", Some(&t), None).await;
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    let key = list.releases[0].infohash.clone();
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/preload",
        Some(&t),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "infohash": key })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(q.added.load(Ordering::SeqCst), 1);
}

/// Real ffmpeg: an MKV with MPEG-4 video, AC3 sound and SRT subtitles becomes
/// MP4 for the browser, and the subtitles WebVTT. Skipped without ffmpeg.
#[sqlx::test(migrations = "./migrations")]
async fn transcodes_with_ffmpeg(pool: PgPool) {
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg not installed, skipping");
        return;
    }
    let dir = temp_dir("ffmpeg");
    let show = dir.join("Breaking Bad S02");
    std::fs::create_dir_all(&show).unwrap();
    std::fs::write(dir.join("subs.srt"), "1\n00:00:00,500 --> 00:00:02,000\nПривет\n").unwrap();
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=3:size=160x120:rate=10",
        ])
        .args(["-f", "lavfi", "-i", "sine=duration=3", "-i"])
        .arg(dir.join("subs.srt"))
        .args([
            "-map", "0", "-map", "1", "-map", "2", "-c:v", "mpeg4", "-c:a", "ac3", "-c:s", "srt",
        ])
        .args([
            "-metadata:s:a:0",
            "language=rus",
            "-metadata:s:a:0",
            "title=LostFilm",
            "-metadata:s:s:0",
            "language=rus",
        ])
        .arg(show.join("Breaking.Bad.S02E01.mkv"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let jackett = start_jackett().await;
    let q = Arc::new(Qbit::default());
    *q.progress.lock().unwrap() = 1.0;
    *q.files.lock().unwrap() =
        Some(json!([{ "index": 0, "name": "Breaking Bad S02/Breaking.Bad.S02E01.mkv", "size": 1 }]));
    let qb = serve(qbittorrent(q.clone())).await;
    let state = test_state_with(
        pool,
        TorrentConfig {
            jackett_url: Some(jackett),
            jackett_api_key: Some("secret".into()),
            qbittorrent_url: Some(qb),
            preload_dir: Some(dir.clone()),
            ..Default::default()
        },
    );
    let app = app(state.clone());
    let t = login_ok(&app, ADMIN_TG, None).await.token;
    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&t), None).await;
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    let key = list
        .releases
        .iter()
        .find(|r| r.voice == "LostFilm")
        .unwrap()
        .infohash
        .clone();
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/preload",
        Some(&t),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "infohash": key })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    jobs::sync_preloads(&state).await.unwrap();
    call(
        &app,
        Method::PUT,
        "/api/prefs",
        Some(&t),
        Some(json!({ "subtitle_lang": "Русский" })),
    )
    .await;

    let (code, body) = call(
        &app,
        Method::POST,
        "/api/play",
        Some(&t),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "episode": 1, "release": key })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let info: PlayInfo = serde_json::from_value(body).unwrap();
    assert!(!info.direct, "MPEG-4 + AC3 in MKV needs re-encoding");
    assert!(!info.native);
    assert!((info.duration.unwrap() - 3.0).abs() < 0.2);
    assert_eq!(info.audio.len(), 1);
    assert_eq!(info.audio[0].label(), "LostFilm · Русский");
    assert_eq!(info.subtitles.len(), 1);
    assert_eq!(info.subtitle_track, None, "subtitles are never auto-enabled from prefs");

    // A native client (Android ExoPlayer / Desktop libmpv) gets direct playback for the same MKV+AC3 file.
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/play",
        Some(&t),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "episode": 1, "release": key, "native": true })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let native_info: PlayInfo = serde_json::from_value(body).unwrap();
    assert!(native_info.direct);
    assert!(native_info.native);
    assert!(native_info.plays_directly(Some(0)));

    let (code, headers, bytes) = raw(&app, Method::GET, &info.transcoded_url(Some(0), 1.0), None, &[]).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "video/mp4");
    assert_eq!(&bytes[4..8], b"ftyp", "an MP4 the browser can read");
    assert!(bytes.len() > 1000);

    let (code, _, vtt) = raw(&app, Method::GET, &info.subtitle_url(0, 0.0), None, &[]).await;
    assert_eq!(code, StatusCode::OK);
    let vtt = String::from_utf8(vtt).unwrap();
    assert!(vtt.starts_with("WEBVTT") && vtt.contains("Привет"), "{vtt}");
}

#[sqlx::test(migrations = "./migrations")]
async fn release_search_is_started_early_and_remembered(pool: PgPool) {
    let hits = Arc::new(AtomicU32::new(0));
    let jackett = start_jackett_counted(hits.clone()).await;
    let app = app(test_state_with(
        pool,
        TorrentConfig {
            jackett_url: Some(jackett),
            jackett_api_key: Some("secret".into()),
            ..Default::default()
        },
    ));
    let t = login_ok(&app, ADMIN_TG, None).await.token;

    // Opening the title page starts the search by itself (a Russian and an original-name query).
    let (code, _) = call(&app, Method::GET, "/api/catalog/tv/1396", Some(&t), None).await;
    assert_eq!(code, StatusCode::OK);
    for _ in 0..50 {
        if hits.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    // The list is then served from memory, however often it is asked for.
    for season in 1..=3 {
        let (code, body) = call(
            &app,
            Method::GET,
            &format!("/api/releases/tv/1396?season={season}"),
            Some(&t),
            None,
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{body}");
        let list: ReleaseList = serde_json::from_value(body).unwrap();
        assert_eq!(list.releases.len(), 3);
    }
    call(&app, Method::GET, "/api/catalog/tv/1396", Some(&t), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 2, "no second search for the same title");

    // Another title is searched on its own.
    call(&app, Method::GET, "/api/releases/movie/603", Some(&t), None).await;
    assert_eq!(hits.load(Ordering::SeqCst), 4);
}

#[sqlx::test(migrations = "./migrations")]
async fn old_releases_are_forgotten_unless_in_use(pool: PgPool) {
    let state = test_state(pool.clone());
    let (free, chosen, fresh) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
    for key in [&free, &chosen, &fresh] {
        sqlx::query("INSERT INTO torrent_source (key, title, infohash, torrent) VALUES ($1, 't', $1, '\\x00')")
            .bind(key)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE torrent_source SET updated_at = now() - interval '40 days' WHERE key <> $1")
        .bind(&fresh)
        .execute(&pool)
        .await
        .unwrap();
    let app = app(state.clone());
    let t = login_ok(&app, ADMIN_TG, None).await.token;
    let (code, _) = call(
        &app,
        Method::PUT,
        "/api/release-choice",
        Some(&t),
        Some(json!({ "kind": "movie", "tmdb_id": 603, "infohash": chosen })),
    )
    .await;
    assert_eq!(code, StatusCode::OK);

    assert_eq!(jobs::prune_sources(&state).await.unwrap(), 1);
    let left: Vec<String> = sqlx::query_scalar("SELECT key FROM torrent_source ORDER BY key")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(left, [chosen, fresh]);
}

#[sqlx::test(migrations = "./migrations")]
async fn resolve_release_auto_picks_quality_and_shares_between_users(pool: PgPool) {
    let jackett = start_jackett().await;
    let app = app(test_state_with(
        pool,
        TorrentConfig {
            jackett_url: Some(jackett),
            jackett_api_key: Some("secret".into()),
            ..TorrentConfig::default()
        },
    ));
    let admin = login_ok(&app, ADMIN_TG, None).await;
    let (_, inv) = call(&app, Method::POST, "/api/invites", Some(&admin.token), None).await;
    let peer = login_ok(&app, 2003, inv["code"].as_str()).await;

    // Mobile auto: torznab.xml has 720p with 12 seeders (>= 7) -> picks 720p automatically.
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/releases/tv/1396/resolve",
        Some(&peer.token),
        Some(json!({ "season": 2, "episode": 1, "is_mobile": true })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let resolved: ResolveReleaseResponse = serde_json::from_value(body).unwrap();
    match resolved {
        ResolveReleaseResponse::Ready { choice, from_peer } => {
            assert_eq!(choice.quality_key.as_deref(), Some("720p"));
            assert_eq!(from_peer, None);
        }
        other => panic!("expected Ready 720p, got {other:?}"),
    }

    // Desktop default: 1080p -> picks 1080p automatically for Admin.
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/releases/tv/1396/resolve",
        Some(&admin.token),
        Some(json!({ "season": 2, "episode": 1, "is_mobile": false })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    match serde_json::from_value::<ResolveReleaseResponse>(body).unwrap() {
        ResolveReleaseResponse::Ready { choice, .. } => {
            assert_eq!(choice.quality_key.as_deref(), Some("1080p"));
        }
        other => panic!("expected Ready 1080p, got {other:?}"),
    }

    // Peer sharing: User 3 asking for 1080p adopts Admin's choice without calling Jackett or picking manually.
    let (_, inv2) = call(&app, Method::POST, "/api/invites", Some(&admin.token), None).await;
    let peer2 = login_ok(&app, 2004, inv2["code"].as_str()).await;
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/releases/tv/1396/resolve",
        Some(&peer2.token),
        Some(json!({ "season": 2, "episode": 1, "is_mobile": false })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    match serde_json::from_value::<ResolveReleaseResponse>(body).unwrap() {
        ResolveReleaseResponse::Ready { choice, from_peer } => {
            assert_eq!(choice.quality_key.as_deref(), Some("1080p"));
            assert!(from_peer.is_some());
        }
        other => panic!("expected Ready from peer, got {other:?}"),
    }

    // Missing quality (SD is not in torznab.xml): falls back to NeedsPicker.
    let prefs = UserPrefs {
        desktop_quality: Some("SD".into()),
        ..UserPrefs::default()
    };
    let (code, _) = call(
        &app,
        Method::PUT,
        "/api/prefs",
        Some(&peer2.token),
        Some(serde_json::to_value(prefs).unwrap()),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/releases/tv/1396/resolve",
        Some(&peer2.token),
        Some(json!({ "season": 1, "episode": 1, "is_mobile": false })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert!(matches!(
        serde_json::from_value::<ResolveReleaseResponse>(body).unwrap(),
        ResolveReleaseResponse::NeedsPicker { .. }
    ));
}
