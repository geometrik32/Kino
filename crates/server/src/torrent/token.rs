//! Short-lived signed links for the video stream. A `<video>` element cannot
//! send the `Authorization` header, so the stream URL carries its own proof.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamClaims {
    pub uid: i64,
    /// Info-hash of the torrent.
    pub h: String,
    /// File index inside the torrent.
    pub i: i32,
    /// Expiry, unix seconds.
    pub exp: i64,
    /// Path inside the download folder when the file was preloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<String>,
    /// The video is H.264 and can be copied when re-packing.
    #[serde(default)]
    pub v: bool,
}

/// A key of its own, so a stream link can never pass as a session token.
fn mac(secret: &[u8], payload: &str) -> Hmac<Sha256> {
    let key = Sha256::new()
        .chain_update(secret)
        .chain_update(b"kino-stream")
        .finalize();
    let mut m = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC accepts any key length");
    m.update(payload.as_bytes());
    m
}

pub fn issue(secret: &[u8], claims: &StreamClaims) -> String {
    let payload = B64.encode(serde_json::to_vec(claims).expect("claims serialize"));
    let sig = B64.encode(mac(secret, &payload).finalize().into_bytes());
    format!("{payload}.{sig}")
}

pub fn verify(secret: &[u8], token: &str, now: DateTime<Utc>) -> Option<StreamClaims> {
    let (payload, sig) = token.split_once('.')?;
    mac(secret, payload).verify_slice(&B64.decode(sig).ok()?).ok()?;
    let c: StreamClaims = serde_json::from_slice(&B64.decode(payload).ok()?).ok()?;
    (c.exp > now.timestamp()).then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn stream_tokens() {
        let now = Utc::now();
        let c = StreamClaims {
            uid: 1,
            h: "a".repeat(40),
            i: 3,
            exp: (now + chrono::Duration::hours(1)).timestamp(),
            p: None,
            v: false,
        };
        let t = issue(SECRET, &c);
        assert_eq!(verify(SECRET, &t, now), Some(c));
        assert_eq!(verify(SECRET, &t, now + chrono::Duration::hours(2)), None);
        // Not a session token, and a session token is not a stream token.
        assert_eq!(crate::session::verify(SECRET, &t, now), None);
        let s = crate::session::issue(SECRET, 1, now + chrono::Duration::hours(1));
        assert_eq!(verify(SECRET, &s, now), None);
    }
}
