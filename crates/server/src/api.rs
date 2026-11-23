//! HTTP routes under `/api`.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{FromRequestParts, Path, State},
    http::{request::Parts, HeaderMap, StatusCode},
    routing::{get, post, put},
    Json, Router,
};

struct IpLimiter {
    window: Duration,
    max_requests: usize,
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl IpLimiter {
    fn new(window: Duration, max_requests: usize) -> Self {
        Self {
            window,
            max_requests,
            hits: Mutex::new(HashMap::new()),
        }
    }

    fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut map = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= 4096 && !map.contains_key(key) {
            map.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < self.window));
            if map.len() >= 4096 {
                map.clear();
            }
        }
        let q = map.entry(key.to_owned()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= self.window) {
            q.pop_front();
        }
        if q.len() >= self.max_requests {
            return false;
        }
        q.push_back(now);
        true
    }
}

static AUTH_LIMITER: LazyLock<IpLimiter> = LazyLock::new(|| IpLimiter::new(Duration::from_secs(60), 30));
static IMG_LIMITER: LazyLock<IpLimiter> = LazyLock::new(|| IpLimiter::new(Duration::from_secs(60), 600));

fn client_key(headers: &HeaderMap) -> String {
    headers
        .get("x-real-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .unwrap_or("direct")
        .to_owned()
}
use chrono::{FixedOffset, Utc};
use kino_core::{
    AuthResponse, CalendarEntry, CatalogRow, ContinueItem, EpisodeRatingUpdate, HistoryEntry, HomeFeed, LoginLink,
    LoginLinkRequest, LoginLinkStatus, MarkWatched, Me, MediaKind, PersonDetails, PreloadItem, PreloadRequest,
    Progress, ProgressUpdate, RatingUpdate, ReleaseChoice, ReleaseChoiceUpdate, Role, ScaleUpdate, SearchResults,
    Season, SeasonRatingUpdate, ShelfPage, StatusUpdate, TitleDetails, TitleExtras, TitleState, TitleStatus,
    TitleSummary, TracksUpdate, UserPrefs, UserPublicProfile, UserSocialLibrary, WatchlistItem, WebAppAuthRequest,
    WidgetAuthRequest,
};
use serde::{Deserialize, Serialize};

use crate::{
    catalog::{personalize, Catalog, CatalogError},
    config::Config,
    db::{self, LoginOutcome, Pool},
    error::AppError,
    session, social,
    telegram_auth::{self, TelegramUser},
    userdata,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: Pool,
    pub config: Arc<Config>,
    pub catalog: Arc<dyn Catalog>,
    pub media: Arc<crate::play::Media>,
    pub sync: Arc<crate::sync::SyncHub>,
    pub telegram: reqwest::Client,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/public-config", get(public_config))
        .route("/auth/webapp", post(auth_webapp))
        .route("/auth/widget", post(auth_widget))
        .route("/auth/link", post(start_login_link))
        .route("/auth/link/{token}", get(poll_login_link))
        .route("/me", get(me))
        .route("/continue", get(continue_watching))
        .route("/progress", put(save_progress))
        .route("/progress/{kind}/{tmdb_id}", get(title_progress))
        .route("/release-choice", put(save_release_choice))
        .route("/release-choice/{kind}/{tmdb_id}", get(release_choices))
        .route("/title/{kind}/{tmdb_id}/state", get(title_state))
        .route("/title/{kind}/{tmdb_id}/status", put(put_status))
        .route("/title/{kind}/{tmdb_id}/rating", put(put_rating))
        .route("/title/tv/{tmdb_id}/season/{season}/rating", put(put_season_rating))
        .route(
            "/title/tv/{tmdb_id}/season/{season}/episode/{episode}/rating",
            put(put_episode_rating),
        )
        .route("/title/{kind}/{tmdb_id}/tracks", put(put_tracks))
        .route("/title/{kind}/{tmdb_id}/scale", put(put_scale))
        .route("/my", get(my_titles))
        .route("/calendar", get(calendar))
        .route("/users", get(list_users))
        .route("/users/{id}/titles", get(user_titles))
        .route("/marks", post(post_marks))
        .route("/history", get(history))
        .route("/prefs", get(get_prefs).put(put_prefs))
        .route("/preload", get(preloads).post(add_preload))
        .route("/preload/{id}", axum::routing::delete(delete_preload))
        .route("/watchlist", get(watchlist))
        .route(
            "/watchlist/{kind}/{tmdb_id}",
            put(watchlist_add).delete(watchlist_remove),
        )
        .route("/invites", post(create_invite))
        .route("/catalog/home", get(catalog_home))
        .route("/catalog/home/shelves", get(catalog_home_shelves))
        .route("/catalog/shelf/{id}", get(catalog_shelf))
        .route("/catalog/search", get(catalog_search))
        .route("/catalog/{kind}/{tmdb_id}", get(catalog_details))
        .route("/catalog/{kind}/{tmdb_id}/extras", get(catalog_extras))
        .route("/person/{id}", get(person))
        .route("/catalog/tv/{tmdb_id}/season/{number}", get(catalog_season))
        .route("/img/{size}/{file}", get(image))
        .route("/avatar/{token}", get(crate::avatar::serve))
        .route("/sync/join", post(crate::sync::join))
        .route("/sync/leave", post(crate::sync::leave))
        .route("/sync/ws", get(crate::sync::ws))
        .merge(crate::play::routes())
}

/// The logged-in user, from `Authorization: Bearer <token>`.
pub struct CurrentUser(pub i64);

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::unauthorized("missing Authorization header"))?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(|| AppError::unauthorized("expected a Bearer token"))?;
        session::verify(&state.config.session_secret, token, Utc::now())
            .map(CurrentUser)
            .ok_or_else(|| AppError::unauthorized("invalid or expired session"))
    }
}

async fn finish_login(
    state: &AppState,
    tg: TelegramUser,
    invite: Option<String>,
) -> Result<Json<AuthResponse>, AppError> {
    match db::login(&state.pool, &tg, invite.as_deref(), &state.config.admin_telegram_ids).await? {
        LoginOutcome::LoggedIn(mut me) => {
            crate::avatar::attach_to_me(state, &mut me, tg.photo_url.is_some()).await;
            let expires_at = Utc::now() + state.config.session_ttl;
            let token = session::issue(&state.config.session_secret, me.id, expires_at);
            Ok(Json(AuthResponse { token, expires_at, me }))
        }
        LoginOutcome::NotInvited => Err(AppError::forbidden(
            "not_invited",
            "Доступ только по приглашению. Попросите ссылку у администратора.",
        )),
    }
}

fn auth_failed(e: telegram_auth::AuthError) -> AppError {
    tracing::info!(error = %e, "telegram login rejected");
    AppError::unauthorized(format!("Telegram login check failed: {e}"))
}

async fn auth_webapp(
    State(state): State<AppState>,
    Json(req): Json<WebAppAuthRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let cfg = &state.config;
    let tg = telegram_auth::verify_webapp(&req.init_data, &cfg.bot_token, Utc::now(), cfg.auth_max_age)
        .map_err(auth_failed)?;
    finish_login(&state, tg, req.invite).await
}

async fn auth_widget(
    State(state): State<AppState>,
    Json(req): Json<WidgetAuthRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let cfg = &state.config;
    let tg =
        telegram_auth::verify_widget(&req.fields, &cfg.bot_token, Utc::now(), cfg.auth_max_age).map_err(auth_failed)?;
    finish_login(&state, tg, req.invite).await
}

/// Starts logging in through the Telegram app: the site opens `url`, the user
/// presses the button in the bot, and the site's poll gets the session.
async fn start_login_link(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LoginLinkRequest>,
) -> Result<Json<LoginLink>, AppError> {
    if !AUTH_LIMITER.check(&client_key(&headers)) {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Слишком много попыток входа. Попробуйте через минуту.",
        ));
    }
    let Some(bot) = &state.config.bot_username else {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_bot",
            "Вход через Telegram не настроен: не указан BOT_USERNAME.",
        ));
    };
    let token = db::create_login_link(&state.pool, req.invite.as_deref()).await?;
    let url = format!("https://t.me/{bot}?start={}{token}", db::LOGIN_PREFIX);
    Ok(Json(LoginLink { token, url }))
}

async fn poll_login_link(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<LoginLinkStatus>, AppError> {
    if token.len() > 64 || !token.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Ok(Json(LoginLinkStatus::Expired));
    }
    match db::take_login_link(&state.pool, &token).await? {
        db::LoginLinkState::Pending => Ok(Json(LoginLinkStatus::Pending)),
        db::LoginLinkState::Expired => Ok(Json(LoginLinkStatus::Expired)),
        db::LoginLinkState::Confirmed(tg, invite) => {
            let Json(auth) = finish_login(&state, tg, invite).await?;
            Ok(Json(LoginLinkStatus::Done { auth }))
        }
    }
}

async fn load_me(state: &AppState, user: &CurrentUser) -> Result<Me, AppError> {
    db::get_user(&state.pool, user.0)
        .await?
        .ok_or_else(|| AppError::unauthorized("user no longer exists"))
}

async fn me(State(state): State<AppState>, user: CurrentUser) -> Result<Json<Me>, AppError> {
    let mut me = load_me(&state, &user).await?;
    crate::avatar::attach_to_me(&state, &mut me, false).await;
    Ok(Json(me))
}

async fn continue_watching(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<ContinueItem>>, AppError> {
    use futures_util::stream::{self, StreamExt};
    let mut items = db::continue_watching(&state.pool, user.0, 20).await?;
    // Fetch titles concurrently while preserving recency order.
    let keys: Vec<(usize, MediaKind, i64)> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (i, it.last.kind, it.last.tmdb_id))
        .collect();
    let catalog = &state.catalog;
    let summaries = stream::iter(keys.into_iter().map(|(i, kind, id)| async move {
        let title = catalog.details(kind, id).await.ok().map(|d| d.summary);
        (i, title)
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    let mut summaries_vec: Vec<kino_core::TitleSummary> = summaries.iter().filter_map(|(_, t)| t.clone()).collect();
    social::enrich_summaries(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user.0,
        &mut summaries_vec,
    )
    .await;
    let map: HashMap<_, _> = summaries_vec.into_iter().map(|s| ((s.kind, s.tmdb_id), s)).collect();
    for (i, title) in summaries {
        items[i].title = title.map(|t| map.get(&(t.kind, t.tmdb_id)).cloned().unwrap_or(t));
    }
    Ok(Json(items))
}

/// BitTorrent v1 (SHA-1) or v2 (SHA-256) info-hash in hex.
/// An info-hash (v1 or v2), or the placeholder key of a release found by the indexer.
pub(crate) fn infohash_ok(h: &str) -> bool {
    (matches!(h.len(), 40 | 64) && h.bytes().all(|b| b.is_ascii_hexdigit())) || kino_core::is_release_key(h)
}

fn validate(u: &ProgressUpdate) -> Result<(), AppError> {
    let ok_num = |x: f64| x.is_finite() && x >= 0.0;
    if !ok_num(u.position_sec) || !ok_num(u.duration_sec) {
        return Err(AppError::bad_request(
            "position_sec and duration_sec must be non-negative numbers",
        ));
    }
    if u.tmdb_id <= 0 {
        return Err(AppError::bad_request("tmdb_id must be positive"));
    }
    if u.kind == MediaKind::Tv && (u.season < 0 || u.episode < 1) {
        return Err(AppError::bad_request("a series needs season >= 0 and episode >= 1"));
    }
    if let Some(r) = &u.release {
        if !infohash_ok(&r.infohash) {
            return Err(AppError::bad_request(
                "release.infohash must be a 40 or 64 character hex string",
            ));
        }
        if r.file_index < 0 {
            return Err(AppError::bad_request("release.file_index must be >= 0"));
        }
    }
    Ok(())
}

pub async fn persist_progress(state: &AppState, user_id: i64, mut u: ProgressUpdate) -> Result<Progress, AppError> {
    validate(&u)?;
    if let Some(r) = &mut u.release {
        r.infohash.make_ascii_lowercase();
    }
    let p = db::save_progress(&state.pool, user_id, &u).await?;
    userdata::refresh_status(&state.pool, &*state.catalog, user_id, p.kind, p.tmdb_id, true).await?;
    if p.finished {
        after_finished(state, user_id, p.kind, p.tmdb_id, p.season, p.episode).await?;
    }
    Ok(p)
}

async fn save_progress(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(u): Json<ProgressUpdate>,
) -> Result<Json<Progress>, AppError> {
    validate(&u)?;
    if !state.sync.should_accept_http_progress(user.0, &u) {
        let (season, episode) = match u.kind {
            MediaKind::Movie => (0, 0),
            MediaKind::Tv => (u.season, u.episode),
        };
        if let Some(existing) = db::title_progress(&state.pool, user.0, u.kind, u.tmdb_id)
            .await?
            .into_iter()
            .find(|p| p.season == season && p.episode == episode)
        {
            return Ok(Json(existing));
        }
    }
    Ok(Json(persist_progress(&state, user.0, u).await?))
}

/// A film, or the last episode of a season, is watched: its preload goes after a while.
async fn after_finished(
    state: &AppState,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
) -> Result<(), AppError> {
    let season_done = match kind {
        MediaKind::Movie => true,
        MediaKind::Tv => state
            .catalog
            .season(tmdb_id, season)
            .await
            .map(|s| s.episodes.iter().map(|e| e.number).max() == Some(episode))
            .unwrap_or(false),
    };
    if season_done {
        let at = Utc::now() + state.config.preload_keep;
        userdata::preload_schedule_removal(&state.pool, user_id, kind, tmdb_id, season, at).await?;
    }
    Ok(())
}

async fn save_release_choice(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(mut c): Json<ReleaseChoiceUpdate>,
) -> Result<Json<ReleaseChoice>, AppError> {
    if !infohash_ok(&c.infohash) {
        return Err(AppError::bad_request(
            "infohash must be a 40 or 64 character hex string",
        ));
    }
    if c.tmdb_id <= 0
        || c.season < 0
        || c.episode < 0
        || (c.kind == MediaKind::Movie && (c.season != 0 || c.episode != 0))
    {
        return Err(AppError::bad_request(
            "tmdb_id must be positive; season and episode must be 0 for films",
        ));
    }
    c.infohash.make_ascii_lowercase();
    Ok(Json(db::save_release_choice(&state.pool, user.0, &c).await?))
}

async fn release_choices(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
) -> Result<Json<Vec<ReleaseChoice>>, AppError> {
    let kind = parse_kind(&kind)?;
    let mut list = db::release_choices(&state.pool, user.0, kind, tmdb_id).await?;
    let shared = db::shared_release_choices(&state.pool, user.0, kind, tmdb_id).await?;
    for c in shared {
        let exists = list.iter().any(|m| {
            m.release.season == c.release.season
                && m.release.episode == c.release.episode
                && m.release.quality_key == c.release.quality_key
        });
        if !exists {
            list.push(c);
        }
    }
    Ok(Json(list))
}

/// Fills in names and posters; titles the catalog can't resolve stay without them.
async fn summary_of(state: &AppState, kind: MediaKind, tmdb_id: i64) -> Option<kino_core::TitleSummary> {
    state.catalog.details(kind, tmdb_id).await.ok().map(|d| d.summary)
}

async fn title_state(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
) -> Result<Json<TitleState>, AppError> {
    let kind = parse_kind(&kind)?;
    let mut st = userdata::get_state(&state.pool, user.0, kind, tmdb_id).await?;
    st.peers = social::title_peers(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user.0,
        kind,
        tmdb_id,
    )
    .await;
    Ok(Json(st))
}

async fn put_status(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
    Json(u): Json<StatusUpdate>,
) -> Result<Json<TitleState>, AppError> {
    if tmdb_id <= 0 {
        return Err(AppError::bad_request("tmdb_id must be positive"));
    }
    let kind = parse_kind(&kind)?;
    let mut st = userdata::set_status(&state.pool, user.0, kind, tmdb_id, u.status).await?;
    // Marking a film "Просмотрено" by hand also marks it watched for "Continue watching".
    if u.status == Some(TitleStatus::Watched) && kind == MediaKind::Movie {
        let m = MarkWatched {
            kind,
            tmdb_id,
            season: 0,
            episodes: vec![],
            watched: true,
        };
        userdata::mark(&state.pool, user.0, &m).await?;
        st = userdata::get_state(&state.pool, user.0, kind, tmdb_id).await?;
    }
    Ok(Json(st))
}

/// Collects all aired regular-season `(season, episode)` targets, per-season aired counts
/// `(season, aired_count)`, and fully completed seasons for a series.
async fn series_rating_meta(
    catalog: &dyn Catalog,
    tmdb_id: i64,
    details: &TitleDetails,
) -> (Vec<(i32, i32)>, Vec<(i32, i32)>, Vec<i32>) {
    use futures_util::stream::{self, StreamExt};

    let last_aired = details.last_episode.filter(|(s, _)| *s > 0);
    let seasons: Vec<(i32, i32)> = details
        .seasons
        .iter()
        .filter(|s| s.number > 0 && s.episode_count > 0 && last_aired.is_none_or(|(ls, _)| s.number <= ls))
        .map(|s| (s.number, s.episode_count))
        .collect();

    let fetched = stream::iter(seasons.into_iter().map(|(season_num, ep_count)| async move {
        let res = catalog.season(tmdb_id, season_num).await;
        (season_num, ep_count, res)
    }))
    .buffer_unordered(6)
    .collect::<Vec<_>>()
    .await;

    let mut targets = Vec::new();
    let mut season_aired = Vec::new();
    let mut done_seasons = Vec::new();
    for (season_num, ep_count, res) in fetched {
        match res {
            Ok(season) if !season.episodes.is_empty() => {
                let max_ep = season.episodes.iter().map(|e| e.number).max();
                let mut added_max = false;
                let mut aired_here = 0i32;
                for ep in season.episodes {
                    if ep.number >= 1 && last_aired.is_none_or(|last| (season_num, ep.number) <= last) {
                        if Some(ep.number) == max_ep {
                            added_max = true;
                        }
                        aired_here += 1;
                        targets.push((season_num, ep.number));
                    }
                }
                if aired_here > 0 {
                    season_aired.push((season_num, aired_here));
                }
                if added_max {
                    done_seasons.push(season_num);
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    tmdb_id,
                    season = season_num,
                    "falling back to episode_count after catalog.season failed"
                );
                let count = match last_aired {
                    Some((ls, le)) if season_num == ls => ep_count.min(le),
                    _ => ep_count,
                };
                if count > 0 {
                    season_aired.push((season_num, count));
                }
                for ep in 1..=count {
                    targets.push((season_num, ep));
                }
                if count >= ep_count && ep_count > 0 {
                    done_seasons.push(season_num);
                }
            }
        }
    }
    (targets, season_aired, done_seasons)
}

#[cfg(test)]
async fn series_episodes_to_mark(
    catalog: &dyn Catalog,
    tmdb_id: i64,
    details: &TitleDetails,
) -> (Vec<(i32, i32)>, Vec<i32>) {
    let (targets, _, done_seasons) = series_rating_meta(catalog, tmdb_id, details).await;
    (targets, done_seasons)
}

async fn put_rating(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
    Json(u): Json<RatingUpdate>,
) -> Result<Json<TitleState>, AppError> {
    if tmdb_id <= 0 {
        return Err(AppError::bad_request("tmdb_id must be positive"));
    }
    if u.rating.is_some_and(|r| !(1..=5).contains(&r)) {
        return Err(AppError::bad_request("rating must be 1 to 5"));
    }
    let kind = parse_kind(&kind)?;
    let (targets, season_aired, done_seasons, ongoing) = match kind {
        MediaKind::Movie => (
            if u.rating.is_some() { vec![(0, 0)] } else { Vec::new() },
            Vec::new(),
            if u.rating.is_some() { vec![0] } else { Vec::new() },
            false,
        ),
        MediaKind::Tv => match state.catalog.details(kind, tmdb_id).await {
            Ok(details) => {
                let ongoing = details.is_ongoing_series();
                let (targets, sa, done) = series_rating_meta(&*state.catalog, tmdb_id, &details).await;
                (
                    if u.rating.is_some() { targets } else { Vec::new() },
                    sa,
                    if u.rating.is_some() { done } else { Vec::new() },
                    ongoing,
                )
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    tmdb_id,
                    "could not load series details to mark episodes watched on rating"
                );
                (Vec::new(), Vec::new(), Vec::new(), false)
            }
        },
    };
    let mut st = userdata::rate_and_mark_with_meta(
        &state.pool,
        user.0,
        kind,
        tmdb_id,
        u.rating,
        &targets,
        &season_aired,
        ongoing,
    )
    .await?;
    if u.rating.is_some() && !done_seasons.is_empty() {
        let at = Utc::now() + state.config.preload_keep;
        for season in done_seasons {
            userdata::preload_schedule_removal(&state.pool, user.0, kind, tmdb_id, season, at).await?;
        }
    }
    st.peers = social::title_peers(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user.0,
        kind,
        tmdb_id,
    )
    .await;
    Ok(Json(st))
}

async fn put_season_rating(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((tmdb_id, season)): Path<(i64, i32)>,
    Json(u): Json<SeasonRatingUpdate>,
) -> Result<Json<TitleState>, AppError> {
    if tmdb_id <= 0 || season <= 0 {
        return Err(AppError::bad_request("tmdb_id and season must be positive"));
    }
    if u.rating.is_some_and(|r| !(1..=5).contains(&r)) {
        return Err(AppError::bad_request("rating must be 1 to 5"));
    }
    let details = state.catalog.details(MediaKind::Tv, tmdb_id).await?;
    let ongoing = details.is_ongoing_series();
    let (all_targets, season_aired, done_seasons) = series_rating_meta(&*state.catalog, tmdb_id, &details).await;
    let aired_in_season = season_aired
        .iter()
        .find(|(s, _)| *s == season)
        .map(|(_, c)| *c)
        .unwrap_or(0);
    let season_targets: Vec<(i32, i32)> = all_targets.iter().copied().filter(|(s, _)| *s == season).collect();
    let mut st = userdata::rate_season(
        &state.pool,
        user.0,
        tmdb_id,
        season,
        u.rating,
        aired_in_season,
        &season_targets,
        &all_targets,
        &season_aired,
        ongoing,
    )
    .await?;
    if u.rating.is_some() && done_seasons.contains(&season) {
        let at = Utc::now() + state.config.preload_keep;
        userdata::preload_schedule_removal(&state.pool, user.0, MediaKind::Tv, tmdb_id, season, at).await?;
    }
    st.peers = social::title_peers(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user.0,
        MediaKind::Tv,
        tmdb_id,
    )
    .await;
    Ok(Json(st))
}

async fn put_episode_rating(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((tmdb_id, season, episode)): Path<(i64, i32, i32)>,
    Json(u): Json<EpisodeRatingUpdate>,
) -> Result<Json<TitleState>, AppError> {
    if tmdb_id <= 0 || season <= 0 || episode <= 0 {
        return Err(AppError::bad_request("tmdb_id, season and episode must be positive"));
    }
    if u.rating.is_some_and(|r| !(1..=5).contains(&r)) {
        return Err(AppError::bad_request("rating must be 1 to 5"));
    }
    let details = state.catalog.details(MediaKind::Tv, tmdb_id).await?;
    let ongoing = details.is_ongoing_series();
    let (all_targets, season_aired, _) = series_rating_meta(&*state.catalog, tmdb_id, &details).await;
    let mut st = userdata::rate_episode(
        &state.pool,
        user.0,
        tmdb_id,
        season,
        episode,
        u.rating,
        &all_targets,
        &season_aired,
        ongoing,
    )
    .await?;
    st.peers = social::title_peers(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user.0,
        MediaKind::Tv,
        tmdb_id,
    )
    .await;
    Ok(Json(st))
}

async fn put_tracks(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
    Json(u): Json<TracksUpdate>,
) -> Result<Json<TitleState>, AppError> {
    if tmdb_id <= 0 {
        return Err(AppError::bad_request("tmdb_id must be positive"));
    }
    let too_long = |t: &Option<String>| t.as_ref().is_some_and(|t| t.chars().count() > 200);
    if too_long(&u.audio_track) || too_long(&u.subtitle_track) {
        return Err(AppError::bad_request("track names must be at most 200 characters"));
    }
    Ok(Json(
        userdata::set_tracks(
            &state.pool,
            user.0,
            parse_kind(&kind)?,
            tmdb_id,
            u.audio_track,
            u.subtitle_track,
        )
        .await?,
    ))
}

async fn put_scale(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
    Json(u): Json<ScaleUpdate>,
) -> Result<Json<TitleState>, AppError> {
    if tmdb_id <= 0 {
        return Err(AppError::bad_request("tmdb_id must be positive"));
    }
    Ok(Json(
        userdata::set_scale(&state.pool, user.0, parse_kind(&kind)?, tmdb_id, u.video_scale).await?,
    ))
}

#[derive(Deserialize)]
struct MyQuery {
    status: Option<String>,
}

/// "Мои": titles by status, with names and posters.
async fn my_titles(
    State(state): State<AppState>,
    user: CurrentUser,
    axum::extract::Query(q): axum::extract::Query<MyQuery>,
) -> Result<Json<Vec<TitleState>>, AppError> {
    use futures_util::stream::{self, StreamExt};
    let status = match q.status.as_deref() {
        None | Some("") => None,
        Some(s) => Some(TitleStatus::parse(s).ok_or_else(|| AppError::bad_request("unknown status"))?),
    };
    let mut list = userdata::list_states(&state.pool, user.0, status).await?;
    let keys: Vec<(usize, MediaKind, i64)> = list
        .iter()
        .enumerate()
        .map(|(i, it)| (i, it.kind, it.tmdb_id))
        .collect();
    let summaries = stream::iter(keys.into_iter().map(|(i, kind, id)| {
        let state = &state;
        async move { (i, summary_of(state, kind, id).await) }
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    let mut raw_summaries: Vec<TitleSummary> = summaries.iter().filter_map(|(_, t)| t.clone()).collect();
    social::enrich_summaries(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user.0,
        &mut raw_summaries,
    )
    .await;
    let map: HashMap<_, _> = raw_summaries.into_iter().map(|s| ((s.kind, s.tmdb_id), s)).collect();
    for (i, title) in summaries {
        list[i].title = title.map(|t| map.get(&(t.kind, t.tmdb_id)).cloned().unwrap_or(t));
    }
    Ok(Json(list))
}

async fn post_marks(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(m): Json<MarkWatched>,
) -> Result<StatusCode, AppError> {
    if m.tmdb_id <= 0 || (m.kind == MediaKind::Tv && (m.season < 0 || m.episodes.is_empty())) {
        return Err(AppError::bad_request(
            "a series needs a season and at least one episode",
        ));
    }
    if m.episodes.len() > 500 || m.episodes.iter().any(|e| *e < 1) {
        return Err(AppError::bad_request("episodes must be numbers from 1"));
    }
    userdata::mark(&state.pool, user.0, &m).await?;
    userdata::refresh_status(&state.pool, &*state.catalog, user.0, m.kind, m.tmdb_id, false).await?;
    if m.watched {
        let last = m.episodes.iter().copied().max().unwrap_or(0);
        after_finished(&state, user.0, m.kind, m.tmdb_id, m.season, last).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct HistoryQuery {
    limit: Option<i64>,
}

async fn history(
    State(state): State<AppState>,
    user: CurrentUser,
    axum::extract::Query(q): axum::extract::Query<HistoryQuery>,
) -> Result<Json<Vec<HistoryEntry>>, AppError> {
    use futures_util::stream::{self, StreamExt};
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let mut list = userdata::history(&state.pool, user.0, limit).await?;
    let mut unique: Vec<(MediaKind, i64)> = Vec::new();
    for item in &list {
        let key = (item.kind, item.tmdb_id);
        if !unique.contains(&key) {
            unique.push(key);
        }
    }
    let fetched = stream::iter(unique.into_iter().map(|(kind, id)| {
        let state = &state;
        async move { ((kind, id), summary_of(state, kind, id).await) }
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    let cache: HashMap<_, _> = fetched.into_iter().collect();
    let mut raw_summaries: Vec<TitleSummary> = cache.values().flatten().cloned().collect();
    social::enrich_summaries(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user.0,
        &mut raw_summaries,
    )
    .await;
    let enriched: HashMap<_, _> = raw_summaries.into_iter().map(|s| ((s.kind, s.tmdb_id), s)).collect();
    for item in &mut list {
        item.title = cache
            .get(&(item.kind, item.tmdb_id))
            .cloned()
            .flatten()
            .map(|t| enriched.get(&(t.kind, t.tmdb_id)).cloned().unwrap_or(t));
    }
    Ok(Json(list))
}

async fn get_prefs(State(state): State<AppState>, user: CurrentUser) -> Result<Json<UserPrefs>, AppError> {
    Ok(Json(userdata::get_prefs(&state.pool, user.0).await?))
}

async fn put_prefs(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(p): Json<UserPrefs>,
) -> Result<Json<UserPrefs>, AppError> {
    let long = |v: &Option<String>| v.as_ref().is_some_and(|v| v.chars().count() > 100);
    if [
        &p.quality,
        &p.voice,
        &p.subtitle_lang,
        &p.language,
        &p.codec,
        &p.sort,
        &p.desktop_quality,
        &p.mobile_quality,
    ]
    .into_iter()
    .any(long)
    {
        return Err(AppError::bad_request("values must be at most 100 characters"));
    }
    userdata::set_prefs(&state.pool, user.0, &p).await?;
    Ok(Json(p))
}

async fn calendar(State(state): State<AppState>, user: CurrentUser) -> Result<Json<Vec<CalendarEntry>>, AppError> {
    let msk = FixedOffset::east_opt(3 * 3600).expect("UTC+3");
    let today = Utc::now().with_timezone(&msk).format("%Y-%m-%d").to_string();
    Ok(Json(
        social::calendar_entries(
            &state.pool,
            &state.config.session_secret,
            &state.sync,
            &*state.catalog,
            user.0,
            &today,
        )
        .await?,
    ))
}

async fn list_users(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<UserPublicProfile>>, AppError> {
    Ok(Json(
        social::list_users_public(
            &state.pool,
            &state.config.session_secret,
            &state.sync,
            &*state.catalog,
            user.0,
        )
        .await?,
    ))
}

async fn user_titles(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(target_id): Path<i64>,
) -> Result<Json<UserSocialLibrary>, AppError> {
    if target_id <= 0 {
        return Err(AppError::bad_request("user id must be positive"));
    }
    let Some(lib) = social::user_public_library(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        &*state.catalog,
        user.0,
        target_id,
    )
    .await?
    else {
        return Err(AppError::not_found());
    };
    Ok(Json(lib))
}

async fn preloads(State(state): State<AppState>, user: CurrentUser) -> Result<Json<Vec<PreloadItem>>, AppError> {
    Ok(Json(userdata::preloads(&state.pool, user.0).await?))
}

const MAX_ACTIVE_PRELOADS: usize = 10;

async fn add_preload(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(mut r): Json<PreloadRequest>,
) -> Result<Json<PreloadItem>, AppError> {
    if !infohash_ok(&r.infohash) || r.tmdb_id <= 0 || r.season < 0 {
        return Err(AppError::bad_request(
            "need a hex infohash, a positive tmdb_id and season >= 0",
        ));
    }
    r.infohash.make_ascii_lowercase();
    let existing = userdata::preloads(&state.pool, user.0).await?;
    let active_count = existing
        .iter()
        .filter(|p| {
            matches!(
                p.state,
                kino_core::PreloadState::Downloading | kino_core::PreloadState::Ready
            ) && p.infohash != r.infohash
        })
        .count();
    if active_count >= MAX_ACTIVE_PRELOADS {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "preload_limit",
            "Достигнут лимит одновременных загрузок (10). Удалите ненужные в профиле.",
        ));
    }
    let media = &state.media;
    let Some(qbit) = &media.qbit else {
        // Without a downloader the request is only recorded (demo mode).
        return Ok(Json(userdata::preload_add(&state.pool, user.0, &r).await?));
    };
    let src = crate::torrent::resolve(&state.pool, &media.http, &r.infohash, media.jackett.as_ref()).await?;
    r.infohash = src.infohash.clone().unwrap_or(r.infohash);
    if r.title.is_none() && !src.title.is_empty() {
        r.title = Some(src.title.clone());
    }
    qbit.add(&src).await.map_err(|e| {
        tracing::warn!(error = %e, "qBittorrent add failed");
        AppError::new(
            StatusCode::BAD_GATEWAY,
            "downloader_unavailable",
            "Загрузчик сейчас недоступен",
        )
    })?;
    Ok(Json(userdata::preload_add(&state.pool, user.0, &r).await?))
}

async fn delete_preload(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    let Some(hash) = userdata::preload_delete(&state.pool, user.0, id).await? else {
        return Err(AppError::not_found());
    };
    crate::jobs::remove_files(&state, &hash).await;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn parse_kind(kind: &str) -> Result<MediaKind, AppError> {
    MediaKind::parse(kind).ok_or_else(|| AppError::bad_request("kind must be `movie` or `tv`"))
}

async fn title_progress(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
) -> Result<Json<Vec<Progress>>, AppError> {
    Ok(Json(
        db::title_progress(&state.pool, user.0, parse_kind(&kind)?, tmdb_id).await?,
    ))
}

async fn watchlist(State(state): State<AppState>, user: CurrentUser) -> Result<Json<Vec<WatchlistItem>>, AppError> {
    Ok(Json(db::watchlist(&state.pool, user.0).await?))
}

async fn watchlist_add(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
) -> Result<StatusCode, AppError> {
    if tmdb_id <= 0 {
        return Err(AppError::bad_request("tmdb_id must be positive"));
    }
    db::watchlist_add(&state.pool, user.0, parse_kind(&kind)?, tmdb_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn watchlist_remove(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
) -> Result<StatusCode, AppError> {
    if tmdb_id <= 0 {
        return Err(AppError::bad_request("tmdb_id must be positive"));
    }
    if db::watchlist_remove(&state.pool, user.0, parse_kind(&kind)?, tmdb_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found())
    }
}

#[derive(Serialize)]
struct PublicConfig {
    bot_username: Option<String>,
}

/// Settings the client needs before login.
async fn public_config(State(state): State<AppState>) -> Json<PublicConfig> {
    Json(PublicConfig {
        bot_username: state.config.bot_username.clone(),
    })
}

#[derive(Serialize)]
struct InviteResponse {
    code: String,
    /// Link to the website that carries the invite.
    link: String,
}

async fn create_invite(State(state): State<AppState>, user: CurrentUser) -> Result<Json<InviteResponse>, AppError> {
    let me = load_me(&state, &user).await?;
    if me.role != Role::Admin {
        return Err(AppError::forbidden("admin_only", "Only an admin can create invites"));
    }
    let code = db::create_invite(&state.pool, Some(me.id)).await?;
    let link = format!("{}/?invite={code}", state.config.public_url);
    Ok(Json(InviteResponse { code, link }))
}

impl From<CatalogError> for AppError {
    fn from(e: CatalogError) -> Self {
        match e {
            CatalogError::NotFound => AppError::not_found(),
            CatalogError::Upstream(msg) => {
                tracing::warn!(error = %msg, "catalog request failed");
                AppError::new(
                    StatusCode::BAD_GATEWAY,
                    "catalog_unavailable",
                    "Каталог сейчас недоступен",
                )
            }
        }
    }
}

const CUSTOM_SHELF_PAGE_SIZE: usize = 20;

async fn hydrate_keys(state: &AppState, user_id: i64, keys: &[(MediaKind, i64)]) -> Vec<TitleSummary> {
    use futures_util::stream::{self, StreamExt};
    let mut items: Vec<(usize, TitleSummary)> = stream::iter(
        keys.iter()
            .copied()
            .enumerate()
            .map(|(i, (kind, id))| async move { (i, summary_of(state, kind, id).await) }),
    )
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .filter_map(|(i, s)| s.map(|v| (i, v)))
    .collect();
    items.sort_by_key(|(i, _)| *i);
    let mut summaries: Vec<TitleSummary> = items.into_iter().map(|(_, s)| s).collect();
    social::enrich_summaries(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user_id,
        &mut summaries,
    )
    .await;
    summaries
}

async fn rewatch_shelf_row(state: &AppState, user_id: i64, page: u32) -> Result<Option<CatalogRow>, AppError> {
    let favs = userdata::rewatch_favorites(&state.pool, user_id).await?;
    if favs.is_empty() {
        return Ok(None);
    }
    let page = page.max(1);
    let start = ((page - 1) as usize) * CUSTOM_SHELF_PAGE_SIZE;
    if start >= favs.len() {
        return Ok(Some(CatalogRow {
            id: "rewatch".into(),
            title: "Пересматривай любимое".into(),
            subtitle: Some("Завершённые фильмы и сериалы с высокой оценкой".into()),
            categories: vec!["for_you".into(), "rewatch".into()],
            loaded: true,
            has_more: false,
            page,
            items: Vec::new(),
        }));
    }
    let end = (start + CUSTOM_SHELF_PAGE_SIZE).min(favs.len());
    let has_more = end < favs.len();
    let keys: Vec<(MediaKind, i64)> = favs[start..end].iter().map(|(k, id, _)| (*k, *id)).collect();
    let items = hydrate_keys(state, user_id, &keys).await;
    if items.is_empty() && page == 1 {
        return Ok(None);
    }
    Ok(Some(CatalogRow {
        id: "rewatch".into(),
        title: "Пересматривай любимое".into(),
        subtitle: Some("Завершённые фильмы и сериалы с высокой оценкой".into()),
        categories: vec!["for_you".into(), "rewatch".into()],
        loaded: true,
        has_more,
        page,
        items,
    }))
}

async fn peers_shelf_row(
    state: &AppState,
    user_id: i64,
    excluded: &std::collections::HashSet<(MediaKind, i64)>,
    page: u32,
) -> Result<Option<CatalogRow>, AppError> {
    let all_keys = social::peers_recommendation_keys(&state.pool, &state.sync, user_id, excluded, 60).await?;
    if all_keys.is_empty() {
        return Ok(None);
    }
    let page = page.max(1);
    let start = ((page - 1) as usize) * CUSTOM_SHELF_PAGE_SIZE;
    if start >= all_keys.len() {
        return Ok(Some(CatalogRow {
            id: "peers_activity".into(),
            title: "Смотрят и советуют участники".into(),
            subtitle: Some("То, что смотрят или высоко оценили другие".into()),
            categories: vec!["for_you".into()],
            loaded: true,
            has_more: false,
            page,
            items: Vec::new(),
        }));
    }
    let end = (start + CUSTOM_SHELF_PAGE_SIZE).min(all_keys.len());
    let has_more = end < all_keys.len();
    let items = hydrate_keys(state, user_id, &all_keys[start..end]).await;
    if items.is_empty() && page == 1 {
        return Ok(None);
    }
    Ok(Some(CatalogRow {
        id: "peers_activity".into(),
        title: "Смотрят и советуют участники".into(),
        subtitle: Some("То, что смотрят или высоко оценили другие".into()),
        categories: vec!["for_you".into()],
        loaded: true,
        has_more,
        page,
        items,
    }))
}

async fn enrich_rows(state: &AppState, user_id: i64, rows: &mut [CatalogRow]) {
    let mut flat: Vec<TitleSummary> = rows.iter().flat_map(|r| r.items.iter().cloned()).collect();
    if flat.is_empty() {
        return;
    }
    social::enrich_summaries(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user_id,
        &mut flat,
    )
    .await;
    let map: HashMap<(MediaKind, i64), TitleSummary> = flat.into_iter().map(|s| ((s.kind, s.tmdb_id), s)).collect();
    for r in rows {
        for item in &mut r.items {
            if let Some(updated) = map.get(&(item.kind, item.tmdb_id)) {
                *item = updated.clone();
            }
        }
    }
}

async fn build_full_taste(
    state: &AppState,
    user_id: i64,
    with_details: bool,
) -> Result<personalize::UserTaste, AppError> {
    let (signals, extra_excluded) = tokio::try_join!(
        userdata::taste_signals(&state.pool, user_id),
        userdata::excluded_titles(&state.pool, user_id)
    )?;
    let mut taste = if with_details {
        personalize::build_taste(&*state.catalog, &signals).await
    } else {
        personalize::exclusions_only(&signals)
    };
    taste.excluded.extend(extra_excluded);
    Ok(taste)
}

async fn catalog_home(State(state): State<AppState>, user: CurrentUser) -> Result<Json<HomeFeed>, AppError> {
    // Opening the home screen means the user is browsing for a title: wake Jackett in the background.
    state.media.wake_jackett();
    let taste = build_full_taste(&state, user.0, true).await?;
    let mut feed = state.catalog.home_for(&taste).await?;
    let (peers_res, rewatch_res) = tokio::join!(
        peers_shelf_row(&state, user.0, &taste.excluded, 1),
        rewatch_shelf_row(&state, user.0, 1)
    );
    if let Ok(Some(peers)) = peers_res {
        let pos = taste.personal_rows.len().min(feed.rows.len());
        feed.rows.insert(pos, peers);
    }
    if let Ok(Some(rewatch)) = rewatch_res {
        feed.rows.push(rewatch);
    }
    feed.categories = personalize::categories_for(&feed.rows);
    enrich_rows(&state, user.0, &mut feed.rows).await;
    Ok(Json(feed))
}

#[derive(Deserialize)]
struct ShelvesQuery {
    #[serde(default)]
    ids: String,
}

async fn catalog_home_shelves(
    State(state): State<AppState>,
    user: CurrentUser,
    axum::extract::Query(q): axum::extract::Query<ShelvesQuery>,
) -> Result<Json<Vec<CatalogRow>>, AppError> {
    let ids: Vec<String> = q
        .ids
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .take(12)
        .map(str::to_owned)
        .collect();
    if ids.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let taste = build_full_taste(&state, user.0, false).await?;
    let mut rows = state.catalog.home_shelves(&ids, &taste).await?;
    enrich_rows(&state, user.0, &mut rows).await;
    Ok(Json(rows))
}

#[derive(Deserialize)]
struct ShelfQuery {
    page: Option<u32>,
}

async fn catalog_shelf(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ShelfQuery>,
) -> Result<Json<ShelfPage>, AppError> {
    if id.is_empty() || id.len() > 64 {
        return Err(AppError::bad_request("invalid shelf id"));
    }
    let page = q.page.unwrap_or(1).clamp(1, 20);
    let taste = build_full_taste(&state, user.0, false).await?;
    if id == "rewatch" {
        let row = rewatch_shelf_row(&state, user.0, page)
            .await?
            .ok_or_else(AppError::not_found)?;
        return Ok(Json(ShelfPage {
            id: row.id,
            title: row.title,
            subtitle: row.subtitle,
            page: row.page,
            has_more: row.has_more,
            items: row.items,
        }));
    }
    if id == "peers_activity" {
        let row = peers_shelf_row(&state, user.0, &taste.excluded, page)
            .await?
            .ok_or_else(AppError::not_found)?;
        return Ok(Json(ShelfPage {
            id: row.id,
            title: row.title,
            subtitle: row.subtitle,
            page: row.page,
            has_more: row.has_more,
            items: row.items,
        }));
    }
    let mut sp = state.catalog.shelf_page(&id, page, &taste).await?;
    social::enrich_summaries(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user.0,
        &mut sp.items,
    )
    .await;
    Ok(Json(sp))
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
}

async fn catalog_search(
    State(state): State<AppState>,
    user: CurrentUser,
    axum::extract::Query(query): axum::extract::Query<SearchQuery>,
) -> Result<Json<SearchResults>, AppError> {
    if query.q.chars().count() > 200 {
        return Err(AppError::bad_request("query is too long"));
    }
    if !query.q.trim().is_empty() {
        state.media.wake_jackett();
    }
    let mut res = state.catalog.search(&query.q).await?;
    social::enrich_summaries(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user.0,
        &mut res.items,
    )
    .await;
    Ok(Json(res))
}

async fn catalog_details(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
) -> Result<Json<TitleDetails>, AppError> {
    let mut details = state.catalog.details(parse_kind(&kind)?, tmdb_id).await?;
    // Whoever opens a new title is likely to press "Смотреть": look for releases meanwhile.
    crate::play::prefetch_releases(&state, user.0, details.clone());
    let mut one = [details.summary.clone()];
    social::enrich_summaries(&state.pool, &state.config.session_secret, &state.sync, user.0, &mut one).await;
    details.summary = one.into_iter().next().unwrap_or(details.summary);
    Ok(Json(details))
}

/// Cast, recommendations and similar titles. A failure here must not spoil the
/// title page, so the client asks for it separately.
async fn catalog_extras(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
) -> Result<Json<TitleExtras>, AppError> {
    let mut extras = state.catalog.extras(parse_kind(&kind)?, tmdb_id).await?;
    let rec_len = extras.recommendations.len();
    let mut all = Vec::with_capacity(rec_len + extras.similar.len());
    all.append(&mut extras.recommendations);
    all.append(&mut extras.similar);
    social::enrich_summaries(&state.pool, &state.config.session_secret, &state.sync, user.0, &mut all).await;
    extras.similar = all.split_off(rec_len);
    extras.recommendations = all;
    Ok(Json(extras))
}

/// An actor with their films and series.
async fn person(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<PersonDetails>, AppError> {
    let mut p = state.catalog.person(id).await?;
    social::enrich_summaries(
        &state.pool,
        &state.config.session_secret,
        &state.sync,
        user.0,
        &mut p.credits,
    )
    .await;
    Ok(Json(p))
}

async fn catalog_season(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path((tmdb_id, number)): Path<(i64, i32)>,
) -> Result<Json<Season>, AppError> {
    Ok(Json(state.catalog.season(tmdb_id, number).await?))
}

/// Image proxy. Public (an `<img>` can't send a token) and cacheable for a year,
/// since TMDB image paths never change content.
async fn image(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((size, file)): Path<(String, String)>,
) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;
    if !IMG_LIMITER.check(&client_key(&headers)) {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many image requests",
        ));
    }
    let (bytes, ctype) = state.catalog.image(&size, &file).await?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, ctype),
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_owned(),
            ),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (
                axum::http::header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox".to_owned(),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kino_core::{Episode, SeasonSummary, TitleSummary};
    use std::collections::HashMap;

    struct MockSeriesCatalog {
        seasons: HashMap<i32, Result<Season, CatalogError>>,
    }

    #[async_trait]
    impl Catalog for MockSeriesCatalog {
        async fn home_for(&self, _: &personalize::UserTaste) -> Result<HomeFeed, CatalogError> {
            unreachable!()
        }
        async fn home_shelves(
            &self,
            _: &[String],
            _: &personalize::UserTaste,
        ) -> Result<Vec<CatalogRow>, CatalogError> {
            unreachable!()
        }
        async fn shelf_page(&self, _: &str, _: u32, _: &personalize::UserTaste) -> Result<ShelfPage, CatalogError> {
            unreachable!()
        }
        async fn search(&self, _: &str) -> Result<SearchResults, CatalogError> {
            unreachable!()
        }
        async fn random_movie(
            &self,
            _: &crate::catalog::RandomMovieFilter,
        ) -> Result<crate::catalog::RandomMoviePick, CatalogError> {
            unreachable!()
        }
        async fn details(&self, _: MediaKind, _: i64) -> Result<TitleDetails, CatalogError> {
            unreachable!()
        }
        async fn extras(&self, _: MediaKind, _: i64) -> Result<TitleExtras, CatalogError> {
            unreachable!()
        }
        async fn person(&self, _: i64) -> Result<PersonDetails, CatalogError> {
            unreachable!()
        }
        async fn season(&self, _: i64, number: i32) -> Result<Season, CatalogError> {
            match self.seasons.get(&number) {
                Some(Ok(s)) => Ok(s.clone()),
                Some(Err(CatalogError::NotFound)) | None => Err(CatalogError::NotFound),
                Some(Err(CatalogError::Upstream(msg))) => Err(CatalogError::Upstream(msg.clone())),
            }
        }
        async fn trending_today(&self, _: MediaKind, _: usize) -> Result<Vec<TitleSummary>, CatalogError> {
            unreachable!()
        }
        async fn image(&self, _: &str, _: &str) -> Result<(Vec<u8>, String), CatalogError> {
            unreachable!()
        }
    }

    fn ep(number: i32) -> Episode {
        Episode {
            number,
            name: format!("Серия {number}"),
            overview: None,
            still_path: None,
            runtime_min: None,
            air_date: None,
        }
    }

    fn season_sum(number: i32, episode_count: i32) -> SeasonSummary {
        SeasonSummary {
            number,
            name: format!("S{number}"),
            episode_count,
            year: None,
        }
    }

    fn test_details(seasons: Vec<SeasonSummary>, last_episode: Option<(i32, i32)>) -> TitleDetails {
        TitleDetails {
            summary: TitleSummary {
                kind: MediaKind::Tv,
                tmdb_id: 42,
                title: "Тест".into(),
                original_title: None,
                year: Some(2024),
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
            seasons,
            imdb_id: None,
            last_episode,
            series_status: Some("Ended".into()),
            release_date: None,
            next_episode: None,
        }
    }

    #[tokio::test]
    async fn series_episodes_to_mark_skips_specials_and_unaired_episodes() {
        let mut seasons_map = HashMap::new();
        seasons_map.insert(
            0,
            Ok(Season {
                number: 0,
                name: "Спецматериалы".into(),
                episodes: vec![ep(1), ep(2)],
            }),
        );
        seasons_map.insert(
            1,
            Ok(Season {
                number: 1,
                name: "Сезон 1".into(),
                episodes: vec![ep(1), ep(2), ep(3)],
            }),
        );
        seasons_map.insert(
            2,
            Ok(Season {
                number: 2,
                name: "Сезон 2".into(),
                // Continuous numbering starting at 4, with 6 still unaired.
                episodes: vec![ep(4), ep(5), ep(6)],
            }),
        );
        let cat = MockSeriesCatalog { seasons: seasons_map };
        let details = test_details(
            vec![season_sum(1, 3), season_sum(2, 3), season_sum(3, 5), season_sum(0, 2)],
            Some((2, 5)),
        );

        let (mut targets, mut done_seasons) = series_episodes_to_mark(&cat, 42, &details).await;
        targets.sort_unstable();
        done_seasons.sort_unstable();
        assert_eq!(targets, vec![(1, 1), (1, 2), (1, 3), (2, 4), (2, 5)]);
        assert_eq!(done_seasons, vec![1]);
    }

    #[tokio::test]
    async fn series_episodes_to_mark_falls_back_on_season_error_and_ignores_special_last_ep() {
        let mut seasons_map = HashMap::new();
        seasons_map.insert(1, Err(CatalogError::Upstream("timeout".into())));
        seasons_map.insert(
            2,
            Ok(Season {
                number: 2,
                name: "Сезон 2".into(),
                episodes: vec![ep(1), ep(2)],
            }),
        );
        let cat = MockSeriesCatalog { seasons: seasons_map };
        let details = test_details(vec![season_sum(1, 2), season_sum(2, 2), season_sum(0, 1)], Some((0, 1)));

        let (mut targets, mut done_seasons) = series_episodes_to_mark(&cat, 42, &details).await;
        targets.sort_unstable();
        done_seasons.sort_unstable();
        assert_eq!(targets, vec![(1, 1), (1, 2), (2, 1), (2, 2)]);
        assert_eq!(done_seasons, vec![1, 2]);
    }
}
