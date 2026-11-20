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
    AuthResponse, ContinueItem, HomeFeed, Me, MediaKind, Progress, ProgressUpdate, Role, SearchResults, Season,
    TitleDetails, WatchlistItem, WebAppAuthRequest, WidgetAuthRequest,
};
use serde::{Deserialize, Serialize};

use crate::{
    catalog::{Catalog, CatalogError},
    config::Config,
    db::{self, LoginOutcome, Pool},
    error::AppError,
    session,
    telegram_auth::{self, TelegramUser},
};

#[derive(Clone)]
pub struct AppState {
    pub pool: Pool,
    pub config: Arc<Config>,
    pub catalog: Arc<dyn Catalog>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/public-config", get(public_config))
        .route("/auth/webapp", post(auth_webapp))
        .route("/auth/widget", post(auth_widget))
        .route("/me", get(me))
        .route("/continue", get(continue_watching))
        .route("/progress", put(save_progress))
        .route("/progress/{kind}/{tmdb_id}", get(title_progress))
        .route("/watchlist", get(watchlist))
        .route(
            "/watchlist/{kind}/{tmdb_id}",
            put(watchlist_add).delete(watchlist_remove),
        )
        .route("/invites", post(create_invite))
        .route("/catalog/home", get(catalog_home))
        .route("/catalog/search", get(catalog_search))
        .route("/catalog/{kind}/{tmdb_id}", get(catalog_details))
        .route("/catalog/tv/{tmdb_id}/season/{number}", get(catalog_season))
        .route("/img/{size}/{file}", get(image))
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
        let hex_ok = matches!(r.infohash.len(), 40 | 64) && r.infohash.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex_ok {
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
    Ok(Json(db::save_progress(&state.pool, user.0, &u).await?))
}

fn parse_kind(kind: &str) -> Result<MediaKind, AppError> {
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
    Ok(Json(state.catalog.details(parse_kind(&kind)?, tmdb_id).await?))
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
