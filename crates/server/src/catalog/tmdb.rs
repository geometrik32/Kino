//! TMDB client (<https://developer.themoviedb.org/reference>), with a small
//! in-memory cache so repeated page views don't hit the API.
//!
//! Requests honour `HTTPS_PROXY`, which helps where TMDB is blocked.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use kino_core::{
    CatalogRow, Episode, HomeFeed, MediaKind, SearchResults, Season, SeasonSummary, TitleDetails, TitleSummary,
};
use serde::{de::DeserializeOwned, Deserialize};

use super::{year_of, Catalog, CatalogError};

const API: &str = "https://api.themoviedb.org/3";
const IMAGES: &str = "https://image.tmdb.org/t/p";
const CACHE_TTL: Duration = Duration::from_secs(6 * 3600);
const ALLOWED_SIZES: [&str; 5] = ["w185", "w300", "w342", "w780", "w1280"];

pub struct TmdbCatalog {
    http: reqwest::Client,
    /// v3 API key or v4 read access token (starts with `eyJ`).
    key: String,
    language: String,
    cache: Mutex<HashMap<String, (Instant, serde_json::Value)>>,
}

impl TmdbCatalog {
    pub fn new(key: String, language: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("kino-server")
            .build()
            .expect("reqwest client builds");
        Self {
            http,
            key,
            language,
            cache: Mutex::default(),
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T, CatalogError> {
        let mut params: Vec<(&str, &str)> = vec![("language", self.language.as_str())];
        params.extend_from_slice(query);
        let cache_key = format!("{path}?{params:?}");
        if let Some((at, v)) = self.cache.lock().expect("cache lock").get(&cache_key) {
            if at.elapsed() < CACHE_TTL {
                return serde_json::from_value(v.clone()).map_err(|e| CatalogError::Upstream(e.to_string()));
            }
        }

        let mut req = self.http.get(format!("{API}{path}")).query(&params);
        req = if self.key.starts_with("eyJ") {
            req.bearer_auth(&self.key)
        } else {
            req.query(&[("api_key", &self.key)])
        };
        let resp = req.send().await.map_err(|e| CatalogError::Upstream(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CatalogError::NotFound);
        }
        if !resp.status().is_success() {
            return Err(CatalogError::Upstream(format!("TMDB answered {}", resp.status())));
        }
        let value: serde_json::Value = resp.json().await.map_err(|e| CatalogError::Upstream(e.to_string()))?;
        let parsed = serde_json::from_value(value.clone()).map_err(|e| CatalogError::Upstream(e.to_string()))?;
        self.cache
            .lock()
            .expect("cache lock")
            .insert(cache_key, (Instant::now(), value));
        Ok(parsed)
    }

    async fn list(&self, path: &str, kind: Option<MediaKind>) -> Result<Vec<TitleSummary>, CatalogError> {
        let page: Page = self.get(path, &[]).await?;
        Ok(page.results.into_iter().filter_map(|r| r.into_summary(kind)).collect())
    }
}

#[derive(Deserialize)]
struct Page {
    results: Vec<ListItem>,
}

/// A movie or TV result; TMDB uses different field names for each.
#[derive(Deserialize)]
pub(crate) struct ListItem {
    id: i64,
    media_type: Option<String>,
    title: Option<String>,
    name: Option<String>,
    original_title: Option<String>,
    original_name: Option<String>,
    release_date: Option<String>,
    first_air_date: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    vote_average: Option<f32>,
    overview: Option<String>,
}

impl ListItem {
    /// `kind` is known for typed lists; mixed lists carry `media_type`.
    pub(crate) fn into_summary(self, kind: Option<MediaKind>) -> Option<TitleSummary> {
        let kind = kind.or_else(|| self.media_type.as_deref().and_then(MediaKind::parse))?;
        let title = self.title.or(self.name)?;
        let date = self.release_date.or(self.first_air_date);
        Some(TitleSummary {
            kind,
            tmdb_id: self.id,
            title,
            original_title: self.original_title.or(self.original_name),
            year: year_of(date.as_deref()),
            poster_path: self.poster_path,
            backdrop_path: self.backdrop_path,
            rating: self.vote_average.filter(|r| *r > 0.0),
            overview: self.overview.filter(|o| !o.is_empty()),
        })
    }
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
struct Details {
    #[serde(flatten)]
    item: ListItem,
    #[serde(default)]
    genres: Vec<Named>,
    runtime: Option<i32>,
    #[serde(default)]
    episode_run_time: Vec<i32>,
    tagline: Option<String>,
    imdb_id: Option<String>,
    #[serde(default)]
    seasons: Vec<RawSeason>,
    external_ids: Option<ExternalIds>,
    last_episode_to_air: Option<RawEpisodeRef>,
}

#[derive(Deserialize)]
struct RawEpisodeRef {
    season_number: i32,
    episode_number: i32,
}

#[derive(Deserialize)]
struct ExternalIds {
    imdb_id: Option<String>,
}

#[derive(Deserialize)]
struct RawSeason {
    season_number: i32,
    name: String,
    episode_count: Option<i32>,
    air_date: Option<String>,
}

#[derive(Deserialize)]
struct RawSeasonDetails {
    season_number: i32,
    name: String,
    #[serde(default)]
    episodes: Vec<RawEpisode>,
}

#[derive(Deserialize)]
struct RawEpisode {
    episode_number: i32,
    name: Option<String>,
    overview: Option<String>,
    still_path: Option<String>,
    runtime: Option<i32>,
    air_date: Option<String>,
}

pub(crate) fn details_from(raw: serde_json::Value, kind: MediaKind) -> Result<TitleDetails, CatalogError> {
    let d: Details = serde_json::from_value(raw).map_err(|e| CatalogError::Upstream(e.to_string()))?;
    let summary = d.item.into_summary(Some(kind)).ok_or(CatalogError::NotFound)?;
    let mut seasons: Vec<SeasonSummary> = d
        .seasons
        .into_iter()
        .map(|s| SeasonSummary {
            number: s.season_number,
            name: s.name,
            episode_count: s.episode_count.unwrap_or(0),
            year: year_of(s.air_date.as_deref()),
        })
        .collect();
    // Regular seasons first, "Specials" (season 0) last.
    seasons.sort_by_key(|s| (s.number == 0, s.number));
    Ok(TitleDetails {
        summary,
        genres: d.genres.into_iter().map(|g| g.name).collect(),
        runtime_min: d
            .runtime
            .or_else(|| d.episode_run_time.first().copied())
            .filter(|m| *m > 0),
        tagline: d.tagline.filter(|t| !t.is_empty()),
        seasons,
        imdb_id: d.imdb_id.or(d.external_ids.and_then(|e| e.imdb_id)),
        last_episode: d.last_episode_to_air.map(|e| (e.season_number, e.episode_number)),
    })
}

#[async_trait]
impl Catalog for TmdbCatalog {
    async fn home(&self) -> Result<HomeFeed, CatalogError> {
        let (tv, movies, top) = tokio::try_join!(
            self.list("/trending/tv/week", Some(MediaKind::Tv)),
            self.list("/trending/movie/week", Some(MediaKind::Movie)),
            self.list("/movie/top_rated", Some(MediaKind::Movie)),
        )?;
        Ok(HomeFeed {
            rows: vec![
                CatalogRow {
                    title: "Популярные сериалы".into(),
                    items: tv,
                },
                CatalogRow {
                    title: "Популярные фильмы".into(),
                    items: movies,
                },
                CatalogRow {
                    title: "С высоким рейтингом".into(),
                    items: top,
                },
            ],
            demo: false,
        })
    }

    async fn search(&self, query: &str) -> Result<SearchResults, CatalogError> {
        let items = if query.trim().is_empty() {
            Vec::new()
        } else {
            let page: Page = self
                .get("/search/multi", &[("query", query.trim()), ("include_adult", "false")])
                .await?;
            page.results.into_iter().filter_map(|r| r.into_summary(None)).collect()
        };
        Ok(SearchResults {
            query: query.to_owned(),
            items,
        })
    }

    async fn details(&self, kind: MediaKind, tmdb_id: i64) -> Result<TitleDetails, CatalogError> {
        let path = format!("/{}/{tmdb_id}", kind.as_str());
        let raw: serde_json::Value = self.get(&path, &[("append_to_response", "external_ids")]).await?;
        details_from(raw, kind)
    }

    async fn season(&self, tmdb_id: i64, number: i32) -> Result<Season, CatalogError> {
        let raw: RawSeasonDetails = self.get(&format!("/tv/{tmdb_id}/season/{number}"), &[]).await?;
        Ok(Season {
            number: raw.season_number,
            name: raw.name,
            episodes: raw
                .episodes
                .into_iter()
                .map(|e| Episode {
                    number: e.episode_number,
                    name: e
                        .name
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| format!("Серия {}", e.episode_number)),
                    overview: e.overview.filter(|o| !o.is_empty()),
                    still_path: e.still_path,
                    runtime_min: e.runtime,
                    air_date: e.air_date,
                })
                .collect(),
        })
    }

    async fn image(&self, size: &str, file: &str) -> Result<(Vec<u8>, String), CatalogError> {
        if !ALLOWED_SIZES.contains(&size) || !is_image_file(file) {
            return Err(CatalogError::NotFound);
        }
        let resp = self
            .http
            .get(format!("{IMAGES}/{size}/{file}"))
            .send()
            .await
            .map_err(|e| CatalogError::Upstream(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(CatalogError::NotFound);
        }
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/jpeg")
            .to_owned();
        let bytes = resp.bytes().await.map_err(|e| CatalogError::Upstream(e.to_string()))?;
        Ok((bytes.to_vec(), ctype))
    }
}

/// `abc123.jpg` style names only, so the proxy can't be pointed elsewhere.
pub(crate) fn is_image_file(file: &str) -> bool {
    let (stem, ext) = match file.rsplit_once('.') {
        Some(p) => p,
        None => return false,
    };
    !stem.is_empty()
        && stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && matches!(ext, "jpg" | "jpeg" | "png" | "webp" | "svg")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_tv_details_like_tmdb_sends_them() {
        let raw = json!({
            "id": 1396, "name": "Во все тяжкие", "original_name": "Breaking Bad",
            "first_air_date": "2008-01-20", "poster_path": "/p.jpg", "backdrop_path": null,
            "vote_average": 8.9, "overview": "",
            "genres": [{"id": 18, "name": "драма"}],
            "episode_run_time": [47], "tagline": "",
            "seasons": [
                {"season_number": 0, "name": "Спецматериалы", "episode_count": 9, "air_date": "2009-02-17"},
                {"season_number": 1, "name": "Сезон 1", "episode_count": 7, "air_date": "2008-01-20"}
            ],
            "external_ids": {"imdb_id": "tt0903747"}
        });
        let d = details_from(raw, MediaKind::Tv).unwrap();
        assert_eq!(d.summary.title, "Во все тяжкие");
        assert_eq!(d.summary.year, Some(2008));
        assert_eq!(d.summary.overview, None, "empty overview becomes None");
        assert_eq!(d.runtime_min, Some(47));
        assert_eq!(d.tagline, None);
        assert_eq!(
            d.seasons.iter().map(|s| s.number).collect::<Vec<_>>(),
            vec![1, 0],
            "specials last"
        );
        assert_eq!(d.imdb_id.as_deref(), Some("tt0903747"));
    }

    #[test]
    fn mixed_search_results_skip_people() {
        let page: Page = serde_json::from_value(json!({"results": [
            {"id": 1, "media_type": "person", "name": "Someone"},
            {"id": 603, "media_type": "movie", "title": "Матрица", "release_date": "1999-03-30"},
            {"id": 1399, "media_type": "tv", "name": "Игра престолов", "first_air_date": "2011-04-17"}
        ]}))
        .unwrap();
        let items: Vec<_> = page.results.into_iter().filter_map(|r| r.into_summary(None)).collect();
        assert_eq!(
            items.iter().map(|i| (i.kind, i.tmdb_id)).collect::<Vec<_>>(),
            vec![(MediaKind::Movie, 603), (MediaKind::Tv, 1399)]
        );
        assert_eq!(items[0].year, Some(1999));
    }

    #[test]
    fn image_names_are_restricted() {
        assert!(is_image_file("kqjL17yufvn9OVLyXYpvtyrFfak.jpg"));
        assert!(!is_image_file("../etc/passwd"));
        assert!(!is_image_file("a.jpg?x=1"));
        assert!(!is_image_file(".jpg"));
        assert!(!is_image_file("evil.com/x.jpg"));
    }
}
