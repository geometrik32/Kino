//! Releases, playback and the video stream: `/api/releases`, `/api/play`,
//! `/api/stream`.

use std::path::{Component, Path as FsPath, PathBuf};

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use chrono::Utc;
use futures_util::StreamExt;
use kino_core::{
    demo_releases, language_name, normalize_quality_key, MediaKind, MediaTrack, PlayInfo, PlayProbe, PlayProbeRequest,
    PlayRequest, ReleaseChoice, ReleaseList, ReleaseOption, ResolveReleaseRequest, ResolveReleaseResponse,
    StreamProgress, TitleDetails, UserPrefs, MOBILE_720P_MIN_SEEDERS,
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
    /// Recent indexer results; see [`torrent::search_cache`].
    pub searches: torrent::search_cache::SearchCache,
    pub preload_dir: Option<PathBuf>,
    pub ffmpeg: String,
    pub ffprobe: String,
    pub hw: HwAccel,
    /// Re-encoding is heavy: only a few at once on a small server.
    pub transcodes: std::sync::Arc<tokio::sync::Semaphore>,
    /// Subtitle extraction spawns ffmpeg too; cap it so requests cannot pile up.
    pub subtitles: std::sync::Arc<tokio::sync::Semaphore>,
}

/// Default maximum videos re-encoded concurrently (configurable via `MAX_TRANSCODES`).
const DEFAULT_MAX_TRANSCODES: usize = 2;
/// How many subtitle extractions run concurrently.
const MAX_SUBTITLES: usize = 2;
/// Drops a paused/stalled transcode if the client stops reading for this long.
const TRANSCODE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

impl Media {
    pub fn new(c: &TorrentConfig) -> Self {
        let http = torrent::http_client();
        let or = |s: &str, d: &str| if s.is_empty() { d.to_owned() } else { s.to_owned() };
        let transcodes = if c.max_transcodes > 0 {
            c.max_transcodes
        } else {
            DEFAULT_MAX_TRANSCODES
        };
        let jackett = match (&c.jackett_url, &c.jackett_api_key) {
            (Some(url), Some(key)) => {
                let mut j = Jackett::new(http.clone(), url, key);
                if let Some(wake_url) = &c.jackett_wake_url {
                    let group = if c.jackett_wake_group.is_empty() {
                        "jackett".to_owned()
                    } else {
                        c.jackett_wake_group.clone()
                    };
                    let duration = if c.jackett_wake_duration.is_empty() {
                        "10m".to_owned()
                    } else {
                        c.jackett_wake_duration.clone()
                    };
                    j = j.with_wake(torrent::torznab::JackettWake::new(
                        http.clone(),
                        wake_url,
                        group,
                        duration,
                    ));
                }
                Some(j)
            }
            _ => None,
        };
        Self {
            jackett,
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
            searches: torrent::search_cache::SearchCache::default(),
            preload_dir: c.preload_dir.clone(),
            ffmpeg: or(&c.ffmpeg, "ffmpeg"),
            ffprobe: or(&c.ffprobe, "ffprobe"),
            hw: HwAccel::parse(&c.hwaccel),
            transcodes: std::sync::Arc::new(tokio::sync::Semaphore::new(transcodes)),
            subtitles: std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_SUBTITLES)),
            http,
        }
    }

    /// Proactively wakes a sleeping Jackett container in the background when a user starts browsing.
    pub fn wake_jackett(&self) {
        if let Some(j) = &self.jackett {
            j.wake_in_background();
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/releases/{kind}/{tmdb_id}", get(releases))
        .route("/releases/{kind}/{tmdb_id}/resolve", post(resolve_release))
        .route("/release-quality", put(put_release_quality))
        .route("/play", post(play))
        .route("/play/probe", post(play_probe))
        .route("/play/progress/{release}", get(play_progress))
        .merge(
            // The stream links are signed and the apps play them from another origin,
            // subtitles included: let any page read them.
            Router::new()
                .route("/stream/{token}", get(stream))
                .route("/stream/{token}/sub/{file}", get(subtitle))
                .layer(
                    tower_http::cors::CorsLayer::new()
                        .allow_origin(tower_http::cors::Any)
                        .allow_methods([axum::http::Method::GET, axum::http::Method::HEAD]),
                ),
        )
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

async fn fetch_release_list(
    state: &AppState,
    details: &TitleDetails,
    season: Option<i32>,
) -> Result<ReleaseList, AppError> {
    let kind = details.summary.kind;
    let Some(jackett) = &state.media.jackett else {
        let s = (kind == MediaKind::Tv).then(|| season.unwrap_or(1));
        return Ok(ReleaseList {
            releases: demo_releases(&details.summary, s),
            configured: false,
        });
    };
    let items = torrent::search_cached(&state.media.searches, &state.pool, jackett, details).await?;
    let mut releases: Vec<_> = items.iter().map(|it| torrent::to_release(it, details)).collect();
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
    Ok(ReleaseList {
        releases,
        configured: true,
    })
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
    Ok(Json(fetch_release_list(&state, &details, q.season).await?))
}

#[derive(Deserialize)]
struct SetDeviceQualityRequest {
    kind: MediaKind,
    tmdb_id: i64,
    #[serde(default)]
    season: i32,
    #[serde(default)]
    episode: i32,
    #[serde(default)]
    is_mobile: bool,
    quality_key: String,
}

async fn put_release_quality(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<SetDeviceQualityRequest>,
) -> Result<StatusCode, AppError> {
    if req.tmdb_id <= 0 || req.season < 0 || req.episode < 0 {
        return Err(AppError::bad_request("invalid tmdb_id or season/episode"));
    }
    let qkey = normalize_quality_key(&req.quality_key);
    let device_kind = if req.is_mobile { "mobile" } else { "desktop" };
    let (season, episode) = if req.kind == MediaKind::Movie {
        (0, 0)
    } else {
        (req.season, req.episode)
    };
    db::save_device_quality(
        &state.pool,
        user.0,
        req.kind,
        req.tmdb_id,
        season,
        episode,
        device_kind,
        qkey,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetQuality {
    Ask,
    Exact(String),
    MobileAuto,
}

impl TargetQuality {
    fn from_prefs(prefs: &UserPrefs, device_override: Option<String>, is_mobile: bool) -> Self {
        if let Some(q) = device_override {
            return Self::Exact(normalize_quality_key(&q).to_owned());
        }
        let configured = if is_mobile {
            prefs.mobile_quality.as_deref()
        } else {
            prefs.desktop_quality.as_deref().or(prefs.quality.as_deref())
        };
        match configured {
            Some("ask") => Self::Ask,
            Some("auto") | None if is_mobile => Self::MobileAuto,
            None => Self::Exact("1080p".to_owned()),
            Some(q) => Self::Exact(normalize_quality_key(q).to_owned()),
        }
    }

    fn matches_key(&self, qkey: &str) -> bool {
        match self {
            Self::Ask => false,
            Self::Exact(want) => qkey == want,
            Self::MobileAuto => qkey == "720p" || qkey == "SD",
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Ask => String::new(),
            Self::Exact(q) => q.clone(),
            Self::MobileAuto => "720p".to_owned(),
        }
    }
}

fn find_scoped_choice(
    choices: &[ReleaseChoice],
    season: i32,
    episode: i32,
    pred: impl Fn(&str) -> bool,
) -> Option<&ReleaseChoice> {
    let matches_q = |c: &&ReleaseChoice| {
        let k = c
            .release
            .quality_key
            .as_deref()
            .or(c.release.quality.as_deref())
            .map(normalize_quality_key)
            .unwrap_or("");
        !k.is_empty() && pred(k)
    };
    choices
        .iter()
        .filter(matches_q)
        .find(|c| c.release.season == season && c.release.episode == episode)
        .or_else(|| {
            choices
                .iter()
                .filter(matches_q)
                .find(|c| c.release.season == season && c.release.episode == 0)
        })
        .or_else(|| {
            choices
                .iter()
                .filter(matches_q)
                .find(|c| c.release.season == 0 && c.release.episode == 0)
        })
}

fn auto_pick_release<'a>(
    releases: &'a [ReleaseOption],
    kind: MediaKind,
    season: i32,
    target: &TargetQuality,
    preferred_voice: Option<&str>,
    preferred_lang: Option<&str>,
    is_mobile: bool,
) -> Option<&'a ReleaseOption> {
    let for_season: Vec<&ReleaseOption> = releases
        .iter()
        .filter(|r| kind == MediaKind::Movie || r.season.is_none() || r.season == Some(season))
        .collect();
    if for_season.is_empty() {
        return None;
    }
    let pool: Vec<&ReleaseOption> = match preferred_lang {
        Some(lang) => {
            let matching: Vec<&ReleaseOption> = for_season.iter().copied().filter(|r| r.language == lang).collect();
            if matching.is_empty() {
                for_season
            } else {
                matching
            }
        }
        None => for_season,
    };

    let chosen_bucket: &'static str = match target {
        TargetQuality::Ask => return None,
        TargetQuality::Exact(q) => normalize_quality_key(q),
        TargetQuality::MobileAuto => {
            let best_720 = pool
                .iter()
                .filter(|r| normalize_quality_key(&r.quality) == "720p")
                .map(|r| r.seeders)
                .max();
            let best_sd = pool
                .iter()
                .filter(|r| normalize_quality_key(&r.quality) == "SD")
                .map(|r| r.seeders)
                .max();
            match (best_720, best_sd) {
                (Some(s720), _) if s720 >= MOBILE_720P_MIN_SEEDERS => "720p",
                (Some(s720), Some(ssd)) if ssd > s720 => "SD",
                (Some(_), _) => "720p",
                (None, Some(_)) => "SD",
                (None, None) => return None,
            }
        }
    };

    let candidates: Vec<&ReleaseOption> = pool
        .into_iter()
        .filter(|r| normalize_quality_key(&r.quality) == chosen_bucket)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    candidates.into_iter().max_by_key(|r| {
        let voice_match = preferred_voice.is_some_and(|v| r.voice.eq_ignore_ascii_case(v));
        let rus_match = r.language == preferred_lang.unwrap_or("Русский");
        let season_exact = kind == MediaKind::Tv && r.season == Some(season);
        let direct_bonus = !is_mobile && r.direct_play && r.seeders >= 5;
        (voice_match, rus_match, season_exact, direct_bonus, r.seeders)
    })
}

/// Automatically picks a release by quality (device default / prefs) and other users' choices.
async fn resolve_release(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
    Json(req): Json<ResolveReleaseRequest>,
) -> Result<Json<ResolveReleaseResponse>, AppError> {
    let kind = parse_kind(&kind)?;
    if tmdb_id <= 0 || req.season < 0 || req.episode < 0 {
        return Err(AppError::bad_request("invalid tmdb_id or season/episode"));
    }
    let (season, episode) = if kind == MediaKind::Movie {
        (0, 0)
    } else {
        (req.season, req.episode)
    };
    let device_kind = if req.is_mobile { "mobile" } else { "desktop" };
    let prefs = userdata::get_prefs(&state.pool, user.0).await?;
    let dev_override = db::device_quality_for(&state.pool, user.0, kind, tmdb_id, season, episode, device_kind).await?;
    let target = TargetQuality::from_prefs(&prefs, dev_override, req.is_mobile);

    let details = state.catalog.details(kind, tmdb_id).await?;
    if target == TargetQuality::Ask {
        let list = fetch_release_list(&state, &details, (kind == MediaKind::Tv).then_some(season)).await?;
        return Ok(Json(ResolveReleaseResponse::NeedsPicker {
            target_quality: String::new(),
            releases: list,
        }));
    }

    let mine = db::release_choices(&state.pool, user.0, kind, tmdb_id).await?;
    if let Some(c) = find_scoped_choice(&mine, season, episode, |k| target.matches_key(k)) {
        return Ok(Json(ResolveReleaseResponse::Ready {
            choice: c.release.clone(),
            from_peer: None,
        }));
    }

    let shared = db::shared_release_choices(&state.pool, user.0, kind, tmdb_id).await?;
    if let Some(c) = find_scoped_choice(&shared, season, episode, |k| target.matches_key(k)) {
        let mut adopted = c.release.clone();
        if kind == MediaKind::Movie {
            adopted.season = 0;
            adopted.episode = 0;
        }
        let _ = db::save_release_choice(&state.pool, user.0, &adopted).await;
        return Ok(Json(ResolveReleaseResponse::Ready {
            choice: adopted,
            from_peer: c.picked_by.clone(),
        }));
    }

    let preferred_voice = mine
        .first()
        .or_else(|| shared.first())
        .and_then(|c| c.release.voice.clone());
    let list = fetch_release_list(&state, &details, (kind == MediaKind::Tv).then_some(season)).await?;

    let picked = auto_pick_release(
        &list.releases,
        kind,
        season,
        &target,
        preferred_voice.as_deref(),
        prefs.language.as_deref(),
        req.is_mobile,
    );
    match picked {
        Some(best) => {
            let save_season = if kind == MediaKind::Movie || best.season.is_none() {
                0
            } else {
                season
            };
            let choice = best.to_scoped_choice(&details.summary, save_season, 0);
            db::save_release_choice(&state.pool, user.0, &choice).await?;
            Ok(Json(ResolveReleaseResponse::Ready {
                choice,
                from_peer: None,
            }))
        }
        None => Ok(Json(ResolveReleaseResponse::NeedsPicker {
            target_quality: target.label(),
            releases: list,
        })),
    }
}

/// Starts the release search for a title in the background, so the list is ready
/// by the time someone presses "Смотреть". Skips titles where the user already has
/// a saved release choice (so "Продолжить просмотр" does not wake a sleeping indexer).
pub fn prefetch_releases(state: &AppState, user_id: i64, details: TitleDetails) {
    if state.media.jackett.is_none() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let kind = details.summary.kind;
        let tmdb_id = details.summary.tmdb_id;
        let has_saved: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM release_choice WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3
             ) OR EXISTS (
                 SELECT 1 FROM watch_progress
                 WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3 AND release_infohash IS NOT NULL
             )",
        )
        .bind(user_id)
        .bind(kind.as_str())
        .bind(tmdb_id)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(false);
        if has_saved {
            return;
        }
        if let Some(jackett) = &state.media.jackett {
            if let Err(e) = torrent::search_cached(&state.media.searches, &state.pool, jackett, &details).await {
                tracing::debug!(error = %e, "background release search failed");
            }
        }
    });
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
    let src = torrent::resolve(&state.pool, &media.http, &key, media.jackett.as_ref()).await?;
    let hash = src.infohash.clone().ok_or(TorrentError::Unknown)?;
    let episode = (req.kind == MediaKind::Tv).then_some((req.season.max(1), req.episode.max(1)));

    // A release downloaded in advance is played from disk; otherwise through TorrServer.
    let local = preloaded_files(&state, user.0, &hash).await;
    let (file, input, preloaded) = match local {
        Some((list, dir)) => {
            let f = files::pick(&list, episode).cloned().ok_or_else(no_file)?;
            let path = safe_join_canonical(&dir, &f.path).await.ok_or_else(no_file)?;
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
            // Load the start of the file now, so the player can show how far it got.
            ts.warm_up(&hash, f.index);
            let input = ts.stream_url_for_ffmpeg(&hash, f.index);
            (f, input, false)
        }
    };

    let probe = match ffmpeg::probe(&media.ffprobe, &input).await {
        Ok(p) => p,
        Err(e) => {
            if !preloaded {
                if let Some(ts) = &media.torrserver {
                    let stalled = match ts.progress(&hash).await {
                        Ok(p) => p.peers == 0 && p.loaded_bytes == 0 && p.speed_bps == 0,
                        Err(_) => true,
                    };
                    if stalled {
                        return Err(unavailable(
                            "Нет раздающих: не удалось начать загрузку. Попробуйте другую раздачу или «Скачать заранее».",
                        ));
                    }
                }
            }
            tracing::warn!(error = %e, "ffprobe failed, playing the file as it is");
            Probe::default()
        }
    };
    // Without ffprobe nothing is known; let the browser try mp4/webm files directly.
    // Native clients (Android/Desktop) decode MKV/HEVC/AC3/DTS directly on the client.
    let direct = is_direct(&probe, &file.path, req.native);

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
    let (audio_track, subtitle_track) =
        preferred_tracks(&state, user.0, req.kind, req.tmdb_id, &probe, preloaded, req.native).await?;
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
        native: req.native,
    }))
}

fn is_direct(probe: &Probe, file_path: &str, native: bool) -> bool {
    native
        || if probe.container.is_empty() {
            file_path.to_ascii_lowercase().ends_with(".mp4")
        } else {
            probe.browser_ok()
        }
}

async fn preferred_tracks(
    state: &AppState,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    probe: &Probe,
    preloaded: bool,
    native: bool,
) -> Result<(Option<i32>, Option<i32>), AppError> {
    let st = userdata::get_state(&state.pool, user_id, kind, tmdb_id).await?;
    let prefs = userdata::get_prefs(&state.pool, user_id).await?;
    let (audio_track, subtitle_track) =
        choose_tracks(probe, st.audio_track.as_deref(), st.subtitle_track.as_deref(), &prefs);
    // Extracting embedded WebVTT subtitles scans to EOF and blocks `<video default>` on live torrent streams;
    // native players read subtitles inline from the stream.
    let subtitle_track = if preloaded || native { subtitle_track } else { None };
    Ok((audio_track, subtitle_track))
}

fn extract_stream_token(stream: &str) -> &str {
    stream
        .rsplit_once("/api/stream/")
        .map(|(_, t)| t)
        .unwrap_or(stream)
        .split('?')
        .next()
        .unwrap_or("")
}

/// Retries `ffprobe` for an active stream after TorrServer has buffered more data.
async fn play_probe(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<PlayProbeRequest>,
) -> Result<Json<PlayProbe>, AppError> {
    if req.tmdb_id <= 0 {
        return Err(AppError::bad_request("tmdb_id must be positive"));
    }
    let token = extract_stream_token(&req.stream);
    let c = claims(&state, token)?;
    if c.uid != user.0 {
        return Err(AppError::unauthorized("stream link belongs to another user"));
    }
    let input = input_of(&state, &c).await?;
    let Ok(probe) = ffmpeg::probe_with_timeout(&state.media.ffprobe, &input, std::time::Duration::from_secs(25)).await
    else {
        return Ok(Json(PlayProbe::default()));
    };
    let preloaded = c.p.is_some();
    let direct = is_direct(&probe, c.p.as_deref().unwrap_or(""), req.native);
    let (audio_track, subtitle_track) =
        preferred_tracks(&state, user.0, req.kind, req.tmdb_id, &probe, preloaded, req.native).await?;
    let updated = StreamClaims {
        v: probe.video_copyable(),
        ..c
    };
    let stream = Some(format!(
        "/api/stream/{}",
        token::issue(&state.config.session_secret, &updated)
    ));
    Ok(Json(PlayProbe {
        stream,
        direct,
        duration: probe.duration,
        audio: probe.audio,
        subtitles: probe.subtitles,
        audio_track,
        subtitle_track,
    }))
}

/// How far the connection to a release has got, for the bar shown while the
/// player starts. Never fails: until TorrServer knows the torrent it is "connecting".
async fn play_progress(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(release): Path<String>,
) -> Result<Json<StreamProgress>, AppError> {
    let key = release.to_ascii_lowercase();
    if !kino_core::is_release_key(&key) {
        return Err(AppError::bad_request("need a release key"));
    }
    let Some(ts) = &state.media.torrserver else {
        return Ok(Json(StreamProgress::default()));
    };
    let hash = match torrent::source(&state.pool, &key).await? {
        Some(src) => src.infohash,
        None => None,
    }
    .or_else(|| kino_core::is_infohash(&key).then_some(key));
    let Some(hash) = hash else {
        return Ok(Json(StreamProgress::default()));
    };
    match ts.progress(&hash).await {
        Ok(p) => Ok(Json(p)),
        Err(e) => {
            tracing::warn!(hash, error = %e, "TorrServer progress failed");
            Ok(Json(StreamProgress::default()))
        }
    }
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

/// `dir/rel` when `rel` stays lexically inside `dir`.
fn safe_join(dir: &FsPath, rel: &str) -> Option<PathBuf> {
    let rel = FsPath::new(rel);
    (!rel.as_os_str().is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_)))).then(|| dir.join(rel))
}

/// Resolves `dir/rel` safely against symlink escapes (BEP-47 symlinks inside torrents).
async fn safe_join_canonical(dir: &FsPath, rel: &str) -> Option<PathBuf> {
    let joined = safe_join(dir, rel)?;
    let meta = tokio::fs::symlink_metadata(&joined).await.ok()?;
    if meta.file_type().is_symlink() {
        tracing::warn!(path = %joined.display(), "refusing to serve symlink from preload directory");
        return None;
    }
    let canon_dir = tokio::fs::canonicalize(dir).await.ok()?;
    let canon_file = tokio::fs::canonicalize(&joined).await.ok()?;
    canon_file.starts_with(&canon_dir).then_some(canon_file)
}

/// The remembered tracks, else the preferred voice-over and audio language.
/// Subtitles stay off unless the user explicitly picked a track for this title.
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
            // The audio language from the profile, when the file has such a track.
            let want = prefs.language.as_deref()?;
            probe
                .audio
                .iter()
                .find(|t| t.language.as_deref().map(language_name) == Some(want))
                .map(|t| t.index)
        })
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
        Some("off") | None => None,
        Some(l) => by_label(&probe.subtitles, Some(l)),
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
    let input = input_of(&state, &c).await?;
    if q.audio.is_some() || q.start.is_some() {
        let audio = q.audio.unwrap_or(0);
        if audio < 0 {
            return Err(AppError::bad_request("audio track index must be >= 0"));
        }
        // A seek drops the previous request (and its ffmpeg) first, so wait a moment for its slot.
        let permit = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            media.transcodes.clone().acquire_owned(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or_else(|| unavailable("Сервер уже перекодирует другие видео, попробуйте позже"))?;
        let args = ffmpeg::transcode_args(&input, q.start.unwrap_or(0), audio, c.v, media.hw);
        let mut child = ffmpeg::spawn(&media.ffmpeg, &args).map_err(|e| {
            tracing::error!(error = %e, "ffmpeg did not start");
            unavailable("Перекодирование недоступно")
        })?;
        let out = child
            .stdout
            .take()
            .ok_or_else(|| unavailable("Перекодирование недоступно"))?;
        let stderr_handle = child.stderr.take().map(|mut err| {
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut buf = Vec::new();
                let mut limited = (&mut err).take(4096);
                let _ = limited.read_to_end(&mut buf).await;
                String::from_utf8_lossy(&buf).trim().to_owned()
            })
        });
        let mut reader = tokio_util::io::ReaderStream::new(out);
        let first = match tokio::time::timeout(std::time::Duration::from_secs(45), reader.next()).await {
            Ok(Some(Ok(bytes))) => bytes,
            Ok(other) => {
                if let Some(h) = stderr_handle {
                    if let Ok(Ok(err)) = tokio::time::timeout(std::time::Duration::from_secs(2), h).await {
                        if !err.is_empty() {
                            tracing::warn!(stderr = %err, "ffmpeg exited before producing video");
                        }
                    }
                }
                let _ = other;
                return Err(unavailable("Не удалось запустить видео"));
            }
            Err(_) => {
                return Err(unavailable(
                    "Поток не отвечает. Попробуйте другую раздачу или «Скачать заранее».",
                ));
            }
        };
        // Feed ffmpeg stdout through a bounded channel so client disconnects stop ffmpeg
        // immediately (even while blocked on upstream) and idle clients release their slot.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(8);
        tokio::spawn(async move {
            let _keep = (child, permit);
            if tx.send(Ok(first)).await.is_err() {
                return;
            }
            loop {
                let next_chunk = tokio::select! {
                    biased;
                    _ = tx.closed() => break,
                    chunk = reader.next() => chunk,
                };
                let Some(chunk) = next_chunk else { break };
                match tokio::time::timeout(TRANSCODE_IDLE_TIMEOUT, tx.send(chunk)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => break,
                    Err(_) => {
                        tracing::info!("stopping idle ffmpeg transcode after client stall");
                        break;
                    }
                }
            }
            drop(stderr_handle);
        });
        let body = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) });
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
async fn input_of(state: &AppState, c: &StreamClaims) -> Result<String, AppError> {
    match &c.p {
        Some(rel) => {
            let dir = state
                .media
                .preload_dir
                .as_ref()
                .ok_or_else(|| unavailable("Папка загрузок не настроена"))?;
            Ok(safe_join_canonical(dir, rel)
                .await
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
    let upstream = tokio::time::timeout(std::time::Duration::from_secs(45), r.send())
        .await
        .map_err(|_| {
            tracing::warn!("TorrServer stream timed out");
            unavailable("Потоковый сервер не отвечает")
        })?
        .map_err(|e| {
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
    if c.p.is_none() {
        return Err(unavailable(
            "Встроенные субтитры недоступны при потоковом просмотре с торрента",
        ));
    }
    let index: i32 = file
        .trim_end_matches(".vtt")
        .parse()
        .map_err(|_| AppError::bad_request("expected <number>.vtt"))?;
    if index < 0 {
        return Err(AppError::bad_request("subtitle index must be >= 0"));
    }
    let _permit = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        state.media.subtitles.clone().acquire_owned(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .ok_or_else(|| unavailable("Сервер занят извлечением субтитров, попробуйте позже"))?;
    let input = input_of(&state, &c).await?;
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(60),
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
        assert_eq!(choose_tracks(&probe, None, None, &prefs), (Some(1), None));
        assert_eq!(
            choose_tracks(&probe, None, Some("Полные · Русский"), &prefs),
            (Some(1), Some(1))
        );
        // Just a language in the profile: the first audio track in it, studio or not.
        let by_language = UserPrefs {
            language: Some("Русский".into()),
            ..UserPrefs::default()
        };
        assert_eq!(choose_tracks(&probe, None, None, &by_language), (Some(1), None));
        // A language the file does not have changes nothing.
        let german = UserPrefs {
            language: Some("Немецкий".into()),
            ..UserPrefs::default()
        };
        assert_eq!(choose_tracks(&probe, None, None, &german), (Some(0), None));
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
                user_rating: None,
                user_status: None,
                peers: Vec::new(),
            },
            genres: vec![],
            runtime_min: None,
            tagline: None,
            seasons: vec![season(1, 7), season(2, 13), season(3, 13)],
            imdb_id: None,
            last_episode: Some((3, 5)),
            series_status: None,
            release_date: None,
            next_episode: None,
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

    #[test]
    fn stream_token_extraction() {
        assert_eq!(extract_stream_token("/api/stream/abc.def"), "abc.def");
        assert_eq!(
            extract_stream_token("https://kino.example.com/api/stream/abc.def?audio=1&start=10"),
            "abc.def"
        );
        assert_eq!(extract_stream_token("abc.def"), "abc.def");
    }
}
