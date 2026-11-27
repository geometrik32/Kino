//! Releases, playback and the video stream: `/api/releases`, `/api/play`,
//! `/api/stream`.

use std::path::{Component, Path as FsPath, PathBuf};

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use chrono::Utc;
use futures_util::StreamExt;
use kino_core::{
    demo_releases, language_name, normalize_quality_key, MediaKind, MediaTrack, PlayInfo, PlayProbe, PlayProbeRequest,
    PlayRequest, PreloadPhase, ReleaseChoice, ReleaseChoiceUpdate, ReleaseList, ReleaseOption, ResolveReleaseRequest,
    ResolveReleaseResponse, StreamProgress, TitleDetails, UserPrefs, MOBILE_720P_MIN_SEEDERS,
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
    /// Primary release search indexers (extra Torznab such as JacRed when configured, else Jackett).
    pub indexers: Vec<Jackett>,
    /// Fallback search indexers (Jackett behind Sablier) used only if primary indexers fail to answer.
    pub fallback_indexers: Vec<Jackett>,
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
    /// Background preloaded episode analyzer concurrency cap (single worker to avoid starving CPU).
    pub analysis_sem: std::sync::Arc<tokio::sync::Semaphore>,
    /// Wakes the background segment analyzer right when a TV download completes.
    pub segment_notify: std::sync::Arc<tokio::sync::Notify>,
    /// SSD pause-resume cache for torrent streams (Direct and transcoded).
    pub pause_cache: torrent::pause_cache::PauseCache,
    /// Live `(speed_bps, dl_phase)` telemetry by lowercase infohash from qBittorrent polling.
    pub preload_telemetry: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, (u64, PreloadPhase)>>>,
    /// Recently stalled/zero-peer hashes deprioritized by auto-pick so retries pick another release.
    dead_releases: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
}

/// Default maximum videos re-encoded concurrently (configurable via `MAX_TRANSCODES`).
const DEFAULT_MAX_TRANSCODES: usize = 6;
/// How many subtitle extractions run concurrently.
const MAX_SUBTITLES: usize = 2;
/// Capture a pause-cache snapshot after client reading pauses for this long.
const TRANSCODE_STALL_CAPTURE: std::time::Duration = std::time::Duration::from_secs(10);
/// Drops a paused/stalled transcode if the client stops reading for this long.
const TRANSCODE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(540);
/// Remember stalled zero-byte releases this long when auto-picking another candidate.
const DEAD_RELEASE_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const MAX_DEAD_RELEASES: usize = 256;

impl Media {
    pub fn new(c: &TorrentConfig) -> Self {
        let http = torrent::http_client();
        let or = |s: &str, d: &str| if s.is_empty() { d.to_owned() } else { s.to_owned() };
        let transcodes = if c.max_transcodes > 0 {
            c.max_transcodes
        } else {
            DEFAULT_MAX_TRANSCODES
        };
        let mut jackett_list = Vec::new();
        if let (Some(url), Some(key)) = (&c.jackett_url, &c.jackett_api_key) {
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
            jackett_list.push(j);
        }
        let mut extras = Vec::new();
        for extra in &c.extra_torznab {
            extras.push(
                Jackett::new(http.clone(), &extra.url, extra.api_key.as_deref().unwrap_or(""))
                    .with_timeout(std::time::Duration::from_secs(15)),
            );
        }
        let (indexers, fallback_indexers) = if extras.is_empty() {
            (jackett_list, Vec::new())
        } else {
            (extras, jackett_list)
        };
        let transcodes_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(transcodes));
        let pause_cache = torrent::pause_cache::PauseCache::new(
            c.pause_cache_dir.clone(),
            c.pause_cache_max_mb,
            c.pause_cache_ttl_hours,
        );
        pause_cache.set_live_transcodes(std::sync::Arc::clone(&transcodes_sem));
        Self {
            indexers,
            fallback_indexers,
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
            transcodes: transcodes_sem,
            subtitles: std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_SUBTITLES)),
            analysis_sem: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            segment_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
            pause_cache,
            preload_telemetry: std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            dead_releases: std::sync::Mutex::default(),
            http,
        }
    }

    pub fn has_indexers(&self) -> bool {
        !self.indexers.is_empty() || !self.fallback_indexers.is_empty()
    }

    pub fn all_indexers(&self) -> Vec<Jackett> {
        let mut out = Vec::with_capacity(self.indexers.len() + self.fallback_indexers.len());
        out.extend(self.indexers.iter().cloned());
        out.extend(self.fallback_indexers.iter().cloned());
        out
    }

    /// Proactively wakes a sleeping primary indexer (i.e. Jackett when no external
    /// primary Torznab is configured) in the background when a user starts browsing.
    pub fn wake_jackett(&self) {
        for idx in &self.indexers {
            idx.wake_in_background();
        }
    }

    fn mark_dead_release(&self, hash: &str) {
        if hash.is_empty() {
            return;
        }
        let mut map = self.dead_releases.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, ts| ts.elapsed() < DEAD_RELEASE_TTL);
        if map.len() >= MAX_DEAD_RELEASES {
            if let Some(oldest) = map.iter().min_by_key(|(_, ts)| **ts).map(|(k, _)| k.clone()) {
                map.remove(&oldest);
            }
        }
        map.insert(hash.to_ascii_lowercase(), std::time::Instant::now());
    }

    fn clear_dead_release(&self, hash: &str) {
        if hash.is_empty() {
            return;
        }
        let mut map = self.dead_releases.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(&hash.to_ascii_lowercase());
    }

    fn dead_hashes(&self) -> std::collections::HashSet<String> {
        let mut map = self.dead_releases.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, ts| ts.elapsed() < DEAD_RELEASE_TTL);
        map.keys().cloned().collect()
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
        .route("/segments/{kind}/{tmdb_id}", get(episode_segments))
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

async fn enrich_release_meta(
    pool: &sqlx::PgPool,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    releases: &mut [ReleaseOption],
) -> Result<(), AppError> {
    if releases.is_empty() {
        return Ok(());
    }
    let pickers = db::release_pickers(pool, kind, tmdb_id, season).await?;
    let ready: std::collections::HashSet<String> = userdata::ready_preloads_for_title(pool, kind, tmdb_id)
        .await?
        .into_iter()
        .filter(|p| kind == MediaKind::Movie || p.season == 0 || season == 0 || p.season == season)
        .map(|p| p.infohash)
        .collect();
    for r in releases.iter_mut() {
        if let Some(names) = pickers.get(&r.infohash) {
            r.picked_by = names.clone();
        }
        if ready.contains(&r.infohash) {
            r.preloaded = true;
        }
    }
    Ok(())
}

fn canon_release_title(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn merge_release_dups(dst: &mut ReleaseOption, src: ReleaseOption) {
    dst.seeders = dst.seeders.max(src.seeders);
    dst.preloaded |= src.preloaded;
    for who in src.picked_by {
        if !dst.picked_by.contains(&who) {
            dst.picked_by.push(who);
        }
    }
}

/// Collapses duplicate releases that resolved to the same info-hash or share an
/// identical byte size and normalized title with an already-hashed release.
fn dedup_releases(releases: Vec<ReleaseOption>) -> Vec<ReleaseOption> {
    let mut by_hash: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(releases.len());
    let mut out: Vec<ReleaseOption> = Vec::with_capacity(releases.len());
    for mut r in releases {
        r.infohash.make_ascii_lowercase();
        if let Some(&idx) = by_hash.get(&r.infohash) {
            merge_release_dups(&mut out[idx], r);
        } else {
            by_hash.insert(r.infohash.clone(), out.len());
            out.push(r);
        }
    }
    let hashed_sigs: Vec<(u64, String, usize)> = out
        .iter()
        .enumerate()
        .filter(|(_, r)| r.has_infohash() && r.size_bytes > 0)
        .map(|(i, r)| (r.size_bytes, canon_release_title(&r.title), i))
        .collect();
    let mut merged_into: Vec<Option<usize>> = vec![None; out.len()];
    for (i, r) in out.iter().enumerate() {
        if !r.has_infohash() && r.size_bytes > 0 {
            let ct = canon_release_title(&r.title);
            if let Some(&(_, _, target_idx)) = hashed_sigs
                .iter()
                .find(|(sz, title, _)| *sz == r.size_bytes && title == &ct)
            {
                merged_into[i] = Some(target_idx);
            }
        }
    }
    let mut kept = Vec::with_capacity(out.len());
    let mut taken: Vec<Option<ReleaseOption>> = out.into_iter().map(Some).collect();
    for i in 0..taken.len() {
        if let Some(target_idx) = merged_into[i] {
            if let Some(dup) = taken[i].take() {
                if let Some(Some(target)) = taken.get_mut(target_idx) {
                    merge_release_dups(target, dup);
                }
            }
        }
    }
    for slot in taken.into_iter().flatten() {
        kept.push(slot);
    }
    kept
}

async fn fetch_release_list(
    state: &AppState,
    details: &TitleDetails,
    season: Option<i32>,
) -> Result<ReleaseList, AppError> {
    let kind = details.summary.kind;
    let tmdb_id = details.summary.tmdb_id;
    let season_val = if kind == MediaKind::Tv { season.unwrap_or(0) } else { 0 };
    if !state.media.has_indexers() {
        let s = (kind == MediaKind::Tv).then(|| season.unwrap_or(1));
        let mut releases = demo_releases(&details.summary, s);
        enrich_release_meta(&state.pool, kind, tmdb_id, season_val, &mut releases).await?;
        return Ok(ReleaseList {
            releases,
            configured: false,
        });
    }
    let items = torrent::search_cached(
        &state.media.searches,
        &state.pool,
        &state.media.indexers,
        &state.media.fallback_indexers,
        details,
    )
    .await?;
    let mut releases: Vec<_> = items.iter().map(|it| torrent::to_release(it, details)).collect();
    // Releases downloaded before are shown by their real info-hash, so the
    // remembered choice and "По просмотренным" find them.
    let keys: Vec<String> = releases
        .iter()
        .filter(|r| !r.has_infohash())
        .map(|r| r.infohash.clone())
        .collect();
    let known = torrent::known_infohashes(&state.pool, &keys).await?;
    for r in &mut releases {
        if let Some(h) = known.get(&r.infohash) {
            r.infohash = h.clone();
        }
    }
    let mut releases = dedup_releases(releases);
    enrich_release_meta(&state.pool, kind, tmdb_id, season_val, &mut releases).await?;
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
        (req.kind, req.tmdb_id, season, episode),
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

fn choice_quality_key(c: &ReleaseChoice) -> &str {
    c.release
        .quality_key
        .as_deref()
        .or(c.release.quality.as_deref())
        .map(normalize_quality_key)
        .unwrap_or("")
}

fn find_any_scoped_choice(choices: &[ReleaseChoice], season: i32, episode: i32) -> Option<&ReleaseChoice> {
    choices
        .iter()
        .find(|c| c.release.season == season && c.release.episode == episode)
        .or_else(|| {
            choices
                .iter()
                .find(|c| c.release.season == season && c.release.episode == 0)
        })
        .or_else(|| choices.iter().find(|c| c.release.season == 0 && c.release.episode == 0))
}

fn find_scoped_choice(
    choices: &[ReleaseChoice],
    season: i32,
    episode: i32,
    pred: impl Fn(&str) -> bool,
) -> Option<&ReleaseChoice> {
    let matches_q = |c: &&ReleaseChoice| {
        let k = choice_quality_key(c);
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

#[allow(clippy::too_many_arguments)]
fn auto_pick_release<'a>(
    releases: &'a [ReleaseOption],
    kind: MediaKind,
    season: i32,
    episode: i32,
    target: &TargetQuality,
    preferred_voice: Option<&str>,
    preferred_lang: Option<&str>,
    max_bitrate_mbps: Option<u32>,
    is_mobile: bool,
    dead_hashes: &std::collections::HashSet<String>,
) -> Option<&'a ReleaseOption> {
    let max_kbps = max_bitrate_mbps.filter(|&m| m > 0).map(|m| m.saturating_mul(1000));
    let for_season: Vec<&ReleaseOption> = releases
        .iter()
        .filter(|r| {
            !r.collection
                && (kind == MediaKind::Movie
                    || ((r.season.is_none() || r.season == Some(season)) && r.matches_episode(episode)))
                && max_kbps.is_none_or(|limit| r.bitrate_kbps.is_none_or(|kbps| kbps <= limit))
        })
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
        let not_dead = !dead_hashes.contains(&r.infohash.to_ascii_lowercase());
        let voice_match = preferred_voice.is_some_and(|v| r.voice.eq_ignore_ascii_case(v));
        let rus_match = r.language == preferred_lang.unwrap_or("Русский");
        let season_exact = kind == MediaKind::Tv && r.season == Some(season);
        let direct_bonus = !is_mobile && r.direct_play && r.seeders >= 5;
        (not_dead, voice_match, rus_match, season_exact, direct_bonus, r.seeders)
    })
}

/// Automatically picks a release following strict 4-tier priority:
/// 1. Ready preloaded release on server disk (unless user manually chose another release *after* it finished).
/// 2. User's own manual choice for this scope & quality.
/// 3. Other users' manual choice for this scope & quality (most popular first).
/// 4. Device default quality (popular by seeders, not written as a manual user pick).
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
    let mut mine = db::release_choices(&state.pool, user.0, kind, tmdb_id).await?;
    let synthetic_keys: Vec<String> = mine
        .iter()
        .filter(|c| !kino_core::is_infohash(&c.release.infohash))
        .map(|c| c.release.infohash.clone())
        .collect();
    if !synthetic_keys.is_empty() {
        let known = torrent::known_infohashes(&state.pool, &synthetic_keys).await?;
        for c in &mut mine {
            if let Some(h) = known.get(&c.release.infohash) {
                c.release.infohash = h.clone();
            }
        }
    }
    let device_kind = if req.is_mobile { "mobile" } else { "desktop" };
    let dev_override = db::device_quality_for(&state.pool, user.0, kind, tmdb_id, season, episode, device_kind).await?;
    let prefs = userdata::get_prefs(&state.pool, user.0).await?;
    let dev_quality_newest = dev_override.as_ref().map(|(q, _)| normalize_quality_key(q));
    let target = TargetQuality::from_prefs(&prefs, dev_override.as_ref().map(|(q, _)| q.clone()), req.is_mobile);

    // Tier 1: Ready preloaded download on server disk beats every older manual/peer/profile choice.
    let ready_preloads = userdata::ready_preloads_for_title(&state.pool, kind, tmdb_id).await?;
    let best_ready = ready_preloads.iter().find(|p| p.covers(kind, season, episode));

    let mut preload_mismatch: Option<(ReleaseChoiceUpdate, String)> = None;
    if let Some(rp) = best_ready {
        let ready_qk = rp
            .title
            .as_deref()
            .map(torrent::title::parse_title)
            .map(|p| normalize_quality_key(&p.quality))
            .unwrap_or("");
        let overridden_after = mine.iter().any(|c| {
            c.manual
                && c.chosen_at > rp.ready_at
                && c.release.infohash != rp.infohash
                && (kind == MediaKind::Movie
                    || (c.release.season == season && c.release.episode == episode)
                    || (c.release.season == season && c.release.episode == 0)
                    || (c.release.season == 0 && c.release.episode == 0))
        }) || dev_override
            .as_ref()
            .is_some_and(|(dq, ts)| *ts > rp.ready_at && normalize_quality_key(dq) != ready_qk);
        if !overridden_after {
            let parsed = rp.title.as_deref().map(torrent::title::parse_title);
            let q = parsed.as_ref().map(|p| p.quality.clone());
            let qk = q.as_deref().map(|v| normalize_quality_key(v).to_owned());
            let voice = parsed.as_ref().map(|p| p.voice.clone()).filter(|v| !v.is_empty());
            let preloaded_choice = ReleaseChoiceUpdate {
                kind,
                tmdb_id,
                season: if kind == MediaKind::Movie {
                    0
                } else if rp.whole_torrent {
                    rp.season
                } else {
                    season
                },
                episode: if kind == MediaKind::Movie || rp.whole_torrent {
                    0
                } else {
                    episode
                },
                infohash: rp.infohash.clone(),
                title: rp.title.clone(),
                quality: q,
                quality_key: qk,
                voice,
            };
            let matches_target = ready_qk.is_empty() || target == TargetQuality::Ask || target.matches_key(ready_qk);
            if matches_target {
                return Ok(Json(ResolveReleaseResponse::Ready {
                    choice: preloaded_choice,
                    from_peer: None,
                }));
            }
            preload_mismatch = Some((preloaded_choice, ready_qk.to_owned()));
        }
    }

    let fallback = resolve_online_tiers(
        &state,
        user.0,
        kind,
        tmdb_id,
        season,
        episode,
        req.is_mobile,
        mine,
        &dev_override,
        dev_quality_newest,
        &prefs,
        &target,
    )
    .await?;

    if let Some((preloaded, preloaded_quality)) = preload_mismatch {
        if let ResolveReleaseResponse::Ready { ref choice, .. } = fallback {
            if choice.infohash == preloaded.infohash {
                return Ok(Json(fallback));
            }
        }
        return Ok(Json(ResolveReleaseResponse::PreloadQualityMismatch {
            preloaded,
            preloaded_quality,
            target_quality: target.label(),
            fallback: Box::new(fallback),
        }));
    }

    Ok(Json(fallback))
}

fn choice_covers_episode(c: &ReleaseChoice, kind: MediaKind, episode: i32) -> bool {
    if kind == MediaKind::Movie || episode <= 0 || c.release.episode > 0 {
        return true;
    }
    let Some(title) = c.release.title.as_deref() else {
        return true;
    };
    match torrent::title::parse_title(title).episode_range {
        Some((a, b)) => (a..=b).contains(&episode),
        None => true,
    }
}

#[allow(clippy::too_many_arguments)]
async fn resolve_online_tiers(
    state: &AppState,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    is_mobile: bool,
    mine: Vec<ReleaseChoice>,
    dev_override: &Option<(String, chrono::DateTime<chrono::Utc>)>,
    dev_quality_newest: Option<&str>,
    prefs: &UserPrefs,
    target: &TargetQuality,
) -> Result<ResolveReleaseResponse, AppError> {
    // Tier 2: user's own manual choices (`manual = TRUE`), beating profile/default quality.
    // Only if the user switched device quality inside the player strictly *after* their newest manual pick
    // do we filter manual picks by that explicit per-title device quality override.
    let mine_manual: Vec<ReleaseChoice> = mine
        .into_iter()
        .filter(|c| c.manual && choice_covers_episode(c, kind, episode))
        .collect();
    let newest_manual = find_any_scoped_choice(&mine_manual, season, episode);
    let dev_switch_is_newer = match (dev_override, newest_manual) {
        (Some((dq, dev_ts)), Some(c)) => *dev_ts > c.chosen_at && normalize_quality_key(dq) != choice_quality_key(c),
        (Some(_), None) => true,
        (None, _) => false,
    };
    if dev_switch_is_newer {
        if let Some(dq) = dev_quality_newest {
            if let Some(c) = find_scoped_choice(&mine_manual, season, episode, |k| k == dq) {
                return Ok(ResolveReleaseResponse::Ready {
                    choice: c.release.clone(),
                    from_peer: None,
                });
            }
        }
    } else if let Some(c) = newest_manual {
        return Ok(ResolveReleaseResponse::Ready {
            choice: c.release.clone(),
            from_peer: None,
        });
    }

    // Tier 3: other users' manual choices (most picked first, then newest).
    // Beats profile quality settings; respects an explicit per-title player quality override if set.
    let shared: Vec<ReleaseChoice> = db::shared_release_choices(&state.pool, user_id, kind, tmdb_id)
        .await?
        .into_iter()
        .filter(|c| choice_covers_episode(c, kind, episode))
        .collect();
    let shared_pick = if let Some(dq) = dev_quality_newest {
        find_scoped_choice(&shared, season, episode, |k| k == dq)
    } else {
        find_scoped_choice(&shared, season, episode, |k| target.matches_key(k))
            .or_else(|| find_any_scoped_choice(&shared, season, episode))
    };
    if let Some(c) = shared_pick {
        let mut adopted = c.release.clone();
        if kind == MediaKind::Movie {
            adopted.season = 0;
            adopted.episode = 0;
        }
        return Ok(ResolveReleaseResponse::Ready {
            choice: adopted,
            from_peer: c.picked_by.clone(),
        });
    }

    // Tier 4: profile / device default quality (lowest priority).
    let details = state.catalog.details(kind, tmdb_id).await?;
    if *target == TargetQuality::Ask {
        let list = fetch_release_list(state, &details, (kind == MediaKind::Tv).then_some(season)).await?;
        return Ok(ResolveReleaseResponse::NeedsPicker {
            target_quality: String::new(),
            releases: list,
        });
    }

    let user_preloads = userdata::title_preloads(&state.pool, user_id, kind, tmdb_id).await?;
    let preload_voice = user_preloads
        .iter()
        .filter(|p| kind == MediaKind::Movie || p.season == 0 || p.season == season)
        .find_map(|p| {
            let v = p.title.as_deref().map(torrent::title::parse_title)?.voice;
            (!v.is_empty()).then_some((v, p.requested_at))
        });
    let manual_voice = mine_manual.first().and_then(|c| {
        c.release
            .voice
            .clone()
            .filter(|v| !v.is_empty())
            .map(|v| (v, c.chosen_at))
    });
    let preferred_voice = match (preload_voice, manual_voice) {
        (Some((pv, pts)), Some((mv, mts))) => Some(if pts >= mts { pv } else { mv }),
        (Some((pv, _)), None) => Some(pv),
        (None, Some((mv, _))) => Some(mv),
        (None, None) => shared.first().and_then(|c| c.release.voice.clone()),
    };
    let list = fetch_release_list(state, &details, (kind == MediaKind::Tv).then_some(season)).await?;

    let dead_hashes = state.media.dead_hashes();
    let picked = auto_pick_release(
        &list.releases,
        kind,
        season,
        episode,
        target,
        preferred_voice.as_deref(),
        prefs.language.as_deref(),
        prefs.max_bitrate_mbps,
        is_mobile,
        &dead_hashes,
    );
    match picked {
        Some(best) => {
            let save_season = if kind == MediaKind::Movie || best.season.is_none() {
                0
            } else {
                season
            };
            let save_ep = if kind == MediaKind::Tv && episode > 0 && best.episode_range().is_some_and(|(a, b)| a == b) {
                episode
            } else {
                0
            };
            let choice = best.to_scoped_choice(&details.summary, save_season, save_ep);
            Ok(ResolveReleaseResponse::Ready {
                choice,
                from_peer: None,
            })
        }
        None => Ok(ResolveReleaseResponse::NeedsPicker {
            target_quality: target.label(),
            releases: list,
        }),
    }
}

/// Starts the release search for a title in the background, so the list is ready
/// by the time someone presses "Смотреть". Skips titles where the user already has
/// a saved release choice (so "Продолжить просмотр" does not wake a sleeping indexer).
pub fn prefetch_releases(state: &AppState, user_id: i64, details: TitleDetails) {
    if !state.media.has_indexers() {
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
        if let Err(e) = torrent::search_cached(
            &state.media.searches,
            &state.pool,
            &state.media.indexers,
            &state.media.fallback_indexers,
            &details,
        )
        .await
        {
            tracing::debug!(error = %e, "background release search failed");
        }
    });
}

/// How long a stream link works.
const STREAM_TTL: chrono::Duration = chrono::Duration::hours(12);

fn unavailable(msg: &str) -> AppError {
    AppError::new(StatusCode::SERVICE_UNAVAILABLE, "player_unavailable", msg)
}

fn is_hash_in_use(state: &AppState, hash: &str) -> bool {
    state.sync.has_playing_hash(hash) || state.media.pause_cache.has_active_stream_for_hash(hash)
}

async fn drop_torrserver_if_idle(state: &AppState, ts: &TorrServer, hash: &str, only_if_zero_progress: bool) {
    if is_hash_in_use(state, hash) {
        return;
    }
    if only_if_zero_progress {
        let dead = match ts.progress(hash).await {
            Ok(p) => p.loaded_bytes == 0 && p.speed_bps == 0,
            Err(_) => true,
        };
        if !dead {
            return;
        }
    }
    ts.drop_torrent(hash).await;
}

fn spawn_warm_up(state: &AppState, ts: &TorrServer, hash: &str, index: i32) {
    let st = state.clone();
    let ts = ts.clone();
    let hash = hash.to_owned();
    tokio::spawn(async move {
        let completed = ts.preload_once(&hash, index, std::time::Duration::from_secs(90)).await;
        if !completed {
            drop_torrserver_if_idle(&st, &ts, &hash, true).await;
        }
    });
}

/// Cleans up a dead/zero-byte torrent from TorrServer if the caller cancels `/api/play` mid-connect.
struct DeadPlayGuard {
    armed: bool,
    state: AppState,
    ts: TorrServer,
    hash: String,
}

impl DeadPlayGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DeadPlayGuard {
    fn drop(&mut self) {
        if !self.armed || self.hash.is_empty() {
            return;
        }
        let state = self.state.clone();
        let ts = self.ts.clone();
        let hash = std::mem::take(&mut self.hash);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(90)).await;
            drop_torrserver_if_idle(&state, &ts, &hash, true).await;
        });
    }
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
    let src = torrent::resolve(&state.pool, &media.http, &key, &media.all_indexers()).await?;
    let hash = src.infohash.clone().ok_or(TorrentError::Unknown)?;
    torrent::rebind_synthetic_hash(&state.pool, &key, &hash).await;
    let episode = (req.kind == MediaKind::Tv).then_some((req.season.max(1), req.episode.max(1)));
    let (season, ep) = episode.unwrap_or((0, 0));
    let progress = db::title_progress(&state.pool, user.0, req.kind, req.tmdb_id).await?;
    let start_at = progress
        .iter()
        .find(|p| p.season == season && p.episode == ep && !p.finished)
        .map(|p| p.position_sec as i32)
        .unwrap_or(0);

    // A release downloaded in advance is played from disk; otherwise through pause-cache / TorrServer.
    let local = preloaded_files(&state, &hash, episode).await;
    let ((file, probe, preloaded), torrent_ts) = match local {
        Some((list, dir)) => {
            let f = files::pick(&list, episode)
                .cloned()
                .ok_or_else(|| missing_file(&list, episode))?;
            let path = safe_join_canonical(&dir, &f.path)
                .await
                .ok_or_else(|| missing_file(&list, episode))?;
            let input = path.to_string_lossy().into_owned();
            let probe = ffmpeg::probe(&media.ffprobe, &input).await.unwrap_or_else(|e| {
                tracing::warn!(error = %e, "ffprobe failed, playing the file as it is");
                Probe::default()
            });
            ((f, probe, true), None)
        }
        None => {
            let ts = media
                .torrserver
                .as_ref()
                .ok_or_else(|| unavailable("Потоковый сервер не настроен"))?;
            if let Some(hit) = media.pause_cache.lookup_for_play(user.0, &hash, season, ep, start_at) {
                media.pause_cache.trigger_background_warmup(
                    ts.clone(),
                    src.clone(),
                    hit.file.index,
                    hit.resume_window_end,
                    hit.file.length,
                );
                media.pause_cache.remember_probe(&hash, hit.file.index, &hit.probe);
                ((hit.file, hit.probe, false), Some(ts.clone()))
            } else {
                let mut dead_guard = DeadPlayGuard {
                    armed: true,
                    state: state.clone(),
                    ts: ts.clone(),
                    hash: hash.clone(),
                };
                let list = match ts.add(&src).await {
                    Ok(l) => l,
                    Err(e) => {
                        dead_guard.disarm();
                        tracing::warn!(error = %e, "TorrServer failed");
                        return Err(unavailable("Потоковый сервер не отвечает"));
                    }
                };
                if list.is_empty() {
                    dead_guard.disarm();
                    media.mark_dead_release(&hash);
                    drop_torrserver_if_idle(&state, ts, &hash, false).await;
                    return Err(unavailable(
                        "Нет раздающих: не удалось получить список файлов. Попробуйте «Скачать заранее».",
                    ));
                }
                let f = match files::pick(&list, episode).cloned() {
                    Some(f) => f,
                    None => {
                        dead_guard.disarm();
                        drop_torrserver_if_idle(&state, ts, &hash, true).await;
                        return Err(missing_file(&list, episode));
                    }
                };
                let probe = if let Some(cached) = media.pause_cache.lookup_probe(&hash, f.index) {
                    if start_at <= 15 {
                        spawn_warm_up(&state, ts, &hash, f.index);
                    }
                    cached
                } else {
                    // Load the start of the file now, so the player can show how far it got.
                    spawn_warm_up(&state, ts, &hash, f.index);
                    let input = ts.stream_url_for_ffmpeg(&hash, f.index);
                    let probed = match ffmpeg::probe(&media.ffprobe, &input).await {
                        Ok(p) => p,
                        Err(e) => {
                            let stalled = match ts.progress(&hash).await {
                                Ok(p) => p.peers == 0 && p.loaded_bytes == 0 && p.speed_bps == 0,
                                Err(_) => true,
                            };
                            if stalled {
                                dead_guard.disarm();
                                media.mark_dead_release(&hash);
                                drop_torrserver_if_idle(&state, ts, &hash, true).await;
                                return Err(unavailable(
                                    "Нет раздающих: не удалось начать загрузку. Попробуйте другую раздачу или «Скачать заранее».",
                                ));
                            }
                            tracing::warn!(error = %e, "ffprobe failed, playing the file as it is");
                            Probe::default()
                        }
                    };
                    media.pause_cache.remember_probe(&hash, f.index, &probed);
                    probed
                };
                dead_guard.disarm();
                media.clear_dead_release(&hash);
                ((f, probe, false), Some(ts.clone()))
            }
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

    let (audio_track, subtitle_track) =
        preferred_tracks(&state, user.0, req.kind, req.tmdb_id, &probe, preloaded, req.native).await?;
    let stream_start_at = if torrent_ts.is_some() && start_at > 0 && !req.native {
        media.pause_cache.lookup_ready_stream_start(
            user.0,
            &hash,
            file.index,
            audio_track.unwrap_or(0),
            start_at as u64,
            probe.video_copyable(),
            media.hw,
        )
    } else {
        None
    };
    if let Some(ts) = torrent_ts {
        media.pause_cache.register_play(
            user.0,
            req.kind,
            req.tmdb_id,
            season,
            ep,
            src,
            file.clone(),
            probe.clone(),
            f64::from(start_at),
            audio_track.unwrap_or(0),
            media.hw,
            ts,
            media.ffmpeg.clone(),
        );
    }
    let next = match episode {
        Some(e) => next_episode(&state.catalog.details(MediaKind::Tv, req.tmdb_id).await?, e),
        None => None,
    };
    let segments =
        crate::segments::load_segments_for_play(&state.pool, req.kind, req.tmdb_id, season, ep, &hash, probe.duration)
            .await;
    if preloaded && req.kind == MediaKind::Tv {
        media.segment_notify.notify_one();
    }

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
        stream_start_at,
        segments,
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
    if !preloaded {
        state.media.pause_cache.remember_probe(&c.h, c.i, &probe);
    }
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

#[derive(Deserialize)]
struct SegmentsQuery {
    #[serde(default)]
    season: i32,
    #[serde(default)]
    episode: i32,
    #[serde(default)]
    release: String,
    duration: Option<f64>,
}

/// Returns intro / recap / credits segments from PostgreSQL, fetching IntroDB/AniSkip
/// on the first lookup and caching them for all users.
async fn episode_segments(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path((kind_raw, tmdb_id)): Path<(String, i64)>,
    Query(q): Query<SegmentsQuery>,
) -> Result<Json<Vec<kino_core::MediaSegment>>, AppError> {
    let kind = parse_kind(&kind_raw)?;
    if tmdb_id <= 0 || q.season < 0 || q.episode < 0 {
        return Err(AppError::bad_request("invalid title or episode coordinates"));
    }
    let segs = crate::segments::resolve_or_fetch_segments(
        &state,
        kind,
        tmdb_id,
        q.season,
        q.episode,
        &q.release,
        q.duration.filter(|d| d.is_finite() && *d > 0.0),
    )
    .await;
    Ok(Json(segs))
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

fn missing_file(list: &[FileEntry], episode: Option<(i32, i32)>) -> AppError {
    match episode {
        Some((s, e)) => {
            let avail = files::available_episodes(list, s);
            let msg = if avail.is_empty() {
                format!("Серии {} нет в этой раздаче.", kino_core::episode_code(s, e))
            } else {
                format!(
                    "Серии {} нет в этой раздаче. В ней доступны: {}.",
                    kino_core::episode_code(s, e),
                    kino_core::format_episode_ranges(&avail)
                )
            };
            AppError::missing_episode(msg, avail)
        }
        None => AppError::new(StatusCode::NOT_FOUND, "no_file", "В раздаче нет видеофайла"),
    }
}

/// Files of any finished preload on server disk, with the folder they are in.
async fn preloaded_files(
    state: &AppState,
    hash: &str,
    episode: Option<(i32, i32)>,
) -> Option<(Vec<FileEntry>, PathBuf)> {
    let (qbit, dir) = (state.media.qbit.as_ref()?, state.media.preload_dir.clone()?);
    let ready: bool = match episode {
        None => sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM preload WHERE infohash = $1 AND state = 'ready')")
            .bind(hash)
            .fetch_one(&state.pool)
            .await
            .ok()?,
        Some((s, e)) => sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM preload p
                 WHERE p.infohash = $1 AND p.state <> 'deleted' AND (
                     EXISTS (
                         SELECT 1 FROM preload_episode pe
                         WHERE pe.preload_id = p.id AND pe.season = $2 AND pe.episode = $3 AND pe.state = 'ready'
                     )
                     OR (
                         p.whole_torrent = TRUE AND p.state = 'ready'
                         AND NOT EXISTS (
                             SELECT 1 FROM preload_episode pe
                             WHERE pe.preload_id = p.id AND pe.season = $2 AND pe.episode = $3 AND pe.state = 'deleted'
                         )
                     )
                 )
             )",
        )
        .bind(hash)
        .bind(s)
        .bind(e)
        .fetch_one(&state.pool)
        .await
        .ok()?,
    };
    if !ready {
        return None;
    }
    match qbit.file_states(hash).await {
        Ok(qfiles) => {
            let list: Vec<FileEntry> = qfiles
                .into_iter()
                .filter(|f| episode.is_none() || f.priority > 0)
                .map(|f| f.as_file_entry())
                .collect();
            if list.is_empty() {
                None
            } else {
                Some((list, dir))
            }
        }
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
        let start = q.start.unwrap_or(0);
        let pause_cache = media.pause_cache.clone();
        let cache_key = torrent::pause_cache::CacheKey::new(c.uid, &c.h, c.i);
        pause_cache.on_stream_audio(&cache_key, audio);

        // A seek drops the previous request (and its ffmpeg) first, so wait a moment for its slot.
        let permit = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            media.transcodes.clone().acquire_owned(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or_else(|| unavailable("Сервер уже перекодирует другие видео, попробуйте позже"))?;

        if c.p.is_none() {
            if let Some(ready) = pause_cache
                .lookup_ready_transcode(c.uid, &c.h, c.i, audio, start, c.v, media.hw)
                .await
            {
                return Ok(stream_ready_with_spliced_continuation(
                    &media.ffmpeg,
                    input,
                    audio,
                    ready,
                    pause_cache,
                    cache_key,
                    permit,
                ));
            }
        }

        let args = ffmpeg::transcode_args(&input, start, audio, c.v, media.hw);
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
        let first = match tokio::time::timeout(std::time::Duration::from_secs(55), reader.next()).await {
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
            if !send_with_idle_capture(&tx, Ok(first), &pause_cache, &cache_key).await {
                return;
            }
            loop {
                let next_chunk = tokio::select! {
                    biased;
                    _ = tx.closed() => break,
                    chunk = reader.next() => chunk,
                };
                let Some(chunk) = next_chunk else { break };
                if !send_with_idle_capture(&tx, chunk, &pause_cache, &cache_key).await {
                    break;
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
    Ok(media
        .pause_cache
        .serve_stream(ts, c.uid, &c.h, c.i, req.method(), req.headers())
        .await)
}

/// Sends a streamed chunk to the HTTP response channel, triggering SSD pause-cache capture
/// if client reading stalls briefly and closing the stream cleanly after `TRANSCODE_IDLE_TIMEOUT`.
async fn send_with_idle_capture(
    tx: &tokio::sync::mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>,
    item: Result<axum::body::Bytes, std::io::Error>,
    pause_cache: &torrent::pause_cache::PauseCache,
    cache_key: &torrent::pause_cache::CacheKey,
) -> bool {
    let send_fut = tx.send(item);
    tokio::pin!(send_fut);
    match tokio::time::timeout(TRANSCODE_STALL_CAPTURE, &mut send_fut).await {
        Ok(Ok(())) => true,
        Ok(Err(_)) => false,
        Err(_) => {
            pause_cache.on_transcode_stalled(cache_key);
            let rem = TRANSCODE_IDLE_TIMEOUT.saturating_sub(TRANSCODE_STALL_CAPTURE);
            match tokio::time::timeout(rem, send_fut).await {
                Ok(Ok(())) => true,
                Ok(Err(_)) => false,
                Err(_) => {
                    tracing::info!("stopping idle ffmpeg transcode after client stall");
                    false
                }
            }
        }
    }
}

/// Streams a cached pre-transcoded `ready.mp4` prefix immediately from SSD while launching
/// the live continuation `ffmpeg` process in parallel and splicing its `moof`/`mdat` boxes seamlessly.
fn stream_ready_with_spliced_continuation(
    ffmpeg_bin: &str,
    input: String,
    audio: i32,
    ready: torrent::pause_cache::ReadyTranscodeHit,
    pause_cache: torrent::pause_cache::PauseCache,
    cache_key: torrent::pause_cache::CacheKey,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Response {
    let args = ffmpeg::transcode_args_seek(&input, Some(&ready.cont_seek_str), audio, ready.copy_video, ready.hw);
    let child_res = ffmpeg::spawn(ffmpeg_bin, &args);
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        let _permit = permit;
        let mut child = match child_res {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "ready continuation ffmpeg spawn failed");
                let _ = stream_ready_file(&ready.ready_path, &tx, &pause_cache, &cache_key).await;
                return;
            }
        };
        let out = child.stdout.take();
        let stderr_handle = child.stderr.take().map(|mut err| {
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut buf = Vec::new();
                let mut limited = (&mut err).take(4096);
                let _ = limited.read_to_end(&mut buf).await;
                String::from_utf8_lossy(&buf).trim().to_owned()
            })
        });

        if !stream_ready_file(&ready.ready_path, &tx, &pause_cache, &cache_key).await {
            return;
        }
        let Some(out) = out else { return };
        let mut reader = tokio_util::io::ReaderStream::new(out);
        let mut boxes = torrent::fmp4::BoxStreamReader::default();
        let mut moov_checked = false;
        let mut matched = ready.split_idr_hash.is_none();
        let mut skipped_frags = 0usize;
        let mut pending_moof: Option<Vec<u8>> = None;
        let mut next_seq = ready.last_seq;
        let mut dts_deltas: std::collections::HashMap<u32, i128> = std::collections::HashMap::new();

        'outer: loop {
            let next_chunk = tokio::select! {
                biased;
                _ = tx.closed() => break 'outer,
                chunk = reader.next() => chunk,
            };
            let Some(Ok(chunk)) = next_chunk else { break 'outer };
            boxes.push(&chunk);
            loop {
                let (typ, bx) = match boxes.next_box() {
                    Ok(Some(v)) => v,
                    Ok(None) => break,
                    Err(_) => break 'outer,
                };
                match &typ {
                    b"moov" => {
                        let tracks = torrent::fmp4::extract_tracks(&bx);
                        if tracks != ready.tracks {
                            tracing::debug!("ready splice moov mismatch; letting client cleanly reload");
                            break 'outer;
                        }
                        moov_checked = true;
                    }
                    b"moof" => {
                        if !moov_checked {
                            break 'outer;
                        }
                        pending_moof = Some(bx);
                    }
                    b"mdat" => {
                        let Some(mut moof) = pending_moof.take() else { continue };
                        if !matched {
                            if let Some(ref target_hash) = ready.split_idr_hash {
                                if torrent::fmp4::first_sample_sha256(&moof, &bx, 1).as_ref() == Some(target_hash) {
                                    matched = true;
                                } else {
                                    skipped_frags += 1;
                                    if skipped_frags > 24 {
                                        tracing::debug!("ready splice IDR marker not found; letting client reload");
                                        break 'outer;
                                    }
                                    continue;
                                }
                            } else {
                                matched = true;
                            }
                        }
                        let Ok(info) = torrent::fmp4::parse_moof(&moof) else {
                            break 'outer;
                        };
                        for t in &info.tracks {
                            dts_deltas.entry(t.track_id).or_insert_with(|| {
                                let target_end = ready.track_ends.get(&t.track_id).copied().unwrap_or(0);
                                i128::from(target_end) - i128::from(t.base_dts)
                            });
                        }
                        next_seq = next_seq.saturating_add(1);
                        if torrent::fmp4::patch_moof(&mut moof, next_seq, &dts_deltas).is_err() {
                            break 'outer;
                        }
                        if !send_with_idle_capture(&tx, Ok(axum::body::Bytes::from(moof)), &pause_cache, &cache_key)
                            .await
                        {
                            break 'outer;
                        }
                        if !send_with_idle_capture(&tx, Ok(axum::body::Bytes::from(bx)), &pause_cache, &cache_key).await
                        {
                            break 'outer;
                        }
                    }
                    _ => {}
                }
            }
        }
        drop(child);
        if !matched || !moov_checked {
            if let Some(h) = stderr_handle {
                if let Ok(Ok(err)) = tokio::time::timeout(std::time::Duration::from_secs(1), h).await {
                    if !err.is_empty() {
                        tracing::debug!(stderr = %err, "continuation ffmpeg stderr before splice match");
                    }
                }
            }
        }
    });
    let body = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) });
    (
        [(header::CONTENT_TYPE, "video/mp4"), (header::CACHE_CONTROL, "no-store")],
        Body::from_stream(body),
    )
        .into_response()
}

async fn stream_ready_file(
    path: &std::path::Path,
    tx: &tokio::sync::mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>,
    pause_cache: &torrent::pause_cache::PauseCache,
    cache_key: &torrent::pause_cache::CacheKey,
) -> bool {
    use tokio::io::AsyncReadExt;
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return false;
    };
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if tx.is_closed() {
            return false;
        }
        match file.read(&mut buf).await {
            Ok(0) => return true,
            Ok(n) => {
                let chunk = axum::body::Bytes::copy_from_slice(&buf[..n]);
                if !send_with_idle_capture(tx, Ok(chunk), pause_cache, cache_key).await {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
}

/// Where the file is: on disk for a preload, else pause-cache loopback or TorrServer's stream.
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
            if state.media.pause_cache.is_enabled() {
                if let Some(url) = state.media.pause_cache.internal_stream_url(ts, c.uid, &c.h, c.i).await {
                    return Ok(url);
                }
            }
            Ok(ts.stream_url_for_ffmpeg(&c.h, c.i))
        }
    }
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
                preloaded: false,
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
            alternative_titles: Vec::new(),
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

    #[test]
    fn deduplicates_same_hash_and_matching_unresolved_release() {
        let mut r1 = ReleaseOption {
            infohash: "a".repeat(40),
            title: "Spider-Man Brand New Day 2026 WEB-DL 1080p".into(),
            quality: "1080p".into(),
            size_bytes: 10_488_294_037,
            seeders: 120,
            voice: "Дубляж".into(),
            language: "Русский".into(),
            tracker: "Rutor".into(),
            added: "2026-11-25".into(),
            year: Some(2026),
            season: None,
            episode_start: None,
            episode_end: None,
            hdr: false,
            dolby_vision: false,
            subtitles: false,
            direct_play: true,
            codec: Some("H.264".into()),
            bitrate_kbps: None,
            bitrate_approx: false,
            picked_by: vec!["Анна".into()],
            preloaded: false,
            collection: false,
        };
        let mut r2 = r1.clone();
        r2.infohash = format!("u{}", "b".repeat(39));
        r2.seeders = 250;
        r2.tracker = "Kinozal".into();
        r2.picked_by = vec!["Борис".into()];
        let mut r3 = r1.clone();
        r3.infohash = "A".repeat(40);
        r3.preloaded = true;
        r1.seeders = 90;
        let out = dedup_releases(vec![r1, r2, r3]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].infohash, "a".repeat(40));
        assert_eq!(out[0].seeders, 250);
        assert!(out[0].preloaded);
        assert_eq!(out[0].picked_by, vec!["Анна", "Борис"]);
    }

    #[test]
    fn auto_pick_skips_recently_dead_release() {
        let make = |hash: &str, seeders: u32| ReleaseOption {
            infohash: hash.into(),
            title: "Фильм (2024) BDRip 1080p".into(),
            quality: "1080p".into(),
            size_bytes: 5_000_000_000,
            seeders,
            voice: "Дубляж".into(),
            language: "Русский".into(),
            tracker: "RuTracker".into(),
            added: "2026-11-26".into(),
            year: Some(2024),
            season: None,
            episode_start: None,
            episode_end: None,
            hdr: false,
            dolby_vision: false,
            subtitles: false,
            direct_play: true,
            codec: Some("H.264".into()),
            bitrate_kbps: None,
            bitrate_approx: false,
            picked_by: Vec::new(),
            preloaded: false,
            collection: false,
        };
        let top_dead = make(&"1".repeat(40), 877);
        let live = make(&"2".repeat(40), 529);
        let releases = [top_dead.clone(), live.clone()];
        let mut dead = std::collections::HashSet::new();
        dead.insert(top_dead.infohash.clone());
        let picked = auto_pick_release(
            &releases,
            MediaKind::Movie,
            0,
            0,
            &TargetQuality::Exact("1080p".into()),
            None,
            Some("Русский"),
            None,
            false,
            &dead,
        )
        .unwrap();
        assert_eq!(picked.infohash, live.infohash);
    }

    #[test]
    fn auto_pick_respects_max_bitrate_limit() {
        let make = |hash: &str, quality: &str, seeders: u32, bitrate_kbps: Option<u32>| ReleaseOption {
            infohash: hash.into(),
            title: format!("Звёздные войны (1977) {quality}"),
            quality: quality.into(),
            size_bytes: 5_000_000_000,
            seeders,
            voice: "Дубляж".into(),
            language: "Русский".into(),
            tracker: "BitRu".into(),
            added: "2026-11-26".into(),
            year: Some(1977),
            season: None,
            episode_start: None,
            episode_end: None,
            hdr: false,
            dolby_vision: false,
            subtitles: false,
            direct_play: true,
            codec: Some("H.264".into()),
            bitrate_kbps,
            bitrate_approx: true,
            picked_by: Vec::new(),
            preloaded: false,
            collection: false,
        };
        let huge_sd = make(&"1".repeat(40), "SD", 500, Some(458_000));
        let mut collection_1080 = make(&"5".repeat(40), "1080p", 900, None);
        collection_1080.collection = true;
        let normal_720 = make(&"2".repeat(40), "720p", 12, Some(6_500));
        let normal_1080 = make(&"3".repeat(40), "1080p", 80, Some(18_000));
        let huge_1080 = make(&"4".repeat(40), "1080p", 400, Some(109_000));
        let releases = [
            huge_sd,
            collection_1080,
            normal_720.clone(),
            normal_1080.clone(),
            huge_1080,
        ];
        let dead = std::collections::HashSet::new();

        // Mobile auto must skip the 458 Mbit/s SD release and pick 720p within 30 Mbit/s.
        let mobile_pick = auto_pick_release(
            &releases,
            MediaKind::Movie,
            0,
            0,
            &TargetQuality::MobileAuto,
            None,
            Some("Русский"),
            Some(30),
            true,
            &dead,
        )
        .unwrap();
        assert_eq!(mobile_pick.infohash, normal_720.infohash);

        // Desktop 1080p must skip both the 109 Mbit/s release and the 900-seeder collection.
        let desktop_pick = auto_pick_release(
            &releases,
            MediaKind::Movie,
            0,
            0,
            &TargetQuality::Exact("1080p".into()),
            None,
            Some("Русский"),
            Some(30),
            false,
            &dead,
        )
        .unwrap();
        assert_eq!(desktop_pick.infohash, normal_1080.infohash);

        // If every single-movie release in the target bucket exceeds the bitrate cap, fall back to picker (`None`).
        assert!(auto_pick_release(
            &releases,
            MediaKind::Movie,
            0,
            0,
            &TargetQuality::Exact("1080p".into()),
            None,
            Some("Русский"),
            Some(10),
            false,
            &dead,
        )
        .is_none());
    }

    #[test]
    fn auto_pick_skips_partial_season_missing_requested_episode() {
        let mut partial = ReleaseOption {
            infohash: "1".repeat(40),
            title: "Аутсорс / S1E1-7 of 8 (2025) WEB-DL 1080p".into(),
            quality: "1080p".into(),
            size_bytes: 20_000_000_000,
            seeders: 500,
            voice: "Русский".into(),
            language: "Русский".into(),
            tracker: "RuTracker".into(),
            added: "2025-03-01".into(),
            year: Some(2025),
            season: Some(1),
            episode_start: Some(1),
            episode_end: Some(7),
            hdr: false,
            dolby_vision: false,
            subtitles: false,
            direct_play: true,
            codec: Some("H.264".into()),
            bitrate_kbps: None,
            bitrate_approx: false,
            picked_by: Vec::new(),
            preloaded: false,
            collection: false,
        };
        let mut full = partial.clone();
        full.infohash = "2".repeat(40);
        full.title = "Аутсорс (1 сезон: 1-8 серии из 8) / 2024 / WEB-DL (1080p)".into();
        full.seeders = 120;
        full.episode_start = Some(1);
        full.episode_end = Some(8);
        let releases = [partial.clone(), full.clone()];
        let dead = std::collections::HashSet::new();

        // Episode 7 can pick the high-seeder 1-7 pack.
        let ep7 = auto_pick_release(
            &releases,
            MediaKind::Tv,
            1,
            7,
            &TargetQuality::Exact("1080p".into()),
            None,
            Some("Русский"),
            None,
            false,
            &dead,
        )
        .unwrap();
        assert_eq!(ep7.infohash, partial.infohash);

        // Episode 8 must skip 1-7 and pick 1-8.
        let ep8 = auto_pick_release(
            &releases,
            MediaKind::Tv,
            1,
            8,
            &TargetQuality::Exact("1080p".into()),
            None,
            Some("Русский"),
            None,
            false,
            &dead,
        )
        .unwrap();
        assert_eq!(ep8.infohash, full.infohash);

        // No match returns None so caller falls back to NeedsPicker instead of playing a broken file.
        partial.season = Some(1);
        let only_partial = [partial];
        assert!(auto_pick_release(
            &only_partial,
            MediaKind::Tv,
            1,
            8,
            &TargetQuality::Exact("1080p".into()),
            None,
            Some("Русский"),
            None,
            false,
            &dead,
        )
        .is_none());
    }

    #[tokio::test]
    async fn media_puts_extra_torznab_first_and_keeps_jackett_as_fallback() {
        let cfg = TorrentConfig {
            jackett_url: Some("http://jackett:9117".into()),
            jackett_api_key: Some("k".into()),
            jackett_wake_url: Some("http://sablier:10000".into()),
            extra_torznab: vec![crate::config::ExtraTorznabConfig {
                url: "https://jac-red.ru".into(),
                api_key: None,
            }],
            ..TorrentConfig::default()
        };
        let media = Media::new(&cfg);
        assert_eq!(media.indexers.len(), 1);
        assert_eq!(media.fallback_indexers.len(), 1);
        assert!(media.indexers[0].owns_link("https://jac-red.ru/dl/1"));
        assert!(media.fallback_indexers[0].owns_link("http://jackett:9117/dl/1"));
        assert_eq!(media.all_indexers().len(), 2);
    }
}
