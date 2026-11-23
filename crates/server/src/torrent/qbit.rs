//! qBittorrent Web API: downloading releases in advance to the server's disk.

use serde::Deserialize;
use tokio::sync::Mutex;

use super::{files::FileEntry, Source};

pub struct QBittorrent {
    http: reqwest::Client,
    base: String,
    user: String,
    password: String,
    /// Download folder as qBittorrent sees it, e.g. `/downloads/kino`.
    save_path: Option<String>,
    /// Session cookie `(name, value)` (`SID` in older qBittorrent, `QBT_SID_<port>` in 5.x+).
    sid: Mutex<Option<(String, String)>>,
}

fn is_session_cookie(name: &str) -> bool {
    name == "SID" || name.starts_with("QBT_SID_")
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TorrentInfo {
    pub hash: String,
    /// 0.0–1.0.
    pub progress: f32,
    /// "downloading", "stalledDL", "uploading", "error", "missingFiles", ...
    pub state: String,
    pub size: i64,
}

impl TorrentInfo {
    pub fn failed(&self) -> bool {
        matches!(self.state.as_str(), "error" | "missingFiles")
    }

    pub fn done(&self) -> bool {
        self.progress >= 1.0
    }
}

#[derive(Debug, Deserialize)]
struct RawFile {
    index: i32,
    name: String,
    size: u64,
}

impl QBittorrent {
    pub fn new(base: &str, user: &str, password: &str, save_path: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("qBittorrent HTTP client");
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            user: user.to_owned(),
            password: password.to_owned(),
            save_path,
            sid: Mutex::new(None),
        }
    }

    async fn login(&self) -> anyhow::Result<(String, String)> {
        let resp = self
            .http
            .post(format!("{}/api/v2/auth/login", self.base))
            .header(reqwest::header::REFERER, &self.base)
            .form(&[("username", self.user.as_str()), ("password", self.password.as_str())])
            .send()
            .await?
            .error_for_status()?;
        let cookie = resp
            .cookies()
            .find(|c| is_session_cookie(c.name()))
            .map(|c| (c.name().to_owned(), c.value().to_owned()))
            .ok_or_else(|| anyhow::anyhow!("qBittorrent did not accept the login"))?;
        Ok(cookie)
    }

    /// Sends a request with the session cookie, logging in again once if it expired.
    async fn call(
        &self,
        make: impl Fn(&reqwest::Client, &str) -> reqwest::RequestBuilder,
    ) -> anyhow::Result<reqwest::Response> {
        for attempt in 0..2 {
            let (name, val) = {
                let mut guard = self.sid.lock().await;
                if guard.is_none() || attempt > 0 {
                    *guard = Some(self.login().await?);
                }
                guard.clone().unwrap_or_default()
            };
            let resp = make(&self.http, &self.base)
                .header(reqwest::header::COOKIE, format!("{name}={val}"))
                .send()
                .await?;
            if resp.status() != reqwest::StatusCode::FORBIDDEN {
                return Ok(resp.error_for_status()?);
            }
        }
        anyhow::bail!("qBittorrent refused the session")
    }

    /// Starts downloading, files in order so the start of each is ready first.
    pub async fn add(&self, src: &Source) -> anyhow::Result<()> {
        let torrent = src.torrent.clone();
        let magnet = src
            .magnet
            .clone()
            .or_else(|| src.infohash.as_ref().map(|h| format!("magnet:?xt=urn:btih:{h}")));
        self.call(|http, base| {
            let mut form = reqwest::multipart::Form::new()
                .text("category", "kino")
                .text("sequentialDownload", "true")
                .text("firstLastPiecePrio", "true");
            if let Some(p) = &self.save_path {
                form = form.text("savepath", p.clone());
            }
            form = match (&torrent, &magnet) {
                (Some(bytes), _) => form.part(
                    "torrents",
                    reqwest::multipart::Part::bytes(bytes.clone()).file_name("release.torrent"),
                ),
                (None, Some(m)) => form.text("urls", m.clone()),
                (None, None) => form,
            };
            http.post(format!("{base}/api/v2/torrents/add")).multipart(form)
        })
        .await?;
        Ok(())
    }

    pub async fn info(&self, hashes: &[String]) -> anyhow::Result<Vec<TorrentInfo>> {
        let joined = hashes.join("|");
        Ok(self
            .call(|http, base| {
                http.get(format!("{base}/api/v2/torrents/info"))
                    .query(&[("hashes", &joined)])
            })
            .await?
            .json()
            .await?)
    }

    /// Files with paths relative to the download folder. Indexes are 1-based, as TorrServer's.
    pub async fn files(&self, hash: &str) -> anyhow::Result<Vec<FileEntry>> {
        let raw: Vec<RawFile> = self
            .call(|http, base| {
                http.get(format!("{base}/api/v2/torrents/files"))
                    .query(&[("hash", hash)])
            })
            .await?
            .json()
            .await?;
        Ok(raw
            .into_iter()
            .map(|f| FileEntry {
                index: f.index + 1,
                path: f.name,
                length: f.size,
            })
            .collect())
    }

    /// Removes the torrent and its files.
    pub async fn delete(&self, hash: &str) -> anyhow::Result<()> {
        self.call(|http, base| {
            http.post(format!("{base}/api/v2/torrents/delete"))
                .form(&[("hashes", hash), ("deleteFiles", "true")])
        })
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::is_session_cookie;

    #[test]
    fn recognizes_legacy_and_v5_session_cookies() {
        assert!(is_session_cookie("SID"));
        assert!(is_session_cookie("QBT_SID_8085"));
        assert!(is_session_cookie("QBT_SID_8080"));
        assert!(!is_session_cookie("session"));
    }
}
