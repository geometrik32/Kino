//! Configuration from environment variables (see `.env.example`).

use std::{net::SocketAddr, path::PathBuf};

use anyhow::{bail, Context};

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: SocketAddr,
    /// Bot token from @BotFather. Used to check Telegram login signatures.
    pub bot_token: String,
    /// Secret for signing session tokens. At least 32 bytes.
    pub session_secret: Vec<u8>,
    pub session_ttl: chrono::Duration,
    /// How old a Telegram login payload may be.
    pub auth_max_age: chrono::Duration,
    /// Telegram ids that become admins on first login, no invite needed.
    pub admin_telegram_ids: Vec<i64>,
    /// Bot username without `@`, for the Login Widget on the website.
    pub bot_username: Option<String>,
    /// Public URL of the site, used to build invite links.
    pub public_url: String,
    /// Directory with the built Dioxus web app; served at `/` when set.
    pub static_dir: Option<PathBuf>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secrets.
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("admin_telegram_ids", &self.admin_telegram_ids)
            .field("public_url", &self.public_url)
            .field("static_dir", &self.static_dir)
            .finish_non_exhaustive()
    }
}

fn var(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("environment variable {name} is not set"))
}

fn var_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let session_secret = var("SESSION_SECRET")?.into_bytes();
        if session_secret.len() < 32 {
            bail!("SESSION_SECRET must be at least 32 characters");
        }
        let admin_telegram_ids = parse_ids(&var_or("ADMIN_TELEGRAM_IDS", ""))
            .context("ADMIN_TELEGRAM_IDS must be a comma-separated list of numbers")?;
        let session_days: i64 = var_or("SESSION_TTL_DAYS", "90")
            .parse()
            .context("SESSION_TTL_DAYS must be a number")?;
        Ok(Self {
            database_url: var("DATABASE_URL")?,
            bind_addr: var_or("BIND_ADDR", "0.0.0.0:8080")
                .parse()
                .context("BIND_ADDR must look like 0.0.0.0:8080")?,
            bot_token: var("BOT_TOKEN")?,
            session_secret,
            session_ttl: chrono::Duration::days(session_days),
            auth_max_age: chrono::Duration::days(1),
            admin_telegram_ids,
            bot_username: std::env::var("BOT_USERNAME").ok().filter(|s| !s.is_empty()),
            public_url: var_or("PUBLIC_URL", "http://localhost:8080")
                .trim_end_matches('/')
                .to_owned(),
            static_dir: std::env::var("STATIC_DIR").ok().map(PathBuf::from),
        })
    }
}

pub fn parse_ids(s: &str) -> Result<Vec<i64>, std::num::ParseIntError> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::parse)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_ids;

    #[test]
    fn ids() {
        assert_eq!(parse_ids("").unwrap(), Vec::<i64>::new());
        assert_eq!(parse_ids(" 1, 22 ,").unwrap(), vec![1, 22]);
        assert!(parse_ids("1,x").is_err());
    }
}
