//! Talks to the Kino server. The app is served by the same server, so URLs are relative.

use gloo_net::http::{Request, Response};
use kino_core::{
    ApiError, AuthResponse, ContinueItem, HomeFeed, Me, MediaKind, Progress, ReleaseChoice, ReleaseChoiceUpdate,
    SearchResults, Season, TitleDetails, WatchlistItem, WebAppAuthRequest, WidgetAuthRequest,
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

pub async fn watchlist(token: &str) -> Result<Vec<WatchlistItem>, Failure> {
    get("/api/watchlist", token).await
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

/// Adds to or removes from "Хочу посмотреть".
pub async fn set_watchlist(token: &str, kind: MediaKind, id: i64, on: bool) -> Result<(), Failure> {
    let path = format!("/api/watchlist/{}/{id}", kind.as_str());
    let auth = format!("Bearer {token}");
    let req = if on {
        Request::put(&path)
    } else {
        Request::delete(&path)
    };
    let resp = req
        .header("Authorization", &auth)
        .send()
        .await
        .map_err(Failure::network)?;
    if resp.ok() || resp.status() == 404 {
        Ok(())
    } else {
        decode::<serde_json::Value>(resp).await.map(|_| ())
    }
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
