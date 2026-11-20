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
    CastMember, CatalogRow, Episode, HomeFeed, MediaKind, PersonDetails, SearchResults, Season, SeasonSummary,
    TitleDetails, TitleExtras, TitleSummary,
};
use serde::{de::DeserializeOwned, Deserialize};

use super::{year_of, Catalog, CatalogError};

const API: &str = "https://api.themoviedb.org/3";
const IMAGES: &str = "https://image.tmdb.org/t/p";
const CACHE_TTL: Duration = Duration::from_secs(6 * 3600);
/// Answers kept at most; the oldest are dropped first.
const CACHE_MAX: usize = 400;
const ALLOWED_SIZES: [&str; 5] = ["w185", "w300", "w342", "w780", "w1280"];

/// A network error without the request URL, which carries the API key.
fn net_error(e: reqwest::Error) -> CatalogError {
    let mut causes = String::new();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        causes.push_str(": ");
        causes.push_str(&s.to_string());
        source = s.source();
    }
    CatalogError::Upstream(format!("{}{causes}", e.without_url()))
}

pub struct TmdbCatalog {
    http: reqwest::Client,
    /// v3 API key or v4 read access token (starts with `eyJ`).
    key: String,
    language: String,
    cache: Mutex<HashMap<String, (Instant, serde_json::Value)>>,
}

impl TmdbCatalog {
    /// `proxy`, when given, carries all TMDB traffic (API and images).
    pub fn new(key: String, language: String, proxy: Option<&str>) -> anyhow::Result<Self> {
        let http = crate::config::http_client(proxy, Duration::from_secs(15))?;
        Ok(Self {
            http,
            key,
            language,
            cache: Mutex::default(),
        })
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
        let resp = req.send().await.map_err(net_error)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CatalogError::NotFound);
        }
        if !resp.status().is_success() {
            return Err(CatalogError::Upstream(format!("TMDB answered {}", resp.status())));
        }
        let value: serde_json::Value = resp.json().await.map_err(net_error)?;
        let parsed = serde_json::from_value(value.clone()).map_err(|e| CatalogError::Upstream(e.to_string()))?;
        trim_insert(
            &mut self.cache.lock().expect("cache lock"),
            cache_key,
            value,
            CACHE_TTL,
            CACHE_MAX,
        );
        Ok(parsed)
    }

    async fn list(&self, path: &str, kind: Option<MediaKind>) -> Result<Vec<TitleSummary>, CatalogError> {
        let page: Page = self.get(path, &[]).await?;
        Ok(page.results.into_iter().filter_map(|r| r.into_summary(kind)).collect())
    }
}

/// Stores an answer, first dropping what has expired and, when the cache is
/// still full, the oldest answers, so it never grows past `max`.
fn trim_insert(
    cache: &mut HashMap<String, (Instant, serde_json::Value)>,
    key: String,
    value: serde_json::Value,
    ttl: Duration,
    max: usize,
) {
    if cache.len() >= max {
        cache.retain(|_, (at, _)| at.elapsed() < ttl);
    }
    while cache.len() >= max {
        let Some(oldest) = cache.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone()) else {
            break;
        };
        cache.remove(&oldest);
    }
    cache.insert(key, (Instant::now(), value));
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

/// `credits`, `recommendations` and `similar` of a title, as `append_to_response` returns them.
#[derive(Deserialize)]
struct RawExtras {
    credits: Option<RawCredits>,
    recommendations: Option<Page>,
    similar: Option<Page>,
}

#[derive(Deserialize)]
struct RawCredits {
    #[serde(default)]
    cast: Vec<RawCast>,
}

#[derive(Deserialize)]
struct RawCast {
    id: i64,
    name: String,
    character: Option<String>,
    profile_path: Option<String>,
    #[serde(default)]
    order: i32,
}

/// How many actors and titles per row the page shows.
const MAX_CAST: usize = 20;
const MAX_TITLES: usize = 20;
const MAX_CREDITS: usize = 60;

pub(crate) fn extras_from(raw: serde_json::Value, kind: MediaKind, own_id: i64) -> Result<TitleExtras, CatalogError> {
    let r: RawExtras = serde_json::from_value(raw).map_err(|e| CatalogError::Upstream(e.to_string()))?;
    let mut cast = r.credits.map(|c| c.cast).unwrap_or_default();
    cast.sort_by_key(|c| c.order);
    let cast = cast
        .into_iter()
        .take(MAX_CAST)
        .map(|c| CastMember {
            id: c.id,
            name: c.name,
            character: c.character.filter(|c| !c.is_empty()),
            profile_path: c.profile_path,
        })
        .collect();
    // Only titles with a poster: a row of grey tiles is no use.
    let titles = |p: Option<Page>| -> Vec<TitleSummary> {
        p.map(|p| p.results)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|i| i.into_summary(Some(kind)))
            .filter(|t| t.poster_path.is_some() && t.tmdb_id != own_id)
            .collect()
    };
    let recommendations: Vec<_> = titles(r.recommendations).into_iter().take(MAX_TITLES).collect();
    let similar = titles(r.similar)
        .into_iter()
        .filter(|t| !recommendations.iter().any(|r| r.tmdb_id == t.tmdb_id))
        .take(MAX_TITLES)
        .collect();
    Ok(TitleExtras {
        cast,
        recommendations,
        similar,
    })
}

#[derive(Deserialize)]
struct RawPerson {
    id: i64,
    name: String,
    biography: Option<String>,
    birthday: Option<String>,
    deathday: Option<String>,
    place_of_birth: Option<String>,
    profile_path: Option<String>,
    combined_credits: Option<RawCombined>,
}

#[derive(Deserialize)]
struct RawCombined {
    #[serde(default)]
    cast: Vec<RawCredit>,
}

#[derive(Deserialize)]
struct RawCredit {
    #[serde(flatten)]
    item: ListItem,
    #[serde(default)]
    popularity: f32,
    #[serde(default)]
    genre_ids: Vec<i32>,
}

/// TMDB genres of appearances as oneself (talk shows, news, award shows):
/// not what anyone looks for in a filmography.
const SELF_GENRES: [i32; 3] = [10767, 10763, 10764];

pub(crate) fn person_from(raw: serde_json::Value) -> Result<PersonDetails, CatalogError> {
    let r: RawPerson = serde_json::from_value(raw).map_err(|e| CatalogError::Upstream(e.to_string()))?;
    let mut cast = r.combined_credits.map(|c| c.cast).unwrap_or_default();
    cast.retain(|c| !c.genre_ids.iter().any(|g| SELF_GENRES.contains(g)));
    cast.sort_by(|a, b| b.popularity.total_cmp(&a.popularity));
    let mut credits: Vec<TitleSummary> = Vec::new();
    for c in cast {
        let Some(t) = c.item.into_summary(None) else { continue };
        // A role in several episodes or a re-listed film comes more than once.
        if t.poster_path.is_some() && !credits.iter().any(|o| o.kind == t.kind && o.tmdb_id == t.tmdb_id) {
            credits.push(t);
        }
    }
    credits.truncate(MAX_CREDITS);
    let text = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
    Ok(PersonDetails {
        id: r.id,
        name: r.name,
        biography: text(r.biography),
        birthday: text(r.birthday),
        deathday: text(r.deathday),
        place_of_birth: text(r.place_of_birth),
        profile_path: r.profile_path,
        credits,
    })
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

    async fn extras(&self, kind: MediaKind, tmdb_id: i64) -> Result<TitleExtras, CatalogError> {
        let path = format!("/{}/{tmdb_id}", kind.as_str());
        let raw: serde_json::Value = self
            .get(&path, &[("append_to_response", "credits,recommendations,similar")])
            .await?;
        extras_from(raw, kind, tmdb_id)
    }

    async fn person(&self, id: i64) -> Result<PersonDetails, CatalogError> {
        let raw: serde_json::Value = self
            .get(&format!("/person/{id}"), &[("append_to_response", "combined_credits")])
            .await?;
        person_from(raw)
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
            .map_err(net_error)?;
        if !resp.status().is_success() {
            return Err(CatalogError::NotFound);
        }
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/jpeg")
            .to_owned();
        let bytes = resp.bytes().await.map_err(net_error)?;
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
    fn parses_cast_and_suggestions() {
        let raw = json!({
            "id": 1396,
            "credits": { "cast": [
                {"id": 3, "name": "Аарон Пол", "character": "Джесси Пинкман", "profile_path": "/b.jpg", "order": 1},
                {"id": 2, "name": "Брайан Крэнстон", "character": "Уолтер Уайт", "profile_path": "/a.jpg", "order": 0},
                {"id": 4, "name": "Без фото", "character": "", "profile_path": null, "order": 2}
            ]},
            "recommendations": { "results": [
                {"id": 60059, "name": "Лучше звоните Солу", "first_air_date": "2015-02-08", "poster_path": "/s.jpg"},
                {"id": 1, "name": "Без постера", "poster_path": null},
                {"id": 1396, "name": "Он сам", "poster_path": "/x.jpg"}
            ]},
            "similar": { "results": [
                {"id": 60059, "name": "Лучше звоните Солу", "first_air_date": "2015-02-08", "poster_path": "/s.jpg"},
                {"id": 46648, "name": "Настоящий детектив", "first_air_date": "2014-01-12", "poster_path": "/t.jpg"}
            ]}
        });
        let e = extras_from(raw, MediaKind::Tv, 1396).unwrap();
        assert_eq!(
            e.cast.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["Брайан Крэнстон", "Аарон Пол", "Без фото"],
            "billing order"
        );
        assert_eq!(e.cast[2].character, None, "empty role is dropped");
        assert_eq!(e.cast[2].profile_path, None);
        assert_eq!(e.recommendations.len(), 1, "no poster and the title itself are dropped");
        assert_eq!(e.recommendations[0].kind, MediaKind::Tv);
        assert_eq!(e.recommendations[0].year, Some(2015));
        assert_eq!(
            e.similar.iter().map(|t| t.tmdb_id).collect::<Vec<_>>(),
            [46648],
            "what is already recommended is not repeated"
        );
        // A title TMDB knows nothing more about still works.
        let empty = extras_from(json!({ "id": 5 }), MediaKind::Movie, 5).unwrap();
        assert_eq!(empty, TitleExtras::default());
    }

    #[test]
    fn parses_a_person_with_filmography() {
        let raw = json!({
            "id": 17419, "name": "Брайан Крэнстон", "biography": "  ", "birthday": "1956-03-07",
            "place_of_birth": "Голливуд, США", "profile_path": "/a.jpg",
            "combined_credits": { "cast": [
                {"id": 1, "media_type": "tv", "name": "Малкольм в центре внимания", "first_air_date": "2000-01-09",
                 "poster_path": "/m.jpg", "popularity": 40.0, "genre_ids": [35]},
                {"id": 1396, "media_type": "tv", "name": "Во все тяжкие", "first_air_date": "2008-01-20",
                 "poster_path": "/b.jpg", "popularity": 90.5, "genre_ids": [18]},
                {"id": 1396, "media_type": "tv", "name": "Во все тяжкие", "first_air_date": "2008-01-20",
                 "poster_path": "/b.jpg", "popularity": 90.5, "genre_ids": [18]},
                {"id": 2, "media_type": "tv", "name": "Вечернее шоу", "first_air_date": "2010-01-01",
                 "poster_path": "/t.jpg", "popularity": 99.0, "genre_ids": [10767]},
                {"id": 3, "media_type": "movie", "title": "Без постера", "poster_path": null, "popularity": 50.0, "genre_ids": []},
                {"id": 4, "media_type": "movie", "title": "Драйв", "release_date": "2011-09-16",
                 "poster_path": "/d.jpg", "popularity": 60.0, "genre_ids": [80]}
            ]}
        });
        let p = person_from(raw).unwrap();
        assert_eq!(p.name, "Брайан Крэнстон");
        assert_eq!(p.biography, None, "blank biography is dropped");
        assert_eq!(p.birthday.as_deref(), Some("1956-03-07"));
        assert_eq!(
            p.credits.iter().map(|t| (t.kind, t.tmdb_id)).collect::<Vec<_>>(),
            [(MediaKind::Tv, 1396), (MediaKind::Movie, 4), (MediaKind::Tv, 1)],
            "most popular first; once each; no talk shows, no title without a poster"
        );
        assert_eq!(p.credits[1].year, Some(2011));
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
    fn the_cache_stays_small() {
        let mut cache = HashMap::new();
        for i in 0..50 {
            trim_insert(&mut cache, format!("k{i}"), json!(i), CACHE_TTL, 10);
            assert!(cache.len() <= 10);
        }
        // The newest are kept, the oldest are gone.
        assert!(cache.contains_key("k49") && !cache.contains_key("k0"));
        // Expired answers go before live ones.
        let mut cache = HashMap::new();
        for i in 0..4 {
            trim_insert(&mut cache, format!("k{i}"), json!(i), Duration::ZERO, 4);
        }
        trim_insert(&mut cache, "new".into(), json!(1), Duration::ZERO, 4);
        assert_eq!(cache.len(), 1, "all four had expired");
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
