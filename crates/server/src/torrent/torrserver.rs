//! TorrServer: streams a torrent file over HTTP while it downloads.

use serde::Deserialize;
use serde_json::json;

use super::{files::FileEntry, Source};

#[derive(Clone)]
pub struct TorrServer {
    http: reqwest::Client,
    base: String,
    auth: Option<(String, String)>,
}

#[derive(Debug, Deserialize)]
struct Status {
    #[serde(default)]
    hash: String,
    #[serde(default)]
    file_stats: Vec<FileStat>,
}

#[derive(Debug, Deserialize)]
struct FileStat {
    id: i32,
    path: String,
    length: u64,
}

impl TorrServer {
    pub fn new(base: &str, auth: Option<(String, String)>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').to_owned(),
            auth,
        }
    }

    fn req(&self, r: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            Some((u, p)) => r.basic_auth(u, Some(p)),
            None => r,
        }
    }

    /// Adds the torrent (from its `.torrent` when we have it) and waits until
    /// TorrServer knows the file list, which needs the metadata from peers.
    pub async fn add(&self, src: &Source) -> Result<Vec<FileEntry>, reqwest::Error> {
        let status: Status = match &src.torrent {
            Some(bytes) => {
                let part = reqwest::multipart::Part::bytes(bytes.clone()).file_name("release.torrent");
                let form = reqwest::multipart::Form::new()
                    .part("file", part)
                    .text("title", src.title.clone())
                    .text("save", "false");
                self.req(self.http.post(format!("{}/torrent/upload", self.base)))
                    .multipart(form)
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?
            }
            None => {
                let link = src
                    .magnet
                    .clone()
                    .unwrap_or_else(|| format!("magnet:?xt=urn:btih:{}", src.key));
                self.req(self.http.post(format!("{}/torrents", self.base)))
                    .json(&json!({ "action": "add", "link": link, "title": src.title, "save_to_db": false }))
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?
            }
        };
        let hash = if status.hash.is_empty() {
            src.infohash.clone().unwrap_or_default()
        } else {
            status.hash
        };
        if !status.file_stats.is_empty() {
            return Ok(entries(status.file_stats));
        }
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let s: Status = self
                .req(self.http.post(format!("{}/torrents", self.base)))
                .json(&json!({ "action": "get", "hash": hash }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if !s.file_stats.is_empty() {
                return Ok(entries(s.file_stats));
            }
        }
        Ok(Vec::new())
    }

    /// URL of the raw file stream (supports Range).
    pub fn stream_url(&self, infohash: &str, index: i32) -> String {
        format!("{}/stream/video?link={infohash}&index={index}&play", self.base)
    }

    /// The same URL with the credentials inside, for ffmpeg.
    pub fn stream_url_for_ffmpeg(&self, infohash: &str, index: i32) -> String {
        let url = self.stream_url(infohash, index);
        match (&self.auth, url::Url::parse(&url)) {
            (Some((u, p)), Ok(mut parsed)) => {
                let _ = parsed.set_username(u);
                let _ = parsed.set_password(Some(p));
                parsed.to_string()
            }
            _ => url,
        }
    }

    /// Request for the stream, with credentials.
    pub fn get(&self, url: &str) -> reqwest::RequestBuilder {
        self.req(self.http.get(url))
    }
}

fn entries(stats: Vec<FileStat>) -> Vec<FileEntry> {
    stats
        .into_iter()
        .map(|f| FileEntry {
            index: f.id,
            path: f.path,
            length: f.length,
        })
        .collect()
}
