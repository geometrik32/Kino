//! Jackett's Torznab API: search all configured trackers at once.

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
}

#[derive(Debug, thiserror::Error)]
pub enum TorznabError {
    #[error("indexer is unreachable: {0}")]
    Http(#[from] reqwest::Error),
    #[error("indexer answered with an error: {0}")]
    Api(String),
    #[error("indexer answer is not valid XML: {0}")]
    Xml(#[from] roxmltree::Error),
}

#[derive(Clone)]
pub struct Jackett {
    http: reqwest::Client,
    base: String,
    api_key: String,
}

/// Torznab categories: movies (2000) and TV (5000), with their sub-categories.
pub const MOVIE_CATS: &str = "2000,2010,2020,2030,2040,2045,2050,2060,2070,2080";
pub const TV_CATS: &str = "5000,5010,5020,5030,5040,5045,5050,5060,5070,5080";

impl Jackett {
    pub fn new(http: reqwest::Client, base: &str, api_key: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
        }
    }

    pub async fn search(&self, query: &str, cats: &str) -> Result<Vec<TorznabItem>, TorznabError> {
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
                    "attr" => {
                        let value = c.attribute("value").unwrap_or("");
                        match c.attribute("name") {
                            Some("seeders") => it.seeders = value.parse().unwrap_or(0),
                            Some("size") if it.size == 0 => it.size = value.parse().unwrap_or(0),
                            Some("infohash") if super::is_infohash(&value.to_lowercase()) => {
                                it.infohash = Some(value.to_lowercase())
                            }
                            Some("magneturl") if !value.is_empty() => it.magnet = Some(value.to_owned()),
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
}
