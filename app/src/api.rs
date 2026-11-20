//! Talks to the Kino server. The app is served by the same server, so URLs are relative.

use std::sync::LazyLock;

use kino_core::{
    ApiError, AuthResponse, ContinueItem, HistoryEntry, HomeFeed, LoginLink, LoginLinkRequest, LoginLinkStatus,
    MarkWatched, Me, MediaKind, PlayInfo, PlayRequest, PreloadItem, PreloadRequest, Progress, ProgressUpdate,
    RatingUpdate, ReleaseChoice, ReleaseChoiceUpdate, ReleaseList, SearchResults, Season, StatusUpdate, StreamProgress,
    TitleDetails, TitleExtras, TitleState, TitleStatus, TracksUpdate, UserPrefs, WebAppAuthRequest, WidgetAuthRequest,
};
use reqwest::{Client, RequestBuilder, Response};
use serde::de::DeserializeOwned;

use crate::platform;

const TOKEN_KEY: &str = "kino.token";

pub async fn saved_token() -> Option<String> {
    platform::storage_get(TOKEN_KEY).await
}

pub fn save_token(token: &str) {
    platform::storage_set(TOKEN_KEY, token);
}

pub fn forget_token() {
    platform::storage_remove(TOKEN_KEY);
}

static CLIENT: LazyLock<Client> = LazyLock::new(Client::new);

/// A request to `path` (e.g. `/api/me`), with the user's token when there is one.
fn request(method: reqwest::Method, path: &str, token: Option<&str>) -> RequestBuilder {
    let r = CLIENT.request(method, platform::abs(path));
    match token {
        Some(t) => r.bearer_auth(t),
        None => r,
    }
}

/// An API failure, with the server's code (`not_invited`, `unauthorized`, ...) when there is one.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub code: String,
    pub message: String,
}

impl Failure {
    fn network(e: impl std::fmt::Display) -> Self {
        Self {
            code: "network".into(),
            message: format!("Нет связи с сервером: {e}"),
        }
    }
}

async fn decode<T: DeserializeOwned>(resp: Response) -> Result<T, Failure> {
    let status = resp.status();
    if status.is_success() {
        return resp.json().await.map_err(Failure::network);
    }
    match resp.json::<ApiError>().await {
        Ok(e) => Err(Failure {
            code: e.code,
            message: e.message,
        }),
        Err(_) => Err(Failure {
            code: "http".into(),
            message: format!("HTTP {}", status.as_u16()),
        }),
    }
}

async fn get<T: DeserializeOwned>(path: &str, token: &str) -> Result<T, Failure> {
    let resp = request(reqwest::Method::GET, path, Some(token))
        .send()
        .await
        .map_err(Failure::network)?;
    decode(resp).await
}

pub async fn login_webapp(init_data: String, invite: Option<String>) -> Result<AuthResponse, Failure> {
    let req = request(reqwest::Method::POST, "/api/auth/webapp", None).json(&WebAppAuthRequest { init_data, invite });
    decode(req.send().await.map_err(Failure::network)?).await
}

pub async fn login_widget(req: WidgetAuthRequest) -> Result<AuthResponse, Failure> {
    let req = request(reqwest::Method::POST, "/api/auth/widget", None).json(&req);
    decode(req.send().await.map_err(Failure::network)?).await
}

/// Starts logging in through the Telegram app.
pub async fn start_login_link(invite: Option<String>) -> Result<LoginLink, Failure> {
    let req = request(reqwest::Method::POST, "/api/auth/link", None).json(&LoginLinkRequest { invite });
    decode(req.send().await.map_err(Failure::network)?).await
}

pub async fn poll_login_link(token: &str) -> Result<LoginLinkStatus, Failure> {
    let resp = request(reqwest::Method::GET, &format!("/api/auth/link/{token}"), None)
        .send()
        .await
        .map_err(Failure::network)?;
    decode(resp).await
}

pub async fn me(token: &str) -> Result<Me, Failure> {
    get("/api/me", token).await
}

pub async fn continue_watching(token: &str) -> Result<Vec<ContinueItem>, Failure> {
    get("/api/continue", token).await
}

pub async fn home(token: &str) -> Result<HomeFeed, Failure> {
    get("/api/catalog/home", token).await
}

pub async fn search(token: &str, q: &str) -> Result<SearchResults, Failure> {
    let q = urlencoding::encode(q);
    get(&format!("/api/catalog/search?q={q}"), token).await
}

pub async fn details(token: &str, kind: MediaKind, id: i64) -> Result<TitleDetails, Failure> {
    get(&format!("/api/catalog/{}/{id}", kind.as_str()), token).await
}

/// Cast, recommendations and similar titles; loaded apart so the page shows at once.
pub async fn extras(token: &str, kind: MediaKind, id: i64) -> Result<TitleExtras, Failure> {
    get(&format!("/api/catalog/{}/{id}/extras", kind.as_str()), token).await
}

pub async fn season(token: &str, id: i64, number: i32) -> Result<Season, Failure> {
    get(&format!("/api/catalog/tv/{id}/season/{number}"), token).await
}

pub async fn title_progress(token: &str, kind: MediaKind, id: i64) -> Result<Vec<Progress>, Failure> {
    get(&format!("/api/progress/{}/{id}", kind.as_str()), token).await
}

pub async fn release_choices(token: &str, kind: MediaKind, id: i64) -> Result<Vec<ReleaseChoice>, Failure> {
    get(&format!("/api/release-choice/{}/{id}", kind.as_str()), token).await
}

/// Remembers the release picked for a film or a season.
pub async fn save_release_choice(token: &str, choice: &ReleaseChoiceUpdate) -> Result<ReleaseChoice, Failure> {
    let req = request(reqwest::Method::PUT, "/api/release-choice", Some(token)).json(choice);
    decode(req.send().await.map_err(Failure::network)?).await
}

/// PUT/POST/DELETE with an optional JSON body; the response body is decoded.
async fn send<T: DeserializeOwned>(
    method: &str,
    path: &str,
    token: &str,
    body: Option<&impl serde::Serialize>,
) -> Result<T, Failure> {
    let method = match method {
        "PUT" => reqwest::Method::PUT,
        "POST" => reqwest::Method::POST,
        _ => reqwest::Method::DELETE,
    };
    let builder = request(method, path, Some(token));
    let req = match body {
        Some(b) => builder.json(b),
        None => builder,
    };
    let resp = req.send().await.map_err(Failure::network)?;
    if resp.status().as_u16() == 204 {
        // No body: decode `null` into `()` (or any type that accepts it).
        return serde_json::from_str("null").map_err(Failure::network);
    }
    decode(resp).await
}

pub async fn title_state(token: &str, kind: MediaKind, id: i64) -> Result<TitleState, Failure> {
    get(&format!("/api/title/{}/{id}/state", kind.as_str()), token).await
}

pub async fn set_status(
    token: &str,
    kind: MediaKind,
    id: i64,
    status: Option<TitleStatus>,
) -> Result<TitleState, Failure> {
    let path = format!("/api/title/{}/{id}/status", kind.as_str());
    send("PUT", &path, token, Some(&StatusUpdate { status })).await
}

pub async fn set_rating(token: &str, kind: MediaKind, id: i64, rating: Option<i16>) -> Result<TitleState, Failure> {
    let path = format!("/api/title/{}/{id}/rating", kind.as_str());
    send("PUT", &path, token, Some(&RatingUpdate { rating })).await
}

pub async fn mark(token: &str, m: &MarkWatched) -> Result<(), Failure> {
    send("POST", "/api/marks", token, Some(m)).await
}

pub async fn my(token: &str) -> Result<Vec<TitleState>, Failure> {
    get("/api/my", token).await
}

pub async fn history(token: &str) -> Result<Vec<HistoryEntry>, Failure> {
    get("/api/history?limit=100", token).await
}

pub async fn prefs(token: &str) -> Result<UserPrefs, Failure> {
    get("/api/prefs", token).await
}

pub async fn save_prefs(token: &str, p: &UserPrefs) -> Result<UserPrefs, Failure> {
    send("PUT", "/api/prefs", token, Some(p)).await
}

pub async fn preloads(token: &str) -> Result<Vec<PreloadItem>, Failure> {
    get("/api/preload", token).await
}

pub async fn add_preload(token: &str, r: &PreloadRequest) -> Result<PreloadItem, Failure> {
    send("POST", "/api/preload", token, Some(r)).await
}

pub async fn delete_preload(token: &str, id: i64) -> Result<(), Failure> {
    send("DELETE", &format!("/api/preload/{id}"), token, None::<&()>).await
}

pub async fn releases(token: &str, kind: MediaKind, id: i64, season: i32) -> Result<ReleaseList, Failure> {
    get(&format!("/api/releases/{}/{id}?season={season}", kind.as_str()), token).await
}

pub async fn play(token: &str, r: &PlayRequest) -> Result<PlayInfo, Failure> {
    let mut info: PlayInfo = send("POST", "/api/play", token, Some(r)).await?;
    // The server answers with a path; the player needs a full address in the apps.
    info.stream = platform::abs(&info.stream);
    Ok(info)
}

/// How far connecting to a release has got.
pub async fn play_progress(token: &str, release: &str) -> Result<StreamProgress, Failure> {
    get(&format!("/api/play/progress/{release}"), token).await
}

pub async fn save_progress(token: &str, u: &ProgressUpdate) -> Result<Progress, Failure> {
    send("PUT", "/api/progress", token, Some(u)).await
}

pub async fn set_tracks(token: &str, kind: MediaKind, id: i64, t: &TracksUpdate) -> Result<TitleState, Failure> {
    let path = format!("/api/title/{}/{id}/tracks", kind.as_str());
    send("PUT", &path, token, Some(t)).await
}
