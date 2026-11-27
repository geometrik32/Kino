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
    ApiError, PlayInfo, PreloadItem, PreloadState, ReleaseChoice, ReleaseList, ResolveReleaseResponse, StreamPhase,
    StreamProgress, UserPrefs,
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
    file_progress: Mutex<std::collections::HashMap<i32, f32>>,
    file_prios: Mutex<std::collections::HashMap<i32, i32>>,
    prio_calls: Mutex<Vec<(i32, i32)>>,
    started: Mutex<Vec<String>>,
    added: AtomicU32,
    deleted: Mutex<Vec<String>>,
    /// File list to report instead of the two episodes.
    files: Mutex<Option<serde_json::Value>>,
}

/// qBittorrent: login cookie, add, progress, files, filePrio, start, delete.
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
                    if q.added.load(Ordering::SeqCst) == 0 {
                        return Json(json!([]));
                    }
                    let hash = p[0].1.clone();
                    let progress = *q.progress.lock().unwrap();
                    Json(json!([{ "hash": hash, "progress": progress, "state": "downloading", "size": 2000 }]))
                },
            ),
        )
        .route(
            "/api/v2/torrents/files",
            get(|State(q): State<Arc<Qbit>>| async move {
                if let Some(custom) = q.files.lock().unwrap().clone() {
                    return Json(custom);
                }
                let default_p = *q.progress.lock().unwrap();
                let fp = q.file_progress.lock().unwrap().clone();
                let prios = q.file_prios.lock().unwrap().clone();
                Json(json!([
                    {
                        "index": 0,
                        "name": EPISODES[0],
                        "size": 1000,
                        "progress": fp.get(&0).copied().unwrap_or(default_p),
                        "priority": prios.get(&0).copied().unwrap_or(1)
                    },
                    {
                        "index": 1,
                        "name": EPISODES[1],
                        "size": 1000,
                        "progress": fp.get(&1).copied().unwrap_or(default_p),
                        "priority": prios.get(&1).copied().unwrap_or(1)
                    }
                ]))
            }),
        )
        .route(
            "/api/v2/torrents/filePrio",
            post(|State(q): State<Arc<Qbit>>, body: String| async move {
                let mut ids_str = String::new();
                let mut prio = 1i32;
                for (k, v) in url::form_urlencoded::parse(body.as_bytes()) {
                    if k == "id" {
                        ids_str = v.into_owned();
                    } else if k == "priority" {
                        prio = v.parse().unwrap_or(1);
                    }
                }
                let mut prios = q.file_prios.lock().unwrap();
                let mut calls = q.prio_calls.lock().unwrap();
                for part in ids_str.split('|').filter(|s| !s.is_empty()) {
                    if let Ok(idx) = part.parse::<i32>() {
                        prios.insert(idx, prio);
                        calls.push((idx, prio));
                    }
                }
                StatusCode::OK
            }),
        )
        .route(
            "/api/v2/torrents/start",
            post(|State(q): State<Arc<Qbit>>, body: String| async move {
                q.started.lock().unwrap().push(body);
                StatusCode::OK
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
    assert!(
        items[0].delete_after.is_some(),
        "ready preloads get a max-retention deadline automatically"
    );

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

    // Default Russian language skips the English 720p release -> NeedsPicker when no Russian 720p/SD exists.
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/releases/tv/1396/resolve",
        Some(&peer.token),
        Some(json!({ "season": 2, "episode": 1, "is_mobile": true })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert!(matches!(
        serde_json::from_value::<ResolveReleaseResponse>(body).unwrap(),
        ResolveReleaseResponse::NeedsPicker { .. }
    ));

    // When peer allows any language, mobile auto sees 720p with 12 seeders (>= 7) -> picks 720p automatically
    // without saving a manual override in `release_choice`.
    let (code, _) = call(
        &app,
        Method::PUT,
        "/api/prefs",
        Some(&peer.token),
        Some(serde_json::to_value(UserPrefs::default()).unwrap()),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
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
    let (_, choices) = call(
        &app,
        Method::GET,
        "/api/release-choice/tv/1396",
        Some(&peer.token),
        None,
    )
    .await;
    let choices: Vec<ReleaseChoice> = serde_json::from_value(choices).unwrap();
    assert!(choices.is_empty(), "auto-pick must not create manual release choices");

    // Desktop default: 1080p -> picks 1080p automatically for Admin (also not manual).
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/releases/tv/1396/resolve",
        Some(&admin.token),
        Some(json!({ "season": 2, "episode": 1, "is_mobile": false })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let admin_pick = match serde_json::from_value::<ResolveReleaseResponse>(body).unwrap() {
        ResolveReleaseResponse::Ready { choice, from_peer } => {
            assert_eq!(choice.quality_key.as_deref(), Some("1080p"));
            assert_eq!(from_peer, None);
            choice
        }
        other => panic!("expected Ready 1080p, got {other:?}"),
    };
    let (_, admin_choices) = call(
        &app,
        Method::GET,
        "/api/release-choice/tv/1396",
        Some(&admin.token),
        None,
    )
    .await;
    let admin_choices: Vec<ReleaseChoice> = serde_json::from_value(admin_choices).unwrap();
    assert!(
        admin_choices.is_empty(),
        "auto-pick must not create manual release choices"
    );

    // Only explicit manual picks (via PUT /api/release-choice) appear in `picked_by`
    // and are shared across peers as Tier 3 recommendations.
    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&admin.token), None).await;
    let before_list: ReleaseList = serde_json::from_value(body).unwrap();
    assert!(before_list.releases.iter().all(|r| r.picked_by.is_empty()));

    let (code, _) = call(
        &app,
        Method::PUT,
        "/api/release-choice",
        Some(&admin.token),
        Some(json!({
            "kind": "tv",
            "tmdb_id": 1396,
            "season": 2,
            "episode": 1,
            "infohash": admin_pick.infohash,
            "quality_key": "1080p",
            "title": admin_pick.title,
        })),
    )
    .await;
    assert_eq!(code, StatusCode::OK);

    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&admin.token), None).await;
    let after_list: ReleaseList = serde_json::from_value(body).unwrap();
    let picked = after_list
        .releases
        .iter()
        .find(|r| r.infohash == admin_pick.infohash)
        .unwrap();
    assert_eq!(picked.picked_by.len(), 1);

    // Peer sharing: User 3 asking for 1080p adopts Admin's manual choice without calling Jackett.
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

#[sqlx::test(migrations = "./migrations")]
async fn ready_preload_wins_tier1_unless_manual_override_is_newer(pool: PgPool) {
    let jackett = start_jackett().await;
    let q = Arc::new(Qbit::default());
    *q.progress.lock().unwrap() = 1.0;
    let qb = serve(qbittorrent(q)).await;
    let dir = temp_dir("tier1");
    std::fs::create_dir_all(dir.join("Breaking Bad S02")).unwrap();
    std::fs::write(dir.join(EPISODES[0]), b"PRELOADED-EP1").unwrap();
    let app = app(test_state_with(
        pool,
        TorrentConfig {
            jackett_url: Some(jackett),
            jackett_api_key: Some("secret".into()),
            qbittorrent_url: Some(qb),
            preload_dir: Some(dir),
            ffprobe: "kino-test-no-ffprobe".into(),
            ..Default::default()
        },
    ));
    let admin = login_ok(&app, ADMIN_TG, None).await;
    let (_, inv) = call(&app, Method::POST, "/api/invites", Some(&admin.token), None).await;
    let peer = login_ok(&app, 2005, inv["code"].as_str()).await;

    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&admin.token), None).await;
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    let lostfilm = list.releases.iter().find(|r| r.voice == "LostFilm").unwrap();
    let kinozal = list.releases.iter().find(|r| r.tracker == "Kinozal").unwrap();

    // Peer saves an earlier manual 2160p choice before the preload finishes.
    let (code, _) = call(
        &app,
        Method::PUT,
        "/api/release-choice",
        Some(&peer.token),
        Some(json!({
            "kind": "tv",
            "tmdb_id": 1396,
            "season": 2,
            "episode": 1,
            "infohash": kinozal.infohash,
            "quality_key": "2160p",
            "title": kinozal.title,
        })),
    )
    .await;
    assert_eq!(code, StatusCode::OK);

    // Admin preloads the 1080p LostFilm release; GET /api/preload syncs it to Ready immediately.
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/preload",
        Some(&admin.token),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "infohash": lostfilm.infohash })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let preload: PreloadItem = serde_json::from_value(body).unwrap();
    let (_, body) = call(&app, Method::GET, "/api/preload", Some(&admin.token), None).await;
    let preloads: Vec<PreloadItem> = serde_json::from_value(body).unwrap();
    assert_eq!(preloads[0].state, PreloadState::Ready);

    // Release listing marks the preloaded release.
    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&peer.token), None).await;
    let rel_list: ReleaseList = serde_json::from_value(body).unwrap();
    assert!(rel_list
        .releases
        .iter()
        .any(|r| r.infohash == preload.infohash && r.preloaded));

    // Tier 1: Ready preload beats Peer's older manual choice.
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/releases/tv/1396/resolve",
        Some(&peer.token),
        Some(json!({ "season": 2, "episode": 1, "is_mobile": false })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    match serde_json::from_value::<ResolveReleaseResponse>(body).unwrap() {
        ResolveReleaseResponse::Ready { choice, from_peer } => {
            assert_eq!(choice.infohash, preload.infohash);
            assert_eq!(from_peer, None);
        }
        other => panic!("expected Tier 1 preload, got {other:?}"),
    }

    // Peer saves a newer manual override after ready_at -> Peer now gets their manual choice.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let (code, _) = call(
        &app,
        Method::PUT,
        "/api/release-choice",
        Some(&peer.token),
        Some(json!({
            "kind": "tv",
            "tmdb_id": 1396,
            "season": 2,
            "episode": 1,
            "infohash": kinozal.infohash,
            "quality_key": "2160p",
            "title": kinozal.title,
        })),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/releases/tv/1396/resolve",
        Some(&peer.token),
        Some(json!({ "season": 2, "episode": 1, "is_mobile": false })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    match serde_json::from_value::<ResolveReleaseResponse>(body).unwrap() {
        ResolveReleaseResponse::Ready { choice, .. } => {
            assert_eq!(choice.infohash, kinozal.infohash);
        }
        other => panic!("expected newer manual override, got {other:?}"),
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn missing_episode_reports_available_episodes(pool: PgPool) {
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
    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&t), None).await;
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    let lostfilm = list.releases.iter().find(|r| r.voice == "LostFilm").unwrap();

    // Fixture has only S02E01 and S02E02 -> requesting S02E05 reports available episodes.
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/play",
        Some(&t),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "episode": 5, "release": lostfilm.infohash })),
    )
    .await;
    assert!(code.is_client_error(), "expected 4xx, got {code}: {body}");
    let err: ApiError = serde_json::from_value(body).unwrap();
    assert_eq!(err.code, "no_file");
    assert!(
        err.available_episodes.iter().any(|(s, e)| *s == 2 && *e == 2),
        "expected S02E02 in available_episodes, got {:?}",
        err.available_episodes
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn extra_torznab_is_primary_keeps_jackett_asleep_and_falls_back_on_failure(pool: PgPool) {
    let jackett_hits = Arc::new(AtomicU32::new(0));
    let jackett = start_jackett_counted(jackett_hits.clone()).await;
    let sablier_hits = Arc::new(AtomicU32::new(0));
    let sablier_url = serve(Router::new().route(
        "/api/strategies/blocking",
        get({
            let hits = sablier_hits.clone();
            move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }
        }),
    ))
    .await;

    let extra_feed = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel>
  <item>
    <title>Во все тяжкие / Breaking Bad S02 (2009) WEB-DL 1080p Дубляж</title>
    <jackettindexer id="all">rutracker, bitru, rutor</jackettindexer>
    <link>magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</link>
    <category>5000</category>
    <size>24000000000</size>
    <torznab:attr name="seeders" value="320" />
    <torznab:attr name="infohash" value="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" />
  </item>
  <item>
    <title>Во все тяжкие / Breaking Bad (Сезон 2) (2009) BDRip 1080p MVO</title>
    <jackettindexer id="all">BigFANGroup</jackettindexer>
    <link>magnet:?xt=urn:btih:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb</link>
    <category>5000</category>
    <size>18400000000</size>
    <torznab:attr name="seeders" value="7944" />
    <torznab:attr name="infohash" value="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" />
  </item>
  <item>
    <title>Во все тяжкие / Breaking Bad (Сезон 2) (2009) BDRip 1080p MVO</title>
    <jackettindexer id="all">kinozal, rutor</jackettindexer>
    <link>magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a</link>
    <category>5000</category>
    <size>18400000000</size>
    <torznab:attr name="seeders" value="150" />
    <torznab:attr name="infohash" value="c12fe1c06bba254a9dc9f519b335aa7c1367a88a" />
  </item>
</channel></rss>"#;

    let extra_url = serve(Router::new().route(
        "/api/v2.0/indexers/all/results/torznab/api",
        get(move |Query(q): Query<Vec<(String, String)>>| async move {
            assert!(!q.iter().any(|(k, _)| k == "apikey"));
            if q.iter().any(|(k, v)| k == "q" && v.contains("Матрица"))
                || q.iter().any(|(k, v)| k == "q" && v.contains("Matrix"))
            {
                r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0"><channel></channel></rss>"#
            } else {
                extra_feed
            }
        }),
    ))
    .await;

    let broken_url = serve(Router::new().route(
        "/api/v2.0/indexers/all/results/torznab/api",
        get(|| async { StatusCode::BAD_GATEWAY }),
    ))
    .await;

    let app_primary = app(test_state_with(
        pool.clone(),
        TorrentConfig {
            jackett_url: Some(jackett.clone()),
            jackett_api_key: Some("secret".into()),
            jackett_wake_url: Some(sablier_url.clone()),
            jackett_wake_group: "jackett".into(),
            jackett_wake_duration: "10m".into(),
            extra_torznab: vec![kino_server::config::ExtraTorznabConfig {
                url: extra_url.clone(),
                api_key: None,
            }],
            ..Default::default()
        },
    ));
    let t = login_ok(&app_primary, ADMIN_TG, None).await.token;

    // Browsing home/search keeps Jackett asleep while primary extra_torznab is active.
    let (code, _) = call(&app_primary, Method::GET, "/api/catalog/home", Some(&t), None).await;
    assert_eq!(code, StatusCode::OK);
    let (code, _) = call(
        &app_primary,
        Method::GET,
        "/api/catalog/search?q=matrix",
        Some(&t),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(sablier_hits.load(Ordering::SeqCst), 0);

    // Primary returns 2 valid releases (BigFANGroup dropped) and never wakes/queries Jackett.
    let (code, body) = call(&app_primary, Method::GET, "/api/releases/tv/1396", Some(&t), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    assert!(list.configured);
    assert_eq!(list.releases.len(), 2);
    assert!(!list
        .releases
        .iter()
        .any(|r| r.tracker.eq_ignore_ascii_case("bigfangroup")));
    assert_eq!(jackett_hits.load(Ordering::SeqCst), 0);
    assert_eq!(sablier_hits.load(Ordering::SeqCst), 0);

    // Even when primary responds cleanly with 0 releases, Jackett must stay asleep.
    let (code, body) = call(&app_primary, Method::GET, "/api/releases/movie/603", Some(&t), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let empty_list: ReleaseList = serde_json::from_value(body).unwrap();
    assert!(empty_list.releases.is_empty());
    assert_eq!(jackett_hits.load(Ordering::SeqCst), 0);
    assert_eq!(sablier_hits.load(Ordering::SeqCst), 0);

    // Partial failure across extra_torznab endpoints still uses the healthy one without waking Jackett.
    let app_partial = app(test_state_with(
        pool.clone(),
        TorrentConfig {
            jackett_url: Some(jackett.clone()),
            jackett_api_key: Some("secret".into()),
            jackett_wake_url: Some(sablier_url.clone()),
            jackett_wake_group: "jackett".into(),
            jackett_wake_duration: "10m".into(),
            extra_torznab: vec![
                kino_server::config::ExtraTorznabConfig {
                    url: extra_url,
                    api_key: None,
                },
                kino_server::config::ExtraTorznabConfig {
                    url: broken_url.clone(),
                    api_key: None,
                },
            ],
            ..Default::default()
        },
    ));
    let t_part = login_ok(&app_partial, ADMIN_TG, None).await.token;
    let (code, body) = call(&app_partial, Method::GET, "/api/releases/tv/1396", Some(&t_part), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let part_list: ReleaseList = serde_json::from_value(body).unwrap();
    assert_eq!(part_list.releases.len(), 2);
    assert_eq!(jackett_hits.load(Ordering::SeqCst), 0);
    assert_eq!(sablier_hits.load(Ordering::SeqCst), 0);

    // When all primary endpoints fail to respond, server wakes Jackett and falls back to it.
    let app_fallback = app(test_state_with(
        pool.clone(),
        TorrentConfig {
            jackett_url: Some(jackett.clone()),
            jackett_api_key: Some("secret".into()),
            jackett_wake_url: Some(sablier_url.clone()),
            jackett_wake_group: "jackett".into(),
            jackett_wake_duration: "10m".into(),
            extra_torznab: vec![kino_server::config::ExtraTorznabConfig {
                url: broken_url,
                api_key: None,
            }],
            ..Default::default()
        },
    ));
    let t_fb = login_ok(&app_fallback, ADMIN_TG, None).await.token;
    let (code, body) = call(&app_fallback, Method::GET, "/api/releases/tv/1396", Some(&t_fb), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let fb_list: ReleaseList = serde_json::from_value(body).unwrap();
    assert_eq!(fb_list.releases.len(), 3);
    assert!(jackett_hits.load(Ordering::SeqCst) >= 2);
    assert!(sablier_hits.load(Ordering::SeqCst) >= 1);

    // When `jackett_parallel` is true, primary and Jackett are queried together and merged.
    let par_jackett_hits = Arc::new(AtomicU32::new(0));
    let par_jackett = start_jackett_counted(par_jackett_hits.clone()).await;
    let par_extra = serve(Router::new().route(
        "/api/v2.0/indexers/all/results/torznab/api",
        get(move || async move { extra_feed }),
    ))
    .await;
    let app_parallel = app(test_state_with(
        pool,
        TorrentConfig {
            jackett_url: Some(par_jackett),
            jackett_api_key: Some("secret".into()),
            jackett_wake_url: Some(sablier_url),
            jackett_wake_group: "jackett".into(),
            jackett_wake_duration: "10m".into(),
            extra_torznab: vec![kino_server::config::ExtraTorznabConfig {
                url: par_extra,
                api_key: None,
            }],
            jackett_parallel: true,
            ..Default::default()
        },
    ));
    let t_par = login_ok(&app_parallel, ADMIN_TG, None).await.token;
    let (code, body) = call(&app_parallel, Method::GET, "/api/releases/tv/1396", Some(&t_par), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let par_list: ReleaseList = serde_json::from_value(body).unwrap();
    assert_eq!(par_list.releases.len(), 4);
    assert!(par_jackett_hits.load(Ordering::SeqCst) >= 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn admin_search_sources_toggle_updates_runtime_and_db(pool: PgPool) {
    let jackett_hits = Arc::new(AtomicU32::new(0));
    let jackett = start_jackett_counted(jackett_hits.clone()).await;
    let extra_feed = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel>
  <item>
    <title>Во все тяжкие / Breaking Bad S02 (2009) WEB-DL 1080p Дубляж</title>
    <jackettindexer id="all">rutracker, bitru, rutor</jackettindexer>
    <link>magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</link>
    <category>5000</category>
    <size>24000000000</size>
    <torznab:attr name="seeders" value="320" />
    <torznab:attr name="infohash" value="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" />
  </item>
</channel></rss>"#;
    let extra_url = serve(Router::new().route(
        "/api/v2.0/indexers/all/results/torznab/api",
        get(move || async move { extra_feed }),
    ))
    .await;
    let state = test_state_with(
        pool.clone(),
        TorrentConfig {
            jackett_url: Some(jackett),
            jackett_api_key: Some("secret".into()),
            extra_torznab: vec![kino_server::config::ExtraTorznabConfig {
                url: extra_url,
                api_key: None,
            }],
            ..Default::default()
        },
    );
    let app_srv = app(state.clone());

    let admin_t = login_ok(&app_srv, ADMIN_TG, None).await.token;
    let (code, body) = call(&app_srv, Method::POST, "/api/invites", Some(&admin_t), None).await;
    assert_eq!(code, StatusCode::OK);
    let invite = body["code"].as_str().unwrap().to_owned();
    let user_t = login_ok(&app_srv, 2002, Some(&invite)).await.token;

    // Non-admin user cannot read or toggle server search sources.
    let (code, _) = call(&app_srv, Method::GET, "/api/admin/search-sources", Some(&user_t), None).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let (code, _) = call(
        &app_srv,
        Method::PUT,
        "/api/admin/search-sources",
        Some(&user_t),
        Some(json!({ "jackett_parallel": true })),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN);

    // Initially off -> primary only (1 release, 0 Jackett hits).
    let (code, body) = call(&app_srv, Method::GET, "/api/admin/search-sources", Some(&admin_t), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "available": true, "jackett_parallel": false }));

    let (code, body) = call(&app_srv, Method::GET, "/api/releases/tv/1396", Some(&admin_t), None).await;
    assert_eq!(code, StatusCode::OK);
    let list_off: ReleaseList = serde_json::from_value(body).unwrap();
    assert_eq!(list_off.releases.len(), 1);
    assert_eq!(jackett_hits.load(Ordering::SeqCst), 0);

    // Admin flips toggle ON -> clears cache, queries Jackett in parallel, and persists in DB.
    let (code, body) = call(
        &app_srv,
        Method::PUT,
        "/api/admin/search-sources",
        Some(&admin_t),
        Some(json!({ "jackett_parallel": true })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "available": true, "jackett_parallel": true }));
    assert!(state.media.is_jackett_parallel());

    let (code, body) = call(&app_srv, Method::GET, "/api/releases/tv/1396", Some(&admin_t), None).await;
    assert_eq!(code, StatusCode::OK);
    let list_on: ReleaseList = serde_json::from_value(body).unwrap();
    assert_eq!(list_on.releases.len(), 4);
    assert!(jackett_hits.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        kino_server::db::get_bool_setting(&pool, "jackett_parallel")
            .await
            .unwrap(),
        Some(true)
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn preload_ready_sends_telegram_notification_once(pool: PgPool) {
    let sent: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let mock_tg = Router::new().route(
        &format!("/bot{BOT_TOKEN}/sendMessage"),
        post({
            let sent = sent.clone();
            move |Json(payload): Json<serde_json::Value>| {
                let sent = sent.clone();
                async move {
                    sent.lock().unwrap().push(payload);
                    Json(json!({ "ok": true, "result": { "message_id": 1 } }))
                }
            }
        }),
    );
    let mock_tg_url = serve(mock_tg).await;
    let jackett = start_jackett().await;
    let q = Arc::new(Qbit::default());
    let qb = serve(qbittorrent(q.clone())).await;
    let mut state = test_state_with(
        pool,
        TorrentConfig {
            jackett_url: Some(jackett),
            jackett_api_key: Some("secret".into()),
            qbittorrent_url: Some(qb),
            ..Default::default()
        },
    );
    let mut cfg = (*state.config).clone();
    cfg.telegram_api_url = Some(mock_tg_url);
    state.config = Arc::new(cfg);
    let app = app(state.clone());

    let admin = login_ok(&app, ADMIN_TG, None).await;
    let (_, inv) = call(&app, Method::POST, "/api/invites", Some(&admin.token), None).await;
    let peer = login_ok(&app, 2006, inv["code"].as_str()).await;

    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&admin.token), None).await;
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    let lostfilm = list.releases.iter().find(|r| r.voice == "LostFilm").unwrap();

    for tok in [&admin.token, &peer.token] {
        let (code, body) = call(
            &app,
            Method::POST,
            "/api/preload",
            Some(tok),
            Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "infohash": lostfilm.infohash })),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{body}");
    }
    let (_, preloads_list) = call(&app, Method::GET, "/api/preload", Some(&admin.token), None).await;
    let real_hash = preloads_list[0]["infohash"].as_str().unwrap().to_owned();

    // Still downloading: no notification sent yet.
    *q.progress.lock().unwrap() = 0.45;
    jobs::sync_preloads(&state).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert!(
        sent.lock().unwrap().is_empty(),
        "must not notify before download finishes"
    );

    // Once complete, each requesting user gets one Telegram message.
    *q.progress.lock().unwrap() = 1.0;
    jobs::sync_preloads(&state).await.unwrap();
    for _ in 0..40 {
        if sent.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    // Repeated syncs and duplicate preload requests must not duplicate notifications.
    jobs::sync_preloads(&state).await.unwrap();
    let (code, _) = call(
        &app,
        Method::POST,
        "/api/preload",
        Some(&admin.token),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "infohash": lostfilm.infohash })),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    let msgs = sent.lock().unwrap().clone();
    assert_eq!(msgs.len(), 2, "expected one notification per requesting user: {msgs:?}");
    let mut chats: Vec<i64> = msgs.iter().filter_map(|m| m["chat_id"].as_i64()).collect();
    chats.sort_unstable();
    assert_eq!(chats, vec![ADMIN_TG, 2006]);
    let expected_url = format!("https://kino.example.com/?open=tv_1396&play=1&season=2&release={real_hash}");
    for m in &msgs {
        assert_eq!(m["parse_mode"], "HTML");
        let text = m["text"].as_str().unwrap();
        assert!(text.contains("Сезон 2 сериала «Во все тяжкие» скачался"), "{text}");
        assert!(text.contains("1080p") && text.contains("LostFilm"), "{text}");
        let btn = &m["reply_markup"]["inline_keyboard"][0][0];
        assert_eq!(btn["text"], "▶ Смотреть");
        assert_eq!(btn["web_app"]["url"], expected_url);
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn single_episode_preload_priorities_playback_and_resolve(pool: PgPool) {
    let jackett = start_jackett().await;
    let q = Arc::new(Qbit::default());
    let qb = serve(qbittorrent(q.clone())).await;
    let dir = temp_dir("single-ep-preload");
    std::fs::create_dir_all(dir.join("Breaking Bad S02")).unwrap();
    std::fs::write(dir.join(EPISODES[1]), b"EP2-LOCAL-BYTES").unwrap();

    let state = test_state_with(
        pool,
        TorrentConfig {
            jackett_url: Some(jackett),
            jackett_api_key: Some("secret".into()),
            qbittorrent_url: Some(qb),
            qbittorrent_password: "pw".into(),
            preload_dir: Some(dir),
            ffprobe: "kino-test-no-ffprobe".into(),
            ..Default::default()
        },
    );
    let app = app(state.clone());
    let admin = login_ok(&app, ADMIN_TG, None).await;
    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&admin.token), None).await;
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    let lostfilm = list.releases.iter().find(|r| r.voice == "LostFilm").unwrap();

    // Download only Episode 2 from a 2-episode season pack.
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/preload",
        Some(&admin.token),
        Some(json!({
            "kind": "tv",
            "tmdb_id": 1396,
            "season": 2,
            "episode": 2,
            "infohash": lostfilm.infohash,
        })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let item: PreloadItem = serde_json::from_value(body).unwrap();
    assert!(!item.whole_torrent);
    assert_eq!(item.episodes.len(), 1);
    assert_eq!((item.episodes[0].season, item.episodes[0].episode), (2, 2));
    assert!(
        !q.started.lock().unwrap().is_empty(),
        "torrent started after priority setup"
    );
    assert!(
        q.prio_calls.lock().unwrap().contains(&(0, 0)),
        "unwanted E01 must be set to priority=0"
    );

    // Only E02 completes downloading on disk; E01 remains at 0%.
    q.file_progress.lock().unwrap().insert(1, 1.0);
    jobs::sync_preloads(&state).await.unwrap();
    let (_, body) = call(&app, Method::GET, "/api/preload", Some(&admin.token), None).await;
    let preloads: Vec<PreloadItem> = serde_json::from_value(body).unwrap();
    assert_eq!(preloads.len(), 1);
    assert!(preloads[0].is_ready_for(kino_core::MediaKind::Tv, 2, 2));
    assert!(!preloads[0].is_ready_for(kino_core::MediaKind::Tv, 2, 1));

    // Resolve E02 hits Tier 1 disk preload; resolve E01 must NOT claim E02's partial preload.
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/releases/tv/1396/resolve",
        Some(&admin.token),
        Some(json!({ "season": 2, "episode": 2, "is_mobile": false })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    match serde_json::from_value::<ResolveReleaseResponse>(body).unwrap() {
        ResolveReleaseResponse::Ready { choice, .. } => assert_eq!(choice.infohash, item.infohash),
        other => panic!("expected E02 ready preload, got {other:?}"),
    }

    // Play E02 streams from disk (`preloaded: true`), while E01 is not falsely treated as preloaded.
    let (code, body) = call(
        &app,
        Method::POST,
        "/api/play",
        Some(&admin.token),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "episode": 2, "release": item.infohash })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let info2: PlayInfo = serde_json::from_value(body).unwrap();
    assert!(info2.preloaded);

    let (code, _) = call(
        &app,
        Method::POST,
        "/api/play",
        Some(&admin.token),
        Some(json!({ "kind": "tv", "tmdb_id": 1396, "season": 2, "episode": 1, "release": item.infohash })),
    )
    .await;
    assert_ne!(
        code,
        StatusCode::OK,
        "un-downloaded E01 must not play from disk without TorrServer"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn all_server_preloads_visibility_and_single_episode_deletion(pool: PgPool) {
    let jackett = start_jackett().await;
    let q = Arc::new(Qbit::default());
    let qb = serve(qbittorrent(q.clone())).await;
    let state = test_state_with(
        pool,
        TorrentConfig {
            jackett_url: Some(jackett),
            jackett_api_key: Some("secret".into()),
            qbittorrent_url: Some(qb),
            ..Default::default()
        },
    );
    let app = app(state.clone());

    let admin = login_ok(&app, ADMIN_TG, None).await;
    let (_, inv1) = call(&app, Method::POST, "/api/invites", Some(&admin.token), None).await;
    let peer1 = login_ok(&app, 3001, inv1["code"].as_str()).await;
    let (_, inv2) = call(&app, Method::POST, "/api/invites", Some(&admin.token), None).await;
    let peer2 = login_ok(&app, 3002, inv2["code"].as_str()).await;

    let (_, body) = call(&app, Method::GET, "/api/releases/tv/1396", Some(&peer1.token), None).await;
    let list: ReleaseList = serde_json::from_value(body).unwrap();
    let lostfilm = list.releases.iter().find(|r| r.voice == "LostFilm").unwrap();

    // Peer1 downloads both E01 and E02 separately.
    for ep in [1, 2] {
        let (code, body) = call(
            &app,
            Method::POST,
            "/api/preload",
            Some(&peer1.token),
            Some(json!({
                "kind": "tv",
                "tmdb_id": 1396,
                "season": 2,
                "episode": ep,
                "infohash": lostfilm.infohash,
            })),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{body}");
    }
    *q.progress.lock().unwrap() = 1.0;
    jobs::sync_preloads(&state).await.unwrap();

    // Admin sees Peer1's series with both episodes and `can_delete: true`.
    let (code, body) = call(&app, Method::GET, "/api/preload", Some(&admin.token), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let admin_list: Vec<PreloadItem> = serde_json::from_value(body).unwrap();
    assert_eq!(admin_list.len(), 1);
    assert!(admin_list[0].can_delete);
    assert!(admin_list[0].requested_by.is_some());
    assert_eq!(admin_list[0].episodes.len(), 2);

    // Peer2 sees Peer1's server download & episodes, with `can_delete: false`, and cannot delete it.
    let (code, body) = call(&app, Method::GET, "/api/preload", Some(&peer2.token), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let peer2_list: Vec<PreloadItem> = serde_json::from_value(body).unwrap();
    assert_eq!(peer2_list.len(), 1);
    assert!(!peer2_list[0].can_delete);
    assert_eq!(peer2_list[0].episodes.len(), 2);
    let pid = peer2_list[0].id;
    let (code, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/preload/{pid}/episode/2/1"),
        Some(&peer2.token),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN);

    // Peer1 deletes only E01 -> torrent remains on qBittorrent with E01 filePrio=0 and E02 still ready.
    let (code, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/preload/{pid}/episode/2/1"),
        Some(&peer1.token),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    assert!(q.deleted.lock().unwrap().is_empty(), "E02 still needed, keep torrent");
    assert_eq!(q.file_prios.lock().unwrap().get(&0).copied(), Some(0));

    let (_, body) = call(&app, Method::GET, "/api/preload", Some(&peer1.token), None).await;
    let after_ep1: Vec<PreloadItem> = serde_json::from_value(body).unwrap();
    assert_eq!(after_ep1.len(), 1);
    assert_eq!(after_ep1[0].episodes.len(), 1);
    assert_eq!(after_ep1[0].episodes[0].episode, 2);

    // Deleting the last episode (E02) removes the entire torrent and files.
    let (code, _) = call(
        &app,
        Method::DELETE,
        &format!("/api/preload/{pid}/episode/2/2"),
        Some(&peer1.token),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    assert_eq!(q.deleted.lock().unwrap().len(), 1);
}
