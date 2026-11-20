//! Talks to the Kino server. The app is served by the same server, so URLs are relative.

use gloo_net::http::{Request, Response};
use kino_core::{ApiError, AuthResponse, ContinueItem, Me, WatchlistItem, WebAppAuthRequest, WidgetAuthRequest};
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
