//! Talks to the Kino server. The app is served by the same server, so URLs are relative.

use gloo_net::http::{Request, Response};
use kino_core::{
    ApiError, AuthResponse, ContinueItem, HistoryEntry, HomeFeed, MarkWatched, Me, MediaKind, PreloadItem,
    PreloadRequest, Progress, RatingUpdate, ReleaseChoice, ReleaseChoiceUpdate, SearchResults, Season, StatusUpdate,
    TitleDetails, TitleState, TitleStatus, UserPrefs, WebAppAuthRequest, WidgetAuthRequest,
};
use serde::{de::DeserializeOwned, Deserialize};

const TOKEN_KEY: &str = "kino.token";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub fn saved_token() -> Option<String> {
    storage()?.get_item(TOKEN_KEY).ok()?
}

pub fn save_token(token: &str) {
    if let Some(s) = storage() {
        let _ = s.set_item(TOKEN_KEY, token);
    }
}

pub fn forget_token() {
    if let Some(s) = storage() {
        let _ = s.remove_item(TOKEN_KEY);
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
            message: e.to_string(),
        }
    }
}

async fn decode<T: DeserializeOwned>(resp: Response) -> Result<T, Failure> {
    if resp.ok() {
        return resp.json().await.map_err(Failure::network);
    }
    match resp.json::<ApiError>().await {
        Ok(e) => Err(Failure {
            code: e.code,
            message: e.message,
        }),
        Err(_) => Err(Failure {
            code: "http".into(),
            message: format!("HTTP {}", resp.status()),
        }),
    }
}

async fn get<T: DeserializeOwned>(path: &str, token: &str) -> Result<T, Failure> {
    let resp = Request::get(path)
        .header("Authorization", &format!("Bearer {token}"))
        .send()
        .await
        .map_err(Failure::network)?;
    decode(resp).await
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicConfig {
    pub bot_username: Option<String>,
}

pub async fn public_config() -> Result<PublicConfig, Failure> {
    decode(
        Request::get("/api/public-config")
            .send()
            .await
            .map_err(Failure::network)?,
    )
    .await
}

pub async fn login_webapp(init_data: String, invite: Option<String>) -> Result<AuthResponse, Failure> {
    let req = Request::post("/api/auth/webapp")
        .json(&WebAppAuthRequest { init_data, invite })
        .map_err(Failure::network)?;
    decode(req.send().await.map_err(Failure::network)?).await
}

pub async fn login_widget(req: WidgetAuthRequest) -> Result<AuthResponse, Failure> {
    let req = Request::post("/api/auth/widget").json(&req).map_err(Failure::network)?;
    decode(req.send().await.map_err(Failure::network)?).await
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
    let q = js_sys::encode_uri_component(q);
    get(&format!("/api/catalog/search?q={q}"), token).await
}

pub async fn details(token: &str, kind: MediaKind, id: i64) -> Result<TitleDetails, Failure> {
    get(&format!("/api/catalog/{}/{id}", kind.as_str()), token).await
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
    let req = Request::put("/api/release-choice")
        .header("Authorization", &format!("Bearer {token}"))
        .json(choice)
        .map_err(Failure::network)?;
    decode(req.send().await.map_err(Failure::network)?).await
}

/// PUT/POST/DELETE with an optional JSON body; the response body is decoded.
async fn send<T: DeserializeOwned>(
    method: &str,
    path: &str,
    token: &str,
    body: Option<&impl serde::Serialize>,
) -> Result<T, Failure> {
    let auth = format!("Bearer {token}");
    let builder = match method {
        "PUT" => Request::put(path),
        "POST" => Request::post(path),
        _ => Request::delete(path),
    }
    .header("Authorization", &auth);
    let req = match body {
        Some(b) => builder.json(b).map_err(Failure::network)?,
        None => builder.build().map_err(Failure::network)?,
    };
    let resp = req.send().await.map_err(Failure::network)?;
    if resp.status() == 204 {
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
