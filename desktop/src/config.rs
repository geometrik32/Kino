//! Settings of the desktop app, kept in the user's profile
//! (`%APPDATA%\Orel Kino\config.json` on Windows).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_SERVER: &str = "https://kino.aat-terra.ru";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Address of the Kino server, without a trailing slash.
    pub server: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: DEFAULT_SERVER.into(),
        }
    }
}

/// Folder of the app's files (settings, saved sign-in).
pub fn data_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Orel Kino")
}

fn path() -> PathBuf {
    data_dir().join("config.json")
}

impl Config {
    /// Reads the settings; the environment variable `KINO_SERVER` wins over the file.
    pub fn load() -> Self {
        let mut cfg: Config = std::fs::read(path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if let Ok(s) = std::env::var("KINO_SERVER") {
            cfg.server = s;
        }
        cfg.server = cfg.server.trim().trim_end_matches('/').to_owned();
        if cfg.server.is_empty() {
            cfg.server = DEFAULT_SERVER.into();
        }
        cfg
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir())?;
        std::fs::write(path(), serde_json::to_vec_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.server, DEFAULT_SERVER);
    }
}
