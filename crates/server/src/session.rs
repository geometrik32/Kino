//! Stateless session tokens: `base64url(claims).base64url(hmac)`.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

#[derive(Serialize, Deserialize)]
struct Claims {
    /// Internal user id.
    uid: i64,
    /// Expiry, unix seconds.
    exp: i64,
}

fn mac(secret: &[u8], payload: &str) -> Hmac<Sha256> {
    let mut m = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    m.update(payload.as_bytes());
    m
}

pub fn issue(secret: &[u8], user_id: i64, expires_at: DateTime<Utc>) -> String {
    let claims = Claims {
        uid: user_id,
        exp: expires_at.timestamp(),
    };
    let payload = B64.encode(serde_json::to_vec(&claims).expect("claims serialize"));
    let sig = B64.encode(mac(secret, &payload).finalize().into_bytes());
    format!("{payload}.{sig}")
}

/// Returns the user id when the token is authentic and not expired.
pub fn verify(secret: &[u8], token: &str, now: DateTime<Utc>) -> Option<i64> {
    let (payload, sig) = token.split_once('.')?;
    let sig = B64.decode(sig).ok()?;
    mac(secret, payload).verify_slice(&sig).ok()?;
    let claims: Claims = serde_json::from_slice(&B64.decode(payload).ok()?).ok()?;
    (claims.exp > now.timestamp()).then_some(claims.uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn round_trip_and_expiry() {
        let now = Utc::now();
        let t = issue(SECRET, 7, now + chrono::Duration::hours(1));
        assert_eq!(verify(SECRET, &t, now), Some(7));
        assert_eq!(verify(SECRET, &t, now + chrono::Duration::hours(2)), None);
    }

    #[test]
    fn rejects_forgery() {
        let now = Utc::now();
        let t = issue(SECRET, 7, now + chrono::Duration::hours(1));
        assert_eq!(verify(b"another-secret-another-secret-xx", &t, now), None);
        let other = issue(SECRET, 8, now + chrono::Duration::hours(1));
        let (_, sig7) = t.split_once('.').unwrap();
        let (payload8, _) = other.split_once('.').unwrap();
        assert_eq!(verify(SECRET, &format!("{payload8}.{sig7}"), now), None);
        assert_eq!(verify(SECRET, "garbage", now), None);
    }
}
