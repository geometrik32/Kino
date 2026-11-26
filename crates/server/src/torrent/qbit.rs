//! qBittorrent Web API: downloading releases in advance to the server's disk.

use kino_core::PreloadPhase;
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
    /// "downloading", "metaDL", "stalledDL", "queuedDL", "checkingDL", "uploading", "error", "missingFiles", ...
    pub state: String,
    pub size: i64,
    #[serde(default)]
    pub dlspeed: i64,
}

impl TorrentInfo {
    pub fn failed(&self) -> bool {
        matches!(self.state.as_str(), "error" | "missingFiles")
    }

    pub fn done(&self) -> bool {
        self.progress >= 1.0
    }

    pub fn speed_bps(&self) -> u64 {
        self.dlspeed.max(0) as u64
    }

    pub fn dl_phase(&self) -> PreloadPhase {
        if self.failed() {
            return PreloadPhase::Failed;
        }
        if self.done() {
            return PreloadPhase::Ready;
        }
        match self.state.as_str() {
            "metaDL" | "forcedMetaDL" => PreloadPhase::Metadata,
            "checkingDL" | "checkingUP" | "checkingResumeData" | "allocating" | "moving" => PreloadPhase::Checking,
            "stalledDL" | "queuedDL" | "pausedDL" | "stoppedDL" => PreloadPhase::Waiting,
            _ if self.dlspeed > 0 => PreloadPhase::Downloading,
            _ if self.progress <= 0.0 => PreloadPhase::Waiting,
            _ => PreloadPhase::Downloading,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawFile {
    index: i32,
    name: String,
    size: u64,
    #[serde(default)]
    progress: Option<f32>,
    #[serde(default)]
    priority: Option<i32>,
}

/// Detailed file entry from qBittorrent (`GET /api/v2/torrents/files`).
#[derive(Debug, Clone, PartialEq)]
pub struct QbitFile {
    /// 1-based index matching `FileEntry.index` and TorrServer.
    pub index: i32,
    /// 0-based qBittorrent file index used by `torrents/filePrio`.
    pub qbit_index: i32,
    pub path: String,
    pub length: u64,
    pub progress: f32,
    /// 0 = do not download, >0 = wanted.
    pub priority: i32,
}

impl QbitFile {
    pub fn as_file_entry(&self) -> FileEntry {
        FileEntry {
            index: self.index,
            path: self.path.clone(),
            length: self.length,
        }
    }
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
    /// Idempotent when qBittorrent responds `409 Conflict` for an already-queued info-hash.
    pub async fn add(&self, src: &Source) -> anyhow::Result<()> {
        self.add_with_options(src, false).await
    }

    /// Adds a torrent, optionally in the stopped/paused state so file priorities can be set first.
    pub async fn add_with_options(&self, src: &Source, stopped: bool) -> anyhow::Result<()> {
        let torrent = src.torrent.clone();
        let magnet = src
            .magnet
            .clone()
            .or_else(|| src.infohash.as_ref().map(|h| format!("magnet:?xt=urn:btih:{h}")));
        let res = self
            .call(|http, base| {
                let mut form = reqwest::multipart::Form::new()
                    .text("category", "kino")
                    .text("sequentialDownload", "true")
                    .text("firstLastPiecePrio", "true");
                if stopped {
                    form = form.text("stopped", "true").text("paused", "true");
                }
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
            .await;
        match res {
            Ok(_) => Ok(()),
            Err(e) => {
                let is_conflict = e
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(|re| re.status() == Some(reqwest::StatusCode::CONFLICT));
                if is_conflict {
                    if let Some(hash) = src.infohash.as_deref().filter(|h| !h.is_empty()) {
                        if let Ok(list) = self.info(&[hash.to_owned()]).await {
                            if list.iter().any(|t| t.hash.eq_ignore_ascii_case(hash) && !t.failed()) {
                                return Ok(());
                            }
                        }
                    }
                }
                Err(e)
            }
        }
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

    /// Files with paths relative to the download folder plus per-file progress and priority.
    pub async fn file_states(&self, hash: &str) -> anyhow::Result<Vec<QbitFile>> {
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
            .map(|f| QbitFile {
                index: f.index + 1,
                qbit_index: f.index,
                path: f.name,
                length: f.size,
                progress: f.progress.unwrap_or(0.0),
                priority: f.priority.unwrap_or(1),
            })
            .collect())
    }

    /// Files with paths relative to the download folder. Indexes are 1-based, as TorrServer's.
    pub async fn files(&self, hash: &str) -> anyhow::Result<Vec<FileEntry>> {
        Ok(self
            .file_states(hash)
            .await?
            .into_iter()
            .map(|f| f.as_file_entry())
            .collect())
    }

    /// Sets download priority for the given 0-based qBittorrent file indices (`0` = skip, `1` = normal).
    pub async fn set_file_priority(&self, hash: &str, qbit_indices: &[i32], priority: i32) -> anyhow::Result<()> {
        if qbit_indices.is_empty() {
            return Ok(());
        }
        let ids = qbit_indices.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("|");
        let prio = priority.to_string();
        self.call(|http, base| {
            http.post(format!("{base}/api/v2/torrents/filePrio")).form(&[
                ("hash", hash),
                ("id", ids.as_str()),
                ("priority", prio.as_str()),
            ])
        })
        .await?;
        Ok(())
    }

    /// Starts (resumes) a torrent in qBittorrent (supports both v5 `/start` and v4 `/resume`).
    pub async fn start(&self, hash: &str) -> anyhow::Result<()> {
        let res = self
            .call(|http, base| {
                http.post(format!("{base}/api/v2/torrents/start"))
                    .form(&[("hashes", hash)])
            })
            .await;
        match res {
            Ok(_) => Ok(()),
            Err(e)
                if e.downcast_ref::<reqwest::Error>()
                    .is_some_and(|re| re.status() == Some(reqwest::StatusCode::NOT_FOUND)) =>
            {
                self.call(|http, base| {
                    http.post(format!("{base}/api/v2/torrents/resume"))
                        .form(&[("hashes", hash)])
                })
                .await?;
                Ok(())
            }
            Err(e) => Err(e),
        }
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
    use super::{is_session_cookie, PreloadPhase, TorrentInfo};

    #[test]
    fn recognizes_legacy_and_v5_session_cookies() {
        assert!(is_session_cookie("SID"));
        assert!(is_session_cookie("QBT_SID_8085"));
        assert!(is_session_cookie("QBT_SID_8080"));
        assert!(!is_session_cookie("session"));
    }

    #[test]
    fn classifies_qbittorrent_states_into_preload_phases() {
        let info = |state: &str, progress: f32, dlspeed: i64| TorrentInfo {
            hash: "a".repeat(40),
            progress,
            state: state.into(),
            size: 1000,
            dlspeed,
        };
        assert_eq!(info("metaDL", 0.0, 0).dl_phase(), PreloadPhase::Metadata);
        assert_eq!(info("stalledDL", 0.0, 0).dl_phase(), PreloadPhase::Waiting);
        assert_eq!(info("checkingDL", 0.3, 0).dl_phase(), PreloadPhase::Checking);
        assert_eq!(
            info("downloading", 0.25, 5_000_000).dl_phase(),
            PreloadPhase::Downloading
        );
        assert_eq!(info("uploading", 1.0, 0).dl_phase(), PreloadPhase::Ready);
        assert_eq!(info("error", 0.0, 0).dl_phase(), PreloadPhase::Failed);
    }
}
