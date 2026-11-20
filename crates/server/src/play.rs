//! Releases, playback and the video stream: `/api/releases`, `/api/play`,
//! `/api/stream`.

use std::path::{Component, Path as FsPath, PathBuf};

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use futures_util::StreamExt;
use kino_core::{
    demo_releases, language_name, MediaKind, MediaTrack, PlayInfo, PlayRequest, ReleaseList, TitleDetails, UserPrefs,
};
use serde::Deserialize;
use tower::ServiceExt;

use crate::{
    api::{parse_kind, AppState, CurrentUser},
    config::TorrentConfig,
    db,
    error::AppError,
    torrent::{
        self,
        ffmpeg::{self, HwAccel, Probe},
        files::{self, FileEntry},
        qbit::QBittorrent,
        token::{self, StreamClaims},
        torrserver::TorrServer,
        torznab::Jackett,
        TorrentError,
    },
    userdata,
};

/// Clients of the torrent services that are configured.
pub struct Media {
    pub http: reqwest::Client,
    pub jackett: Option<Jackett>,
    pub torrserver: Option<TorrServer>,
    pub qbit: Option<QBittorrent>,
    pub preload_dir: Option<PathBuf>,
    pub ffmpeg: String,
    pub ffprobe: String,
    pub hw: HwAccel,
    /// Re-encoding is heavy: only a few at once on a small server.
    pub transcodes: std::sync::Arc<tokio::sync::Semaphore>,
}

/// How many videos the server re-encodes at the same time.
const MAX_TRANSCODES: usize = 2;

impl Media {
    pub fn new(c: &TorrentConfig) -> Self {
        let http = torrent::http_client();
        let or = |s: &str, d: &str| if s.is_empty() { d.to_owned() } else { s.to_owned() };
        Self {
            jackett: match (&c.jackett_url, &c.jackett_api_key) {
                (Some(url), Some(key)) => Some(Jackett::new(http.clone(), url, key)),
                _ => None,
            },
            torrserver: c.torrserver_url.as_ref().map(|url| {
                let auth = c.torrserver_user.clone().zip(c.torrserver_password.clone());
                TorrServer::new(url, auth)
            }),
            qbit: c.qbittorrent_url.as_ref().map(|url| {
                QBittorrent::new(
                    url,
                    &c.qbittorrent_user,
                    &c.qbittorrent_password,
                    c.qbittorrent_save_path.clone(),
                )
            }),
            preload_dir: c.preload_dir.clone(),
            ffmpeg: or(&c.ffmpeg, "ffmpeg"),
            ffprobe: or(&c.ffprobe, "ffprobe"),
            hw: HwAccel::parse(&c.hwaccel),
            transcodes: std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_TRANSCODES)),
            http,
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/releases/{kind}/{tmdb_id}", get(releases))
        .route("/play", post(play))
        .route("/stream/{token}", get(stream))
        .route("/stream/{token}/sub/{file}", get(subtitle))
}

impl From<TorrentError> for AppError {
    fn from(e: TorrentError) -> Self {
        match e {
            TorrentError::Unknown => AppError::new(
                StatusCode::NOT_FOUND,
                "unknown_release",
                "Раздача не найдена, обновите список",
            ),
            TorrentError::Db(e) => e.into(),
            other => {
                tracing::warn!(error = %other, "torrent service failed");
                AppError::new(
                    StatusCode::BAD_GATEWAY,
                    "torrent_unavailable",
                    "Трекеры сейчас недоступны",
                )
            }
        }
    }
}

#[derive(Deserialize)]
struct SeasonQuery {
    season: Option<i32>,
}

/// Releases of a title from every tracker in Jackett, or demo data without it.
async fn releases(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
    Query(q): Query<SeasonQuery>,
) -> Result<Json<ReleaseList>, AppError> {
    let kind = parse_kind(&kind)?;
    let details = state.catalog.details(kind, tmdb_id).await?;
    let Some(jackett) = &state.media.jackett else {
        let season = (kind == MediaKind::Tv).then(|| q.season.unwrap_or(1));
        return Ok(Json(ReleaseList {
            releases: demo_releases(&details.summary, season),
            configured: false,
        }));
    };
    let items = torrent::search(jackett, &details).await?;
    torrent::save_sources(&state.pool, &items).await?;
    let mut releases: Vec<_> = items.iter().map(torrent::to_release).collect();
    // Releases downloaded before are shown by their real info-hash, so the
    // remembered choice and "По просмотренным" find them.
    let keys: Vec<String> = releases
        .iter()
        .filter(|r| !r.has_infohash())
        .map(|r| r.infohash.clone())
        .collect();
    let known: Vec<(String, String)> =
        sqlx::query_as("SELECT key, infohash FROM torrent_source WHERE key = ANY($1) AND infohash IS NOT NULL")
            .bind(&keys)
            .fetch_all(&state.pool)
            .await?;
    for r in &mut releases {
        if let Some((_, h)) = known.iter().find(|(k, _)| *k == r.infohash) {
            r.infohash = h.clone();
        }
    }
    Ok(Json(ReleaseList {
        releases,
        configured: true,
    }))
}

/// How long a stream link works.
const STREAM_TTL: chrono::Duration = chrono::Duration::hours(12);

fn unavailable(msg: &str) -> AppError {
    AppError::new(StatusCode::SERVICE_UNAVAILABLE, "player_unavailable", msg)
}

/// Prepares playback: finds the file, looks inside it and hands out a stream link.
async fn play(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<PlayRequest>,
) -> Result<Json<PlayInfo>, AppError> {
    let key = req.release.to_ascii_lowercase();
    if !kino_core::is_release_key(&key) || req.tmdb_id <= 0 || req.season < 0 || req.episode < 0 {
        return Err(AppError::bad_request(
            "need a release key, a positive tmdb_id and season/episode >= 0",
        ));
    }
    let media = &state.media;
    let src = torrent::resolve(&state.pool, &media.http, &key).await?;
    let hash = src.infohash.clone().ok_or(TorrentError::Unknown)?;
    let episode = (req.kind == MediaKind::Tv).then_some((req.season.max(1), req.episode.max(1)));

    // A release downloaded in advance is played from disk; otherwise through TorrServer.
    let local = preloaded_files(&state, user.0, &hash).await;
    let (file, input, preloaded) = match local {
        Some((list, dir)) => {
            let f = files::pick(&list, episode).cloned().ok_or_else(no_file)?;
            let path = safe_join(&dir, &f.path).ok_or_else(no_file)?;
            let input = path.to_string_lossy().into_owned();
            (f, input, true)
        }
        None => {
            let ts = media
                .torrserver
                .as_ref()
                .ok_or_else(|| unavailable("Потоковый сервер не настроен"))?;
            let list = ts.add(&src).await.map_err(|e| {
                tracing::warn!(error = %e, "TorrServer failed");
                unavailable("Потоковый сервер не отвечает")
            })?;
            if list.is_empty() {
                return Err(unavailable(
                    "Нет раздающих: не удалось получить список файлов. Попробуйте «Скачать заранее».",
                ));
            }
            let f = files::pick(&list, episode).cloned().ok_or_else(no_file)?;
            let input = ts.stream_url_for_ffmpeg(&hash, f.index);
            (f, input, false)
        }
    };

    let probe = match ffmpeg::probe(&media.ffprobe, &input).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "ffprobe failed, playing the file as it is");
            Probe::default()
        }
    };
    // Without ffprobe nothing is known; let the browser try mp4/webm files directly.
    let direct = if probe.container.is_empty() {
        file.path.to_ascii_lowercase().ends_with(".mp4")
    } else {
        probe.browser_ok()
    };

    let claims = StreamClaims {
        uid: user.0,
        h: hash.clone(),
        i: file.index,
        exp: (Utc::now() + STREAM_TTL).timestamp(),
        p: preloaded.then(|| file.path.clone()),
        v: probe.video_copyable(),
    };
    let stream = format!("/api/stream/{}", token::issue(&state.config.session_secret, &claims));

    let (season, ep) = episode.unwrap_or((0, 0));
    let progress = db::title_progress(&state.pool, user.0, req.kind, req.tmdb_id).await?;
    let start_at = progress
        .iter()
        .find(|p| p.season == season && p.episode == ep && !p.finished)
        .map(|p| p.position_sec as i32)
        .unwrap_or(0);
    let st = userdata::get_state(&state.pool, user.0, req.kind, req.tmdb_id).await?;
    let prefs = userdata::get_prefs(&state.pool, user.0).await?;
    let (audio_track, subtitle_track) =
        choose_tracks(&probe, st.audio_track.as_deref(), st.subtitle_track.as_deref(), &prefs);
    let next = match episode {
        Some(e) => next_episode(&state.catalog.details(MediaKind::Tv, req.tmdb_id).await?, e),
        None => None,
    };

    Ok(Json(PlayInfo {
        infohash: hash,
        file_index: file.index,
        file_name: file.path.rsplit('/').next().unwrap_or(&file.path).to_owned(),
        stream,
        direct,
        duration: probe.duration,
        audio: probe.audio,
        subtitles: probe.subtitles,
        audio_track,
        subtitle_track,
        start_at,
        next,
        preloaded,
    }))
}

fn no_file() -> AppError {
    AppError::new(StatusCode::NOT_FOUND, "no_file", "В раздаче нет этой серии")
}

/// Files of a finished preload of this user, with the folder they are in.
async fn preloaded_files(state: &AppState, user_id: i64, hash: &str) -> Option<(Vec<FileEntry>, PathBuf)> {
    let (qbit, dir) = (state.media.qbit.as_ref()?, state.media.preload_dir.clone()?);
    let ready: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM preload WHERE user_id = $1 AND infohash = $2 AND state = 'ready')",
    )
    .bind(user_id)
    .bind(hash)
    .fetch_one(&state.pool)
    .await
    .ok()?;
    if !ready {
        return None;
    }
    match qbit.files(hash).await {
        Ok(list) => Some((list, dir)),
        Err(e) => {
            tracing::warn!(error = %e, "qBittorrent files failed, streaming instead");
            None
        }
    }
}

/// `dir/rel` when `rel` stays inside `dir`.
fn safe_join(dir: &FsPath, rel: &str) -> Option<PathBuf> {
    let rel = FsPath::new(rel);
    rel.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then(|| dir.join(rel))
}

/// The remembered tracks, else the preferred voice-over and subtitle language.
pub fn choose_tracks(
    probe: &Probe,
    audio: Option<&str>,
    subtitle: Option<&str>,
    prefs: &UserPrefs,
) -> (Option<i32>, Option<i32>) {
    let by_label = |list: &[MediaTrack], label: Option<&str>| {
        label
            .and_then(|l| list.iter().find(|t| t.label() == l))
            .map(|t| t.index)
    };
    let audio = by_label(&probe.audio, audio)
        .or_else(|| {
            let voice = prefs.voice.as_deref()?.to_lowercase();
            probe
                .audio
                .iter()
                .find(|t| t.title.as_deref().is_some_and(|n| n.to_lowercase().contains(&voice)))
                .map(|t| t.index)
        })
        .or_else(|| {
            probe
                .audio
                .iter()
                .find(|t| t.default)
                .or(probe.audio.first())
                .map(|t| t.index)
        });
    let subtitle = match subtitle {
        Some("off") => None,
        Some(l) if by_label(&probe.subtitles, Some(l)).is_some() => by_label(&probe.subtitles, Some(l)),
        _ => prefs.subtitle_lang.as_deref().and_then(|want| {
            probe
                .subtitles
                .iter()
                .find(|t| t.language.as_deref().map(language_name) == Some(want))
                .map(|t| t.index)
        }),
    };
    (audio, subtitle)
}

/// The episode after `(season, episode)`, if it is out.
pub fn next_episode(d: &TitleDetails, (season, episode): (i32, i32)) -> Option<(i32, i32)> {
    let cur = d.seasons.iter().find(|s| s.number == season)?;
    let next = if episode < cur.episode_count {
        (season, episode + 1)
    } else {
        d.seasons
            .iter()
            .find(|s| s.number == season + 1 && s.episode_count > 0)?;
        (season + 1, 1)
    };
    match d.last_episode {
        Some(last) if next > last => None,
        _ => Some(next),
    }
}

#[derive(Deserialize)]
struct StreamQuery {
    audio: Option<i32>,
    start: Option<u64>,
}

fn claims(state: &AppState, token: &str) -> Result<StreamClaims, AppError> {
    token::verify(&state.config.session_secret, token, Utc::now())
        .ok_or_else(|| AppError::unauthorized("stream link expired, start playing again"))
}

/// The video: the file as it is (with Range, for seeking), or re-encoded from
/// `start` with the chosen audio when `audio`/`start` are given.
async fn stream(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(q): Query<StreamQuery>,
    req: Request,
) -> Result<Response, AppError> {
    let c = claims(&state, &token)?;
    let media = &state.media;
    let input = input_of(&state, &c)?;
    if q.audio.is_some() || q.start.is_some() {
        // A seek drops the previous request (and its ffmpeg) first, so wait a moment for its slot.
        let permit = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            media.transcodes.clone().acquire_owned(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or_else(|| unavailable("Сервер уже перекодирует другие видео, попробуйте позже"))?;
        let args = ffmpeg::transcode_args(&input, q.start.unwrap_or(0), q.audio.unwrap_or(0), c.v, media.hw);
        let mut child = ffmpeg::spawn(&media.ffmpeg, &args).map_err(|e| {
            tracing::error!(error = %e, "ffmpeg did not start");
            unavailable("Перекодирование недоступно")
        })?;
        let out = child
            .stdout
            .take()
            .ok_or_else(|| unavailable("Перекодирование недоступно"))?;
        // The child lives as long as the response body; dropping it stops ffmpeg.
        let body = tokio_util::io::ReaderStream::new(out).map(move |chunk| {
            let _keep = (&child, &permit);
            chunk
        });
        return Ok((
            [(header::CONTENT_TYPE, "video/mp4"), (header::CACHE_CONTROL, "no-store")],
            Body::from_stream(body),
        )
            .into_response());
    }
    if c.p.is_some() {
        // tower-http handles Range and HEAD for files on disk.
        let resp = tower_http::services::ServeFile::new(&input)
            .oneshot(req)
            .await
            .map_err(|_| AppError::not_found())?;
        return Ok(resp.into_response());
    }
    let ts = media
        .torrserver
        .as_ref()
        .ok_or_else(|| unavailable("Потоковый сервер не настроен"))?;
    proxy(ts, &ts.stream_url(&c.h, c.i), req.headers()).await
}

/// Where the file is: on disk for a preload, else TorrServer's stream.
fn input_of(state: &AppState, c: &StreamClaims) -> Result<String, AppError> {
    match &c.p {
        Some(rel) => {
            let dir = state
                .media
                .preload_dir
                .as_ref()
                .ok_or_else(|| unavailable("Папка загрузок не настроена"))?;
            Ok(safe_join(dir, rel)
                .ok_or_else(AppError::not_found)?
                .to_string_lossy()
                .into_owned())
        }
        None => {
            let ts = state
                .media
                .torrserver
                .as_ref()
                .ok_or_else(|| unavailable("Потоковый сервер не настроен"))?;
            Ok(ts.stream_url_for_ffmpeg(&c.h, c.i))
        }
    }
}

/// Passes the request to TorrServer with its Range header and streams the answer back.
async fn proxy(ts: &TorrServer, url: &str, headers: &HeaderMap) -> Result<Response, AppError> {
    let mut r = ts.get(url);
    if let Some(range) = headers.get(header::RANGE) {
        r = r.header(header::RANGE, range.as_bytes());
    }
    let upstream = r.send().await.map_err(|e| {
        tracing::warn!(error = %e, "TorrServer stream failed");
        unavailable("Потоковый сервер не отвечает")
    })?;
    let status = StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut resp = Response::builder().status(status);
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_LENGTH,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
    ] {
        if let Some(v) = upstream.headers().get(name.as_str()) {
            if let Ok(v) = HeaderValue::from_bytes(v.as_bytes()) {
                resp = resp.header(name, v);
            }
        }
    }
    resp.body(Body::from_stream(upstream.bytes_stream()))
        .map_err(|_| unavailable("Потоковый сервер не отвечает"))
}

/// A subtitle track as WebVTT, for `<track>`.
async fn subtitle(
    State(state): State<AppState>,
    Path((token, file)): Path<(String, String)>,
    Query(q): Query<StreamQuery>,
) -> Result<Response, AppError> {
    let c = claims(&state, &token)?;
    let index: i32 = file
        .trim_end_matches(".vtt")
        .parse()
        .map_err(|_| AppError::bad_request("expected <number>.vtt"))?;
    let input = input_of(&state, &c)?;
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        tokio::process::Command::new(&state.media.ffmpeg)
            .args(ffmpeg::subtitle_args(&input, index, q.start.unwrap_or(0)))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| unavailable("Субтитры не успели загрузиться"))?
    .map_err(|_| unavailable("Субтитры недоступны"))?;
    if !out.status.success() {
        return Err(AppError::not_found());
    }
    Ok(([(header::CONTENT_TYPE, "text/vtt; charset=utf-8")], out.stdout).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kino_core::{SeasonSummary, TitleSummary};

    fn track(index: i32, lang: &str, title: Option<&str>, default: bool) -> MediaTrack {
        MediaTrack {
            index,
            language: Some(lang.into()),
            title: title.map(String::from),
            codec: "aac".into(),
            channels: Some(2),
            default,
        }
    }

    #[test]
    fn tracks() {
        let probe = Probe {
            audio: vec![track(0, "eng", None, true), track(1, "rus", Some("LostFilm"), false)],
            subtitles: vec![track(0, "eng", None, false), track(1, "rus", Some("Полные"), false)],
            ..Probe::default()
        };
        let none = UserPrefs::default();
        assert_eq!(choose_tracks(&probe, None, None, &none), (Some(0), None));
        let prefs = UserPrefs {
            voice: Some("LostFilm".into()),
            subtitle_lang: Some("Русский".into()),
            ..none
        };
        assert_eq!(choose_tracks(&probe, None, None, &prefs), (Some(1), Some(1)));
        // What was picked in the player wins over the preferences.
        assert_eq!(
            choose_tracks(&probe, Some("Английский"), Some("off"), &prefs),
            (Some(0), None)
        );
    }

    #[test]
    fn next() {
        let season = |number, episode_count| SeasonSummary {
            number,
            name: String::new(),
            episode_count,
            year: None,
        };
        let d = TitleDetails {
            summary: TitleSummary {
                kind: MediaKind::Tv,
                tmdb_id: 1,
                title: "T".into(),
                original_title: None,
                year: None,
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
            },
            genres: vec![],
            runtime_min: None,
            tagline: None,
            seasons: vec![season(1, 7), season(2, 13), season(3, 13)],
            imdb_id: None,
            last_episode: Some((3, 5)),
        };
        assert_eq!(next_episode(&d, (1, 6)), Some((1, 7)));
        assert_eq!(next_episode(&d, (1, 7)), Some((2, 1)));
        assert_eq!(next_episode(&d, (3, 5)), None);
        assert_eq!(next_episode(&d, (9, 1)), None);
    }

    #[test]
    fn paths_stay_inside() {
        let d = FsPath::new("/data");
        assert_eq!(safe_join(d, "Show/e1.mkv"), Some(PathBuf::from("/data/Show/e1.mkv")));
        assert_eq!(safe_join(d, "../etc/passwd"), None);
        assert_eq!(safe_join(d, "/etc/passwd"), None);
    }
}
