//! Checks Telegram login signatures.
//!
//! * Mini App: <https://core.telegram.org/bots/webapps#validating-data-received-via-the-mini-app>
//! * Login Widget (the website): <https://core.telegram.org/widgets/login#checking-authorization>

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub photo_url: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("field `{0}` is missing")]
    Missing(&'static str),
    #[error("field `{0}` is malformed")]
    Malformed(&'static str),
    #[error("signature does not match")]
    BadSignature,
    #[error("login data is too old")]
    Expired,
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

/// `key=value` lines sorted by key, without `hash`, joined by `\n`.
fn data_check_string(fields: &BTreeMap<String, String>) -> String {
    fields
        .iter()
        .filter(|(k, _)| k.as_str() != "hash")
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn check(
    fields: &BTreeMap<String, String>,
    secret: &[u8],
    now: DateTime<Utc>,
    max_age: chrono::Duration,
) -> Result<(), AuthError> {
    let given = fields.get("hash").ok_or(AuthError::Missing("hash"))?;
    let given = hex::decode(given).map_err(|_| AuthError::Malformed("hash"))?;
    let expected = hmac(secret, data_check_string(fields).as_bytes());
    if !bool::from(expected.ct_eq(&given)) {
        return Err(AuthError::BadSignature);
    }
    let auth_date: i64 = fields
        .get("auth_date")
        .ok_or(AuthError::Missing("auth_date"))?
        .parse()
        .map_err(|_| AuthError::Malformed("auth_date"))?;
    let auth_date = DateTime::from_timestamp(auth_date, 0).ok_or(AuthError::Malformed("auth_date"))?;
    if now - auth_date > max_age {
        return Err(AuthError::Expired);
    }
    Ok(())
}

fn webapp_secret(bot_token: &str) -> Vec<u8> {
    hmac(b"WebAppData", bot_token.as_bytes())
}

fn widget_secret(bot_token: &str) -> Vec<u8> {
    Sha256::digest(bot_token.as_bytes()).to_vec()
}

/// Checks `Telegram.WebApp.initData` (a URL-encoded query string) and returns the user.
pub fn verify_webapp(
    init_data: &str,
    bot_token: &str,
    now: DateTime<Utc>,
    max_age: chrono::Duration,
) -> Result<TelegramUser, AuthError> {
    let fields: BTreeMap<String, String> = url::form_urlencoded::parse(init_data.as_bytes()).into_owned().collect();
    check(&fields, &webapp_secret(bot_token), now, max_age)?;
    let user = fields.get("user").ok_or(AuthError::Missing("user"))?;
    serde_json::from_str(user).map_err(|_| AuthError::Malformed("user"))
}

/// Checks the fields returned by the Telegram Login Widget and returns the user.
pub fn verify_widget(
    fields: &BTreeMap<String, String>,
    bot_token: &str,
    now: DateTime<Utc>,
    max_age: chrono::Duration,
) -> Result<TelegramUser, AuthError> {
    check(fields, &widget_secret(bot_token), now, max_age)?;
    let id = fields
        .get("id")
        .ok_or(AuthError::Missing("id"))?
        .parse()
        .map_err(|_| AuthError::Malformed("id"))?;
    Ok(TelegramUser {
        id,
        first_name: fields.get("first_name").cloned(),
        last_name: fields.get("last_name").cloned(),
        username: fields.get("username").cloned(),
        photo_url: fields.get("photo_url").cloned(),
    })
}

/// Builds a signed `initData` string, as Telegram would. Only useful for tests
/// and local development, since it needs the bot token.
pub fn sign_webapp(fields: &[(&str, &str)], bot_token: &str) -> String {
    let map: BTreeMap<String, String> = fields.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    let hash = hex::encode(hmac(&webapp_secret(bot_token), data_check_string(&map).as_bytes()));
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in fields {
        ser.append_pair(k, v);
    }
    ser.append_pair("hash", &hash);
    ser.finish()
}

/// Signs Login Widget fields, as Telegram would. For tests and local development.
pub fn sign_widget(fields: &mut BTreeMap<String, String>, bot_token: &str) {
    let hash = hex::encode(hmac(&widget_secret(bot_token), data_check_string(fields).as_bytes()));
    fields.insert("hash".into(), hash);
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "123456:TEST-token";

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_100, 0).unwrap()
    }

    fn day() -> chrono::Duration {
        chrono::Duration::days(1)
    }

    const USER: &str = r#"{"id":42,"first_name":"Даня","username":"dan"}"#;

    #[test]
    fn webapp_valid() {
        let data = sign_webapp(
            &[("auth_date", "1700000000"), ("query_id", "AAE"), ("user", USER)],
            TOKEN,
        );
        let u = verify_webapp(&data, TOKEN, now(), day()).unwrap();
        assert_eq!(u.id, 42);
        assert_eq!(u.first_name.as_deref(), Some("Даня"));
        assert_eq!(u.username.as_deref(), Some("dan"));
    }

    #[test]
    fn webapp_field_order_does_not_matter() {
        let data = sign_webapp(&[("user", USER), ("auth_date", "1700000000")], TOKEN);
        assert!(verify_webapp(&data, TOKEN, now(), day()).is_ok());
    }

    #[test]
    fn webapp_wrong_token() {
        let data = sign_webapp(&[("auth_date", "1700000000"), ("user", USER)], TOKEN);
        assert_eq!(
            verify_webapp(&data, "other:token", now(), day()),
            Err(AuthError::BadSignature)
        );
    }

    #[test]
    fn webapp_tampered_user() {
        let data = sign_webapp(&[("auth_date", "1700000000"), ("user", USER)], TOKEN);
        let tampered = data.replace("42", "43");
        assert_eq!(
            verify_webapp(&tampered, TOKEN, now(), day()),
            Err(AuthError::BadSignature)
        );
    }

    #[test]
    fn webapp_expired() {
        let data = sign_webapp(&[("auth_date", "1600000000"), ("user", USER)], TOKEN);
        assert_eq!(verify_webapp(&data, TOKEN, now(), day()), Err(AuthError::Expired));
    }

    #[test]
    fn webapp_missing_hash() {
        assert_eq!(
            verify_webapp("auth_date=1700000000", TOKEN, now(), day()),
            Err(AuthError::Missing("hash"))
        );
    }

    #[test]
    fn webapp_secret_is_not_widget_secret() {
        // A widget-style signature must not pass as Mini App data.
        let mut f = BTreeMap::from([
            ("auth_date".to_owned(), "1700000000".to_owned()),
            ("user".to_owned(), USER.to_owned()),
        ]);
        sign_widget(&mut f, TOKEN);
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &f {
            ser.append_pair(k, v);
        }
        assert_eq!(
            verify_webapp(&ser.finish(), TOKEN, now(), day()),
            Err(AuthError::BadSignature)
        );
    }

    #[test]
    fn widget_valid_and_tampered() {
        let mut f = BTreeMap::from([
            ("id".to_owned(), "42".to_owned()),
            ("first_name".to_owned(), "Даня".to_owned()),
            ("auth_date".to_owned(), "1700000000".to_owned()),
        ]);
        sign_widget(&mut f, TOKEN);
        assert_eq!(verify_widget(&f, TOKEN, now(), day()).unwrap().id, 42);
        f.insert("id".into(), "1".into());
        assert_eq!(verify_widget(&f, TOKEN, now(), day()), Err(AuthError::BadSignature));
    }
}
