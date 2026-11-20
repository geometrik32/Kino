//! HTTP routes under `/api`.

use std::sync::Arc;

use axum::{
    extract::{FromRequestParts, Path, State},
    http::{request::Parts, StatusCode},
    routing::{get, post, put},
    Json, Router,
};
use chrono::Utc;
use kino_core::{
    AuthResponse, ContinueItem, HistoryEntry, HomeFeed, LoginLink, LoginLinkRequest, LoginLinkStatus, MarkWatched, Me,
    MediaKind, PersonDetails, PreloadItem, PreloadRequest, Progress, ProgressUpdate, RatingUpdate, ReleaseChoice,
    ReleaseChoiceUpdate, Role, SearchResults, Season, StatusUpdate, TitleDetails, TitleExtras, TitleState, TitleStatus,
    TracksUpdate, UserPrefs, WatchlistItem, WebAppAuthRequest, WidgetAuthRequest,
};
use serde::{Deserialize, Serialize};

use crate::{
    catalog::{Catalog, CatalogError},
    config::Config,
    db::{self, LoginOutcome, Pool},
    error::AppError,
    session,
    telegram_auth::{self, TelegramUser},
    userdata,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: Pool,
    pub config: Arc<Config>,
    pub catalog: Arc<dyn Catalog>,
    pub media: Arc<crate::play::Media>,
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
        .route("/title/{kind}/{tmdb_id}/tracks", put(put_tracks))
        .route("/my", get(my_titles))
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
        .route("/catalog/search", get(catalog_search))
        .route("/catalog/{kind}/{tmdb_id}", get(catalog_details))
        .route("/catalog/{kind}/{tmdb_id}/extras", get(catalog_extras))
        .route("/person/{id}", get(person))
        .route("/catalog/tv/{tmdb_id}/season/{number}", get(catalog_season))
        .route("/img/{size}/{file}", get(image))
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
        LoginOutcome::LoggedIn(me) => {
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
    Json(req): Json<LoginLinkRequest>,
) -> Result<Json<LoginLink>, AppError> {
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
    Ok(Json(load_me(&state, &user).await?))
}

async fn continue_watching(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<ContinueItem>>, AppError> {
    let mut items = db::continue_watching(&state.pool, user.0, 20).await?;
    // Names and posters; a title the catalog can't resolve still shows, just without them.
    for item in &mut items {
        item.title = state
            .catalog
            .details(item.last.kind, item.last.tmdb_id)
            .await
            .ok()
            .map(|d| d.summary);
    }
    Ok(Json(items))
}

/// BitTorrent v1 (SHA-1) or v2 (SHA-256) info-hash in hex.
/// An info-hash (v1 or v2), or the placeholder key of a release found by the indexer.
fn infohash_ok(h: &str) -> bool {
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

async fn save_progress(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(mut u): Json<ProgressUpdate>,
) -> Result<Json<Progress>, AppError> {
    validate(&u)?;
    if let Some(r) = &mut u.release {
        r.infohash.make_ascii_lowercase();
    }
    let p = db::save_progress(&state.pool, user.0, &u).await?;
    userdata::refresh_status(&state.pool, &*state.catalog, user.0, p.kind, p.tmdb_id, true).await?;
    if p.finished {
        after_finished(&state, user.0, p.kind, p.tmdb_id, p.season, p.episode).await?;
    }
    Ok(Json(p))
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
    if c.tmdb_id <= 0 || c.season < 0 || (c.kind == MediaKind::Movie && c.season != 0) {
        return Err(AppError::bad_request("tmdb_id must be positive; season is 0 for films"));
    }
    c.infohash.make_ascii_lowercase();
    Ok(Json(db::save_release_choice(&state.pool, user.0, &c).await?))
}

async fn release_choices(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
) -> Result<Json<Vec<ReleaseChoice>>, AppError> {
    Ok(Json(
        db::release_choices(&state.pool, user.0, parse_kind(&kind)?, tmdb_id).await?,
    ))
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
    Ok(Json(
        userdata::get_state(&state.pool, user.0, parse_kind(&kind)?, tmdb_id).await?,
    ))
}

async fn put_status(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
    Json(u): Json<StatusUpdate>,
) -> Result<Json<TitleState>, AppError> {
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

async fn put_rating(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
    Json(u): Json<RatingUpdate>,
) -> Result<Json<TitleState>, AppError> {
    if u.rating.is_some_and(|r| !(1..=5).contains(&r)) {
        return Err(AppError::bad_request("rating must be 1 to 5"));
    }
    Ok(Json(
        userdata::set_rating(&state.pool, user.0, parse_kind(&kind)?, tmdb_id, u.rating).await?,
    ))
}

async fn put_tracks(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
    Json(u): Json<TracksUpdate>,
) -> Result<Json<TitleState>, AppError> {
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
    let status = match q.status.as_deref() {
        None | Some("") => None,
        Some(s) => Some(TitleStatus::parse(s).ok_or_else(|| AppError::bad_request("unknown status"))?),
    };
    let mut list = userdata::list_states(&state.pool, user.0, status).await?;
    for item in &mut list {
        item.title = summary_of(&state, item.kind, item.tmdb_id).await;
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
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let mut list = userdata::history(&state.pool, user.0, limit).await?;
    let mut cache = std::collections::HashMap::new();
    for item in &mut list {
        let key = (item.kind, item.tmdb_id);
        if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(key) {
            e.insert(summary_of(&state, item.kind, item.tmdb_id).await);
        }
        item.title = cache[&key].clone();
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
    if [&p.quality, &p.voice, &p.subtitle_lang, &p.language, &p.codec, &p.sort]
        .into_iter()
        .any(long)
    {
        return Err(AppError::bad_request("values must be at most 100 characters"));
    }
    userdata::set_prefs(&state.pool, user.0, &p).await?;
    Ok(Json(p))
}

async fn preloads(State(state): State<AppState>, user: CurrentUser) -> Result<Json<Vec<PreloadItem>>, AppError> {
    Ok(Json(userdata::preloads(&state.pool, user.0).await?))
}

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
    let media = &state.media;
    let Some(qbit) = &media.qbit else {
        // Without a downloader the request is only recorded (demo mode).
        return Ok(Json(userdata::preload_add(&state.pool, user.0, &r).await?));
    };
    let src = crate::torrent::resolve(&state.pool, &media.http, &r.infohash).await?;
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
    db::watchlist_add(&state.pool, user.0, parse_kind(&kind)?, tmdb_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn watchlist_remove(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
) -> Result<StatusCode, AppError> {
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

async fn catalog_home(State(state): State<AppState>, _user: CurrentUser) -> Result<Json<HomeFeed>, AppError> {
    Ok(Json(state.catalog.home().await?))
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
}

async fn catalog_search(
    State(state): State<AppState>,
    _user: CurrentUser,
    axum::extract::Query(query): axum::extract::Query<SearchQuery>,
) -> Result<Json<SearchResults>, AppError> {
    if query.q.chars().count() > 200 {
        return Err(AppError::bad_request("query is too long"));
    }
    Ok(Json(state.catalog.search(&query.q).await?))
}

async fn catalog_details(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
) -> Result<Json<TitleDetails>, AppError> {
    let details = state.catalog.details(parse_kind(&kind)?, tmdb_id).await?;
    // Whoever opens a title is likely to press "Смотреть": look for releases meanwhile.
    crate::play::prefetch_releases(&state, details.clone());
    Ok(Json(details))
}

/// Cast, recommendations and similar titles. A failure here must not spoil the
/// title page, so the client asks for it separately.
async fn catalog_extras(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path((kind, tmdb_id)): Path<(String, i64)>,
) -> Result<Json<TitleExtras>, AppError> {
    Ok(Json(state.catalog.extras(parse_kind(&kind)?, tmdb_id).await?))
}

/// An actor with their films and series.
async fn person(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<PersonDetails>, AppError> {
    Ok(Json(state.catalog.person(id).await?))
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
    Path((size, file)): Path<(String, String)>,
) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;
    let (bytes, ctype) = state.catalog.image(&size, &file).await?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, ctype),
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_owned(),
            ),
        ],
        bytes,
    )
        .into_response())
}
