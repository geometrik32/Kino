//! TorrServer: streams a torrent file over HTTP while it downloads.

use kino_core::{StreamPhase, StreamProgress};
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

/// What TorrServer reports about a torrent (`POST /torrents {"action":"get"}`).
#[derive(Debug, Default, Deserialize)]
struct Stats {
    /// 0 added, 1 getting the file list, 2 preloading, 3 working, 4 closed, 5 in the database.
    #[serde(default)]
    stat: i32,
    #[serde(default)]
    active_peers: u32,
    #[serde(default)]
    connected_seeders: u32,
    #[serde(default)]
    download_speed: f64,
    #[serde(default)]
    preloaded_bytes: u64,
    #[serde(default)]
    preload_size: u64,
    #[serde(default)]
    file_stats: Vec<FileStat>,
}

impl Stats {
    fn progress(self) -> StreamProgress {
        let phase = match self.stat {
            // Still looking for peers and for the list of files.
            0 | 1 => StreamPhase::Connecting,
            _ if self.file_stats.is_empty() => StreamPhase::Connecting,
            2 | 3 if self.preload_size > 0 && self.preloaded_bytes >= self.preload_size => StreamPhase::Ready,
            2 | 3 => StreamPhase::Buffering,
            _ => StreamPhase::Connecting,
        };
        StreamProgress {
            phase,
            loaded_bytes: self.preloaded_bytes,
            needed_bytes: self.preload_size,
            peers: self.active_peers,
            seeders: self.connected_seeders,
            speed_bps: self.download_speed.max(0.0) as u64,
        }
    }
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

    /// How far the torrent is from playing: peers found, first part loaded.
    /// A torrent TorrServer does not know yet is "connecting".
    pub async fn progress(&self, hash: &str) -> Result<StreamProgress, reqwest::Error> {
        let resp = self
            .req(self.http.post(format!("{}/torrents", self.base)))
            .json(&json!({ "action": "get", "hash": hash }))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(StreamProgress::default());
        }
        Ok(resp.json::<Stats>().await?.progress())
    }

    /// Asks TorrServer to load the beginning of a file ahead of the player, so
    /// [`TorrServer::progress`] has a percentage to show. The answer is not
    /// needed: only the start of the response is waited for, then it is dropped.
    pub fn warm_up(&self, hash: &str, index: i32) {
        let url = format!("{}/stream/video?link={hash}&index={index}&preload", self.base);
        let req = self.get(&url).timeout(std::time::Duration::from_secs(120));
        tokio::spawn(async move {
            let _ = req.send().await;
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(json: serde_json::Value) -> StreamProgress {
        serde_json::from_value::<Stats>(json).unwrap().progress()
    }

    #[test]
    fn maps_torrserver_states() {
        let files = json!([{ "id": 1, "path": "a.mkv", "length": 10 }]);
        // Looking for the file list.
        let p = stats(json!({ "stat": 1, "active_peers": 2, "connected_seeders": 1, "download_speed": 0.0 }));
        assert_eq!((p.phase, p.peers, p.seeders), (StreamPhase::Connecting, 2, 1));
        // Preloading: 8 of 32 MB.
        let p = stats(json!({
            "stat": 2, "file_stats": files, "preloaded_bytes": 8 << 20, "preload_size": 32 << 20,
            "download_speed": 2_500_000.5, "active_peers": 9
        }));
        assert_eq!(p.phase, StreamPhase::Buffering);
        assert_eq!(p.percent(), Some(25));
        assert_eq!(p.speed_bps, 2_500_000);
        // Preload finished.
        let p = stats(json!({ "stat": 3, "file_stats": files, "preloaded_bytes": 33 << 20, "preload_size": 32 << 20 }));
        assert_eq!(p.phase, StreamPhase::Ready);
        // Working, but no preload amount reported: loading, percentage unknown.
        let p = stats(json!({ "stat": 3, "file_stats": files }));
        assert_eq!((p.phase, p.percent()), (StreamPhase::Buffering, None));
        // Extra fields are ignored, missing ones are zero.
        assert_eq!(
            stats(json!({ "hash": "x", "title": "y" })).phase,
            StreamPhase::Connecting
        );
    }
}
