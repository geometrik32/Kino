//! Jackett's Torznab API: search all configured trackers at once.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use chrono::DateTime;

/// One search result as Jackett returns it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TorznabItem {
    pub title: String,
    /// `.torrent` download link (through Jackett). May redirect to a magnet.
    pub link: Option<String>,
    pub magnet: Option<String>,
    pub infohash: Option<String>,
    pub size: u64,
    pub seeders: u32,
    pub tracker: String,
    /// `YYYY-MM-DD`.
    pub added: String,
    /// Standard/custom Torznab category IDs returned by the indexer.
    pub categories: Vec<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum TorznabError {
    #[error("indexer is unreachable: {0}")]
    Http(String),
    #[error("indexer answered with an error: {0}")]
    Api(String),
    #[error("indexer answer is not valid XML: {0}")]
    Xml(#[from] roxmltree::Error),
}

impl From<reqwest::Error> for TorznabError {
    fn from(e: reqwest::Error) -> Self {
        // Strip the URL so `apikey` never ends up in logs.
        Self::Http(e.without_url().to_string())
    }
}

/// Optional Sablier wake handle for starting a sleeping Jackett container on demand.
#[derive(Clone)]
pub struct JackettWake {
    http: reqwest::Client,
    url: String,
    group: String,
    duration: String,
    last_bg_wake: Arc<AtomicU64>,
}

impl JackettWake {
    pub fn new(http: reqwest::Client, base_url: &str, group: String, duration: String) -> Self {
        Self {
            http,
            url: format!("{}/api/strategies/blocking", base_url.trim_end_matches('/')),
            group,
            duration,
            last_bg_wake: Arc::new(AtomicU64::new(0)),
        }
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Triggers a non-blocking wake request at most once per minute.
    pub fn wake_background(&self) {
        let now = Self::now_secs();
        let prev = self.last_bg_wake.load(Ordering::Relaxed);
        if now.saturating_sub(prev) < 60 {
            return;
        }
        if self
            .last_bg_wake
            .compare_exchange(prev, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let wake = self.clone();
        tokio::spawn(async move {
            wake.ensure_ready().await;
        });
    }

    /// Ensures the Jackett container is running and healthy before sending a request.
    pub async fn ensure_ready(&self) {
        let res = self
            .http
            .get(&self.url)
            .query(&[
                ("group", self.group.as_str()),
                ("session_duration", self.duration.as_str()),
            ])
            .timeout(std::time::Duration::from_secs(45))
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => {
                self.last_bg_wake.store(Self::now_secs(), Ordering::Relaxed);
            }
            Ok(r) => tracing::warn!(status = %r.status(), "Sablier wake returned non-success"),
            Err(e) => tracing::warn!(error = %e.without_url(), "Sablier wake request failed"),
        }
    }
}

#[derive(Clone)]
pub struct Jackett {
    http: reqwest::Client,
    base: String,
    api_key: String,
    wake: Option<JackettWake>,
}

/// Torznab categories: movies (2000) and TV (5000) with sub-categories, plus
/// `8000`/`8010` (`Other`) because Russian indexers in Jackett (RuTor,
/// MegaPeer, torrent.by) return search results under `Other`.
pub const MOVIE_CATS: &str = "2000,2010,2020,2030,2040,2045,2050,2060,2070,2080,8000,8010";
pub const TV_CATS: &str = "5000,5010,5020,5030,5040,5045,5050,5060,5070,5080,8000,8010";

impl Jackett {
    pub fn new(http: reqwest::Client, base: &str, api_key: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            wake: None,
        }
    }

    pub fn with_wake(mut self, wake: JackettWake) -> Self {
        self.wake = Some(wake);
        self
    }

    pub fn wake_in_background(&self) {
        if let Some(w) = &self.wake {
            w.wake_background();
        }
    }

    pub async fn ensure_ready(&self) {
        if let Some(w) = &self.wake {
            w.ensure_ready().await;
        }
    }

    pub fn owns_link(&self, link: &str) -> bool {
        link.starts_with(&self.base)
    }

    pub async fn search(&self, query: &str, cats: &str) -> Result<Vec<TorznabItem>, TorznabError> {
        self.ensure_ready().await;
        let url = format!("{}/api/v2.0/indexers/all/results/torznab/api", self.base);
        let body = self
            .http
            .get(url)
            .query(&[
                ("apikey", self.api_key.as_str()),
                ("t", "search"),
                ("q", query),
                ("cat", cats),
            ])
            .timeout(std::time::Duration::from_secs(40))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        parse_feed(&body)
    }
}

/// Reads the RSS/Torznab answer.
pub fn parse_feed(xml: &str) -> Result<Vec<TorznabItem>, TorznabError> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    if root.tag_name().name() == "error" {
        return Err(TorznabError::Api(
            root.attribute("description").unwrap_or("unknown").to_owned(),
        ));
    }
    let items = root
        .descendants()
        .filter(|n| n.has_tag_name("item"))
        .map(|item| {
            let mut it = TorznabItem::default();
            let mut add_cat = |raw: &str| {
                if let Ok(cat) = raw.parse::<u32>() {
                    if !it.categories.contains(&cat) {
                        it.categories.push(cat);
                    }
                }
            };
            for c in item.children().filter(|c| c.is_element()) {
                let text = c.text().unwrap_or("").trim();
                match c.tag_name().name() {
                    "title" => it.title = text.to_owned(),
                    "link" if !text.is_empty() => it.link = Some(text.to_owned()),
                    "size" => it.size = text.parse().unwrap_or(it.size),
                    "jackettindexer" => it.tracker = tracker_name(text),
                    "pubDate" => {
                        it.added = DateTime::parse_from_rfc2822(text)
                            .map(|d| d.format("%Y-%m-%d").to_string())
                            .unwrap_or_default()
                    }
                    "category" => add_cat(text),
                    "attr" => {
                        let value = c.attribute("value").unwrap_or("");
                        match c.attribute("name") {
                            Some("seeders") => it.seeders = value.parse().unwrap_or(0),
                            Some("size") if it.size == 0 => it.size = value.parse().unwrap_or(0),
                            Some("infohash") if super::is_infohash(&value.to_lowercase()) => {
                                it.infohash = Some(value.to_lowercase())
                            }
                            Some("magneturl") if !value.is_empty() => it.magnet = Some(value.to_owned()),
                            Some("category") => add_cat(value),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            if it.link.as_deref().is_some_and(|l| l.starts_with("magnet:")) {
                it.magnet = it.link.take();
            }
            if it.infohash.is_none() {
                it.infohash = it.magnet.as_deref().and_then(super::bencode::magnet_infohash);
            }
            it
        })
        .filter(|it| !it.title.is_empty() && (it.link.is_some() || it.magnet.is_some()))
        .collect();
    Ok(items)
}

/// "RuTracker.org" → "RuTracker".
fn tracker_name(s: &str) -> String {
    let s = s.trim();
    for tld in [".org", ".net", ".tv", ".guru", ".me", ".ru", ".com", ".to"] {
        if let Some(base) = s.strip_suffix(tld) {
            return base.to_owned();
        }
    }
    s.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed() {
        let items = parse_feed(include_str!("../../tests/fixtures/torznab.xml")).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].tracker, "RuTracker");
        assert_eq!(items[0].seeders, 214);
        assert_eq!(items[0].added, "2024-03-08");
        assert_eq!(items[0].size, 28_400_000_000);
        assert_eq!(items[0].infohash, None);
        assert!(items[0].link.is_some());
        assert_eq!(
            items[1].infohash.as_deref(),
            Some("c12fe1c06bba254a9dc9f519b335aa7c1367a88a")
        );
        assert_eq!(items[2].magnet.as_deref().map(|m| &m[..8]), Some("magnet:?"));
        assert!(items[2].infohash.is_some());
    }

    #[test]
    fn error_answer() {
        let e = parse_feed(r#"<?xml version="1.0"?><error code="100" description="Invalid API Key"/>"#).unwrap_err();
        assert!(e.to_string().contains("Invalid API Key"));
    }

    #[test]
    fn parses_categories() {
        let xml = r#"<?xml version="1.0"?>
        <rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel>
          <item>
            <title>Test (1997)</title>
            <link>https://example.com/1.torrent</link>
            <category>8000</category>
            <torznab:attr name="category" value="8000" />
            <torznab:attr name="category" value="100003" />
          </item>
        </channel></rss>"#;
        let items = parse_feed(xml).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].categories, vec![8000, 100003]);
    }
}
