//! User avatars fetched from Telegram and served through the server.
//!
//! Browsers cannot always reach `t.me` / `api.telegram.org` directly (some networks
//! require `TELEGRAM_PROXY`), and Bot API file URLs include the bot token, so the
//! server fetches, caches in PostgreSQL, and serves avatars under a signed path.

use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::header,
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use chrono::Utc;
use hmac::{Hmac, Mac};
use kino_core::Me;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{api::AppState, db, error::AppError};

const MAX_AVATAR_BYTES: usize = 2 * 1024 * 1024;
const FIRST_SYNC_TIMEOUT: Duration = Duration::from_secs(2);
const REFRESH_INTERVAL: chrono::Duration = chrono::Duration::hours(6);

#[derive(Serialize, Deserialize)]
struct AvatarClaims {
    uid: i64,
    v: String,
}

fn mac(secret: &[u8], payload: &str) -> Hmac<Sha256> {
    let key = Sha256::new()
        .chain_update(secret)
        .chain_update(b"kino-avatar")
        .finalize();
    let mut m = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC accepts any key length");
    m.update(payload.as_bytes());
    m
}

/// Signed path for a user's cached avatar; immutable while `hash` stays the same.
pub fn path(secret: &[u8], user_id: i64, hash: &str) -> String {
    let claims = AvatarClaims {
        uid: user_id,
        v: hash.to_owned(),
    };
    let payload = B64.encode(serde_json::to_vec(&claims).expect("avatar claims serialize"));
    let sig = B64.encode(mac(secret, &payload).finalize().into_bytes());
    format!("/api/avatar/{payload}.{sig}")
}

pub fn verify(secret: &[u8], token: &str) -> Option<(i64, String)> {
    let (payload, sig) = token.split_once('.')?;
    let sig = B64.decode(sig).ok()?;
    mac(secret, payload).verify_slice(&sig).ok()?;
    let claims: AvatarClaims = serde_json::from_slice(&B64.decode(payload).ok()?).ok()?;
    Some((claims.uid, claims.v))
}

/// Public (an `<img>` cannot send `Authorization`), protected by the HMAC token.
pub async fn serve(State(state): State<AppState>, Path(token): Path<String>) -> Result<Response, AppError> {
    let (user_id, requested_hash) = verify(&state.config.session_secret, &token).ok_or_else(AppError::not_found)?;
    let (data, mime, current_hash) = db::avatar_bytes(&state.pool, user_id)
        .await?
        .ok_or_else(AppError::not_found)?;
    let cache = if current_hash.as_deref() == Some(requested_hash.as_str()) {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=60"
    };
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache.to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'".to_owned()),
        ],
        data,
    )
        .into_response())
}

/// Populates `me.avatar_url` and refreshes the cached Telegram photo when missing or stale.
pub async fn attach_to_me(state: &AppState, me: &mut Me, force_if_empty: bool) {
    let meta = match db::avatar_meta(&state.pool, me.id).await {
        Ok(Some(m)) => m,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(user_id = me.id, error = %e, "loading avatar meta failed");
            return;
        }
    };

    let mut hash = meta.avatar_hash.clone();
    if let Some(api_base) = state.config.telegram_api_url.clone() {
        let needs_first = meta.avatar_checked_at.is_none() || (force_if_empty && hash.is_none());
        let stale = meta
            .avatar_checked_at
            .is_some_and(|checked| Utc::now() - checked > REFRESH_INTERVAL);

        if needs_first {
            let state_bg = state.clone();
            let photo_url = meta.photo_url.clone();
            let uid = me.id;
            let tg_id = meta.telegram_id;
            let fut = refresh_user_avatar(state_bg, api_base, uid, tg_id, photo_url);
            match tokio::time::timeout(FIRST_SYNC_TIMEOUT, fut).await {
                Ok(RefreshOutcome::Updated(new_hash)) => hash = new_hash,
                Ok(RefreshOutcome::Failed) => {}
                Err(_) => {
                    // Finish in the background so login / `/api/me` stays snappy.
                    let state_bg = state.clone();
                    let api_bg = state.config.telegram_api_url.clone().unwrap_or_default();
                    tokio::spawn(async move {
                        let _ = refresh_user_avatar(state_bg, api_bg, uid, tg_id, meta.photo_url).await;
                    });
                }
            }
        } else if stale {
            let state_bg = state.clone();
            let uid = me.id;
            let tg_id = meta.telegram_id;
            let photo_url = meta.photo_url;
            tokio::spawn(async move {
                let _ = refresh_user_avatar(state_bg, api_base, uid, tg_id, photo_url).await;
            });
        }
    }

    me.avatar_url = hash.map(|h| path(&state.config.session_secret, me.id, &h));
}

enum RefreshOutcome {
    Updated(Option<String>),
    Failed,
}

async fn refresh_user_avatar(
    state: AppState,
    api_base: String,
    user_id: i64,
    telegram_id: i64,
    photo_url: Option<String>,
) -> RefreshOutcome {
    match fetch_avatar(
        &state.telegram,
        &api_base,
        &state.config.bot_token,
        telegram_id,
        photo_url.as_deref(),
    )
    .await
    {
        FetchOutcome::Found { data, mime } => {
            let hash = hex::encode(&Sha256::digest(&data)[..8]);
            if let Err(e) = db::save_avatar(&state.pool, user_id, &data, &mime, &hash).await {
                tracing::warn!(user_id, error = %e, "saving user avatar failed");
                return RefreshOutcome::Failed;
            }
            RefreshOutcome::Updated(Some(hash))
        }
        FetchOutcome::Empty => {
            if let Err(e) = db::clear_avatar(&state.pool, user_id).await {
                tracing::warn!(user_id, error = %e, "clearing user avatar failed");
                return RefreshOutcome::Failed;
            }
            RefreshOutcome::Updated(None)
        }
        FetchOutcome::Error => RefreshOutcome::Failed,
    }
}

enum FetchOutcome {
    Found { data: Vec<u8>, mime: String },
    Empty,
    Error,
}

#[derive(Deserialize)]
struct BotApiResponse<T> {
    ok: bool,
    result: Option<T>,
}

#[derive(Deserialize)]
struct UserProfilePhotos {
    #[serde(default)]
    photos: Vec<Vec<PhotoSize>>,
}

#[derive(Deserialize)]
struct PhotoSize {
    file_id: String,
    width: u32,
}

#[derive(Deserialize)]
struct TgFile {
    file_path: Option<String>,
}

async fn fetch_avatar(
    client: &reqwest::Client,
    api_base: &str,
    bot_token: &str,
    telegram_id: i64,
    photo_url: Option<&str>,
) -> FetchOutcome {
    let api_base = api_base.trim_end_matches('/');
    let mut bot_empty = false;

    match fetch_via_bot_api(client, api_base, bot_token, telegram_id).await {
        BotResult::Found(data, mime) => return FetchOutcome::Found { data, mime },
        BotResult::Empty => bot_empty = true,
        BotResult::Error => {}
    }

    if let Some(url) = photo_url.filter(|u| is_telegram_photo_url(u, api_base)) {
        if let Some((data, mime)) = download_image(client, url, false).await {
            return FetchOutcome::Found { data, mime };
        }
    }

    if bot_empty {
        FetchOutcome::Empty
    } else {
        FetchOutcome::Error
    }
}

enum BotResult {
    Found(Vec<u8>, String),
    Empty,
    Error,
}

async fn fetch_via_bot_api(client: &reqwest::Client, api_base: &str, bot_token: &str, telegram_id: i64) -> BotResult {
    let photos_url = format!("{api_base}/bot{bot_token}/getUserProfilePhotos");
    let resp = match client
        .get(&photos_url)
        .query(&[("user_id", telegram_id.to_string()), ("limit", "1".to_owned())])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(telegram_id, error = %e.without_url(), "getUserProfilePhotos failed");
            return BotResult::Error;
        }
    };
    if resp.status() == reqwest::StatusCode::BAD_REQUEST {
        return BotResult::Empty;
    }
    if !resp.status().is_success() {
        tracing::debug!(telegram_id, status = %resp.status(), "getUserProfilePhotos HTTP error");
        return BotResult::Error;
    }
    let body: BotApiResponse<UserProfilePhotos> = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!(telegram_id, error = %e.without_url(), "getUserProfilePhotos decode failed");
            return BotResult::Error;
        }
    };
    let Some(sizes) = body
        .result
        .filter(|_| body.ok)
        .and_then(|r| r.photos.into_iter().next())
    else {
        return BotResult::Empty;
    };
    // ~320px is crisp for both 34px and 56px @2x avatars without wasting space.
    let Some(chosen) = sizes.iter().find(|s| s.width >= 200).or_else(|| sizes.last()) else {
        return BotResult::Empty;
    };

    let file_url = format!("{api_base}/bot{bot_token}/getFile");
    let file_resp = match client
        .get(&file_url)
        .query(&[("file_id", &chosen.file_id)])
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::debug!(telegram_id, status = %r.status(), "getFile HTTP error");
            return BotResult::Error;
        }
        Err(e) => {
            tracing::debug!(telegram_id, error = %e.without_url(), "getFile failed");
            return BotResult::Error;
        }
    };
    let file_body: BotApiResponse<TgFile> = match file_resp.json().await {
        Ok(b) => b,
        Err(_) => return BotResult::Error,
    };
    let Some(file_path) = file_body.result.filter(|_| file_body.ok).and_then(|f| f.file_path) else {
        return BotResult::Empty;
    };
    let file_path = file_path.trim_start_matches('/');
    let download_url = format!("{api_base}/file/bot{bot_token}/{file_path}");
    match download_image(client, &download_url, true).await {
        Some((data, mime)) => BotResult::Found(data, mime),
        None => BotResult::Error,
    }
}

async fn download_image(client: &reqwest::Client, url: &str, trust_bytes: bool) -> Option<(Vec<u8>, String)> {
    let mut resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_AVATAR_BYTES {
            return None;
        }
    }
    let header_mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase());
    let mut bytes = Vec::with_capacity(
        resp.content_length()
            .and_then(|n| usize::try_from(n).ok())
            .map(|n| n.min(MAX_AVATAR_BYTES))
            .unwrap_or(0),
    );
    while let Some(chunk) = resp.chunk().await.ok()? {
        if bytes.len().saturating_add(chunk.len()) > MAX_AVATAR_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return None;
    }
    let sniffed = sniff_image_mime(&bytes);
    let mime = match header_mime.as_deref() {
        Some(m @ ("image/jpeg" | "image/png" | "image/webp" | "image/gif")) => m.to_owned(),
        _ if trust_bytes => sniffed?.to_owned(),
        _ => return None,
    };
    let _ = sniffed;
    Some((bytes, mime))
}

fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else {
        None
    }
}

fn is_telegram_photo_url(raw: &str, api_base: &str) -> bool {
    let Ok(parsed) = Url::parse(raw) else {
        return false;
    };
    if let Ok(base) = Url::parse(api_base) {
        if parsed.scheme() == base.scheme()
            && parsed.host_str() == base.host_str()
            && parsed.port_or_known_default() == base.port_or_known_default()
        {
            return true;
        }
    }
    if parsed.scheme() != "https" {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    host == "t.me" || host.ends_with(".t.me") || host == "telegram.org" || host.ends_with(".telegram.org")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn avatar_token_round_trip() {
        let p = path(SECRET, 42, "deadbeef");
        let tok = p.strip_prefix("/api/avatar/").unwrap();
        assert_eq!(verify(SECRET, tok), Some((42, "deadbeef".to_owned())));
        assert_eq!(verify(b"wrong-secret-wrong-secret-wrong!", tok), None);
        assert_eq!(verify(SECRET, "bad.token"), None);
        // Domain-separated from session tokens.
        let session = crate::session::issue(SECRET, 42, Utc::now() + chrono::Duration::hours(1));
        assert_eq!(verify(SECRET, &session), None);
    }

    #[test]
    fn telegram_photo_url_allowlist() {
        assert!(is_telegram_photo_url(
            "https://t.me/i/userpic/320/abc.jpg",
            "https://api.telegram.org"
        ));
        assert!(is_telegram_photo_url(
            "https://cdn4.telegram.org/file.jpg",
            "https://api.telegram.org"
        ));
        assert!(!is_telegram_photo_url(
            "http://169.254.169.254/latest/meta-data",
            "https://api.telegram.org"
        ));
        assert!(!is_telegram_photo_url(
            "https://evil.example.com/a.jpg",
            "https://api.telegram.org"
        ));
    }

    #[test]
    fn sniffs_common_image_headers() {
        assert_eq!(sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image_mime(b"\x89PNG\r\n\x1a\nxxxx"), Some("image/png"));
        assert_eq!(sniff_image_mime(b"RIFF1234WEBP"), Some("image/webp"));
        assert_eq!(sniff_image_mime(b"<html>nope</html>"), None);
    }
}
