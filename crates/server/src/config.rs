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
    /// TMDB API key (v3) or read access token (v4). Without it the demo catalog is used.
    pub tmdb_api_key: Option<String>,
    /// Language for titles and descriptions, e.g. `ru-RU`.
    pub tmdb_language: String,
    /// Directory with the built Dioxus web app; served at `/` when set.
    pub static_dir: Option<PathBuf>,
    /// How often to look for new episodes of followed series.
    pub notify_every: std::time::Duration,
    /// How long a preloaded release is kept after it was watched.
    pub preload_keep: chrono::Duration,
    /// Torrent services; each one is optional. See [`TorrentConfig`].
    pub torrents: TorrentConfig,
}

/// Where the torrent services live. Without Jackett the release list is demo
/// data; without TorrServer nothing can be played; without qBittorrent there is
/// no preloading.
#[derive(Clone, Default)]
pub struct TorrentConfig {
    pub jackett_url: Option<String>,
    pub jackett_api_key: Option<String>,
    pub torrserver_url: Option<String>,
    pub torrserver_user: Option<String>,
    pub torrserver_password: Option<String>,
    pub qbittorrent_url: Option<String>,
    pub qbittorrent_user: String,
    pub qbittorrent_password: String,
    /// Download folder as qBittorrent sees it.
    pub qbittorrent_save_path: Option<String>,
    /// The same folder as this server sees it, to play preloaded files from disk.
    pub preload_dir: Option<PathBuf>,
    pub ffmpeg: String,
    pub ffprobe: String,
    /// `none`, `qsv` (Intel Quick Sync) or `vaapi`.
    pub hwaccel: String,
}

impl TorrentConfig {
    pub fn from_env() -> Self {
        let opt = |n: &str| {
            std::env::var(n)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        Self {
            jackett_url: opt("JACKETT_URL"),
            jackett_api_key: opt("JACKETT_API_KEY"),
            torrserver_url: opt("TORRSERVER_URL"),
            torrserver_user: opt("TORRSERVER_USER"),
            torrserver_password: opt("TORRSERVER_PASSWORD"),
            qbittorrent_url: opt("QBITTORRENT_URL"),
            qbittorrent_user: opt("QBITTORRENT_USER").unwrap_or_else(|| "admin".into()),
            qbittorrent_password: opt("QBITTORRENT_PASSWORD").unwrap_or_default(),
            qbittorrent_save_path: opt("QBITTORRENT_SAVE_PATH"),
            preload_dir: opt("PRELOAD_DIR").map(PathBuf::from),
            ffmpeg: opt("FFMPEG").unwrap_or_else(|| "ffmpeg".into()),
            ffprobe: opt("FFPROBE").unwrap_or_else(|| "ffprobe".into()),
            hwaccel: opt("TRANSCODE_HWACCEL").unwrap_or_else(|| "none".into()),
        }
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secrets.
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("admin_telegram_ids", &self.admin_telegram_ids)
            .field("public_url", &self.public_url)
            .field(
                "tmdb",
                &self.tmdb_api_key.as_ref().map(|_| "set").unwrap_or("demo catalog"),
            )
            .field("static_dir", &self.static_dir)
            .field("jackett", &self.torrents.jackett_url)
            .field("torrserver", &self.torrents.torrserver_url)
            .field("qbittorrent", &self.torrents.qbittorrent_url)
            .field("hwaccel", &self.torrents.hwaccel)
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
            tmdb_api_key: std::env::var("TMDB_API_KEY").ok().filter(|s| !s.trim().is_empty()),
            tmdb_language: var_or("TMDB_LANGUAGE", "ru-RU"),
            static_dir: std::env::var("STATIC_DIR").ok().map(PathBuf::from),
            notify_every: std::time::Duration::from_secs(
                3600 * var_or("NOTIFY_INTERVAL_HOURS", "6")
                    .parse::<u64>()
                    .context("NOTIFY_INTERVAL_HOURS must be a number")?
                    .max(1),
            ),
            preload_keep: chrono::Duration::days(
                var_or("PRELOAD_KEEP_DAYS", "7")
                    .parse()
                    .context("PRELOAD_KEEP_DAYS must be a number")?,
            ),
            torrents: TorrentConfig::from_env(),
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
