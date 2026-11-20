//! Torrents: finding releases (Jackett), reading `.torrent` files, streaming
//! (TorrServer + ffmpeg) and preloading (qBittorrent).

pub mod bencode;
pub mod ffmpeg;
pub mod files;
pub mod qbit;
pub mod title;
pub mod token;
pub mod torrserver;
pub mod torznab;

use std::collections::HashMap;

pub use kino_core::is_infohash;
use kino_core::{MediaKind, ReleaseOption, TitleDetails};
use sha1::{Digest, Sha1};
use sqlx::PgPool;

use self::{
    title::{normalize, parse_title},
    torznab::{Jackett, TorznabItem, MOVIE_CATS, TV_CATS},
};

#[derive(Debug, thiserror::Error)]
pub enum TorrentError {
    #[error(transparent)]
    Indexer(#[from] torznab::TorznabError),
    #[error("could not download the .torrent: {0}")]
    Fetch(#[from] reqwest::Error),
    #[error(transparent)]
    Bencode(#[from] bencode::BencodeError),
    #[error("unknown release")]
    Unknown,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// The key a release is known by before its `.torrent` is downloaded.
pub fn release_key(item: &TorznabItem) -> String {
    match &item.infohash {
        Some(h) => h.clone(),
        None => {
            let src = item.link.as_deref().or(item.magnet.as_deref()).unwrap_or(&item.title);
            format!("u{}", &hex::encode(Sha1::digest(src.as_bytes()))[..39])
        }
    }
}

pub fn to_release(item: &TorznabItem) -> ReleaseOption {
    let p = parse_title(&item.title);
    let direct_play = p.codec.as_deref() == Some("H.264") && !p.hdr && p.quality != "2160p";
    ReleaseOption {
        infohash: release_key(item),
        title: item.title.clone(),
        quality: p.quality,
        size_bytes: item.size,
        seeders: item.seeders,
        voice: p.voice,
        language: p.language,
        tracker: if item.tracker.is_empty() {
            "—".into()
        } else {
            item.tracker.clone()
        },
        added: item.added.clone(),
        year: p.year,
        season: p.season,
        hdr: p.hdr,
        dolby_vision: p.dolby_vision,
        subtitles: p.subtitles,
        direct_play,
        codec: p.codec,
    }
}

/// Whether a release name is about this title: it has one of the names and,
/// for films, a year no more than one off.
pub fn is_about(name: &str, details: &TitleDetails) -> bool {
    let n = normalize(name);
    let s = &details.summary;
    let names = [Some(&s.title), s.original_title.as_ref()];
    let named = names
        .into_iter()
        .flatten()
        .map(|t| normalize(t))
        .any(|t| !t.is_empty() && n.contains(&t));
    let year_ok = match (s.kind, s.year, parse_title(name).year) {
        (MediaKind::Movie, Some(want), Some(have)) => (want - have).abs() <= 1,
        _ => true,
    };
    named && year_ok
}

/// Searches every tracker for the title, by its Russian and original names.
pub async fn search(jackett: &Jackett, details: &TitleDetails) -> Result<Vec<TorznabItem>, TorrentError> {
    let s = &details.summary;
    let (cats, year) = match s.kind {
        MediaKind::Movie => (MOVIE_CATS, s.year.map(|y| format!(" {y}")).unwrap_or_default()),
        MediaKind::Tv => (TV_CATS, String::new()),
    };
    let mut queries = vec![format!("{}{year}", s.title)];
    if let Some(o) = s
        .original_title
        .as_ref()
        .filter(|o| normalize(o) != normalize(&s.title))
    {
        queries.push(format!("{o}{year}"));
    }
    let results = futures_util::future::join_all(queries.iter().map(|q| jackett.search(q, cats))).await;
    let mut seen = HashMap::new();
    let mut first_err = None;
    for r in results {
        match r {
            Ok(items) => {
                for it in items.into_iter().filter(|it| is_about(&it.title, details)) {
                    seen.entry(release_key(&it)).or_insert(it);
                }
            }
            Err(e) => first_err = first_err.or(Some(e)),
        }
    }
    if seen.is_empty() {
        if let Some(e) = first_err {
            return Err(e.into());
        }
    }
    let mut items: Vec<_> = seen.into_values().collect();
    items.sort_by_key(|it| std::cmp::Reverse(it.seeders));
    Ok(items)
}

/// Remembers where each release can be downloaded from.
pub async fn save_sources(pool: &PgPool, items: &[TorznabItem]) -> Result<(), sqlx::Error> {
    for it in items {
        sqlx::query(
            "INSERT INTO torrent_source (key, title, link, magnet, infohash) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (key) DO UPDATE SET title = EXCLUDED.title, link = EXCLUDED.link,
                 magnet = COALESCE(EXCLUDED.magnet, torrent_source.magnet), updated_at = now()",
        )
        .bind(release_key(it))
        .bind(&it.title)
        .bind(&it.link)
        .bind(&it.magnet)
        .bind(&it.infohash)
        .execute(pool)
        .await?;
    }
    Ok(())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Source {
    pub key: String,
    pub title: String,
    pub link: Option<String>,
    pub magnet: Option<String>,
    pub infohash: Option<String>,
    pub torrent: Option<Vec<u8>>,
}

pub async fn source(pool: &PgPool, key: &str) -> Result<Option<Source>, sqlx::Error> {
    sqlx::query_as("SELECT key, title, link, magnet, infohash, torrent FROM torrent_source WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
}

/// Turns a release key into its info-hash, downloading the `.torrent` when the
/// indexer did not give the hash. Also keeps the `.torrent` for TorrServer.
pub async fn resolve(pool: &PgPool, http: &reqwest::Client, key: &str) -> Result<Source, TorrentError> {
    let src = source(pool, key).await?;
    if let Some(s) = src.as_ref().filter(|s| s.infohash.is_some() && s.torrent.is_some()) {
        return Ok(s.clone());
    }
    let Some(mut src) = src else {
        // Never seen by the indexer (demo data, or a magnet typed by hand).
        return if is_infohash(key) {
            Ok(Source {
                key: key.into(),
                title: String::new(),
                link: None,
                magnet: Some(format!("magnet:?xt=urn:btih:{key}")),
                infohash: Some(key.into()),
                torrent: None,
            })
        } else {
            Err(TorrentError::Unknown)
        };
    };
    if let Some(link) = src.link.clone() {
        match fetch(http, &link).await {
            Ok(Fetched::Torrent(bytes)) => {
                src.infohash = Some(bencode::parse_torrent(&bytes)?.infohash);
                src.torrent = Some(bytes);
            }
            Ok(Fetched::Magnet(m)) => {
                src.infohash = bencode::magnet_infohash(&m);
                src.magnet = Some(m);
            }
            // The magnet is enough when the indexer gave one.
            Err(e) if src.infohash.is_none() => return Err(e),
            Err(e) => tracing::warn!(key, "no .torrent, using the magnet: {e}"),
        }
    }
    let hash = src.infohash.clone().ok_or(TorrentError::Unknown)?;
    for k in [key, hash.as_str()] {
        sqlx::query(
            "INSERT INTO torrent_source (key, title, link, magnet, infohash, torrent) VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (key) DO UPDATE SET magnet = EXCLUDED.magnet, infohash = EXCLUDED.infohash,
                 torrent = EXCLUDED.torrent, updated_at = now()",
        )
        .bind(k)
        .bind(&src.title)
        .bind(&src.link)
        .bind(&src.magnet)
        .bind(&hash)
        .bind(&src.torrent)
        .execute(pool)
        .await?;
    }
    Ok(src)
}

enum Fetched {
    Torrent(Vec<u8>),
    Magnet(String),
}

/// Downloads a `.torrent`. Jackett answers some links with a redirect to a magnet.
async fn fetch(http: &reqwest::Client, link: &str) -> Result<Fetched, TorrentError> {
    let mut url = link.to_owned();
    for _ in 0..5 {
        let resp = http
            .get(&url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;
        if !resp.status().is_redirection() {
            return Ok(Fetched::Torrent(resp.error_for_status()?.bytes().await?.to_vec()));
        }
        let to = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if to.starts_with("magnet:") {
            return Ok(Fetched::Magnet(to.to_owned()));
        }
        url = resp.url().join(to).map_err(|_| TorrentError::Unknown)?.to_string();
    }
    Err(TorrentError::Unknown)
}

/// HTTP client that does not follow redirects, so magnet redirects can be read.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("kino")
        .build()
        .expect("http client")
}
