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
use chrono::{Duration as ChronoDuration, Utc};
use futures_util::future::join_all;
use kino_core::{
    CastMember, CatalogRow, Episode, HomeFeed, MediaKind, PersonDetails, SearchResults, Season, SeasonSummary,
    ShelfPage, TitleDetails, TitleExtras, TitleSummary,
};
use rand::Rng;
use serde::{de::DeserializeOwned, Deserialize};

use super::{
    anchor_word_query, matches_normalized_query, normalize_search_text,
    personalize::{
        because_shelf_page, because_shelf_row, categories_for, deferred_stub, filter_excluded, parse_because_id,
        rank_shelves, ShelfMeta, UserTaste, INITIAL_HOME_ROWS,
    },
    split_query_year, year_of, yo_query_variants, Catalog, CatalogError, RandomMovieFilter, RandomMoviePick,
};

const API: &str = "https://api.themoviedb.org/3";
const IMAGES: &str = "https://image.tmdb.org/t/p";
const CACHE_TTL: Duration = Duration::from_secs(6 * 3600);
/// Answers kept at most; the oldest are dropped first.
const CACHE_MAX: usize = 2000;
const MAX_SHELF_PAGE: u32 = 20;
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

    async fn fetch_shelf_raw(&self, spec: &TmdbShelf, page: u32) -> Result<(Vec<TitleSummary>, bool), CatalogError> {
        let page_num = page.clamp(1, MAX_SHELF_PAGE);
        let page_str = page_num.to_string();
        let mut owned: Vec<(&str, String)> = Vec::new();
        owned.push(("page", page_str));
        if let Some(days) = spec.recent_days {
            let today = Utc::now().date_naive();
            let since = today - ChronoDuration::days(days);
            let date_field = match spec.meta.kind {
                Some(MediaKind::Tv) => ("first_air_date.gte", "first_air_date.lte"),
                _ => ("primary_release_date.gte", "primary_release_date.lte"),
            };
            owned.push((date_field.0, since.format("%Y-%m-%d").to_string()));
            owned.push((date_field.1, today.format("%Y-%m-%d").to_string()));
        }
        let mut query: Vec<(&str, &str)> = spec.query.to_vec();
        for (k, v) in &owned {
            query.push((k, v.as_str()));
        }
        let res: DiscoverPage = self.get(spec.path, &query).await?;
        let items = res
            .results
            .into_iter()
            .filter_map(|r| r.into_summary(spec.meta.kind))
            .filter(|t| t.poster_path.is_some())
            .collect();
        let total = res.total_pages.clamp(1, MAX_SHELF_PAGE);
        Ok((items, page_num < total))
    }

    async fn fetch_shelf_filtered(
        &self,
        spec: &TmdbShelf,
        page: u32,
        taste: &UserTaste,
    ) -> Result<(Vec<TitleSummary>, u32, bool), CatalogError> {
        let mut page_num = page.clamp(1, MAX_SHELF_PAGE);
        let (raw, mut has_more) = self.fetch_shelf_raw(spec, page_num).await?;
        let mut items = filter_excluded(raw, &taste.excluded);
        // If exclusions trimmed page 1 aggressively, top up once from page 2.
        if page == 1 && items.len() < 8 && has_more {
            if let Ok((more, next_has_more)) = self.fetch_shelf_raw(spec, 2).await {
                merge_unique(&mut items, filter_excluded(more, &taste.excluded));
                page_num = 2;
                has_more = next_has_more;
            }
        }
        Ok((items, page_num, has_more))
    }

    async fn search_page(&self, query: &str) -> Result<Vec<TitleSummary>, CatalogError> {
        let page: Page = self
            .get("/search/multi", &[("query", query), ("include_adult", "false")])
            .await?;
        Ok(page.results.into_iter().filter_map(|r| r.into_summary(None)).collect())
    }

    async fn search_typed_with_year(&self, query: &str, year: i32) -> Vec<TitleSummary> {
        let y = year.to_string();
        let (movies, tv) = tokio::join!(
            self.get::<Page>(
                "/search/movie",
                &[("query", query), ("year", y.as_str()), ("include_adult", "false")],
            ),
            self.get::<Page>(
                "/search/tv",
                &[
                    ("query", query),
                    ("first_air_date_year", y.as_str()),
                    ("include_adult", "false"),
                ],
            ),
        );
        let mut out = Vec::new();
        if let Ok(p) = movies {
            merge_unique(
                &mut out,
                p.results
                    .into_iter()
                    .filter_map(|r| r.into_summary(Some(MediaKind::Movie)))
                    .collect(),
            );
        }
        if let Ok(p) = tv {
            merge_unique(
                &mut out,
                p.results
                    .into_iter()
                    .filter_map(|r| r.into_summary(Some(MediaKind::Tv)))
                    .collect(),
            );
        }
        out
    }

    async fn search_with_yo_fallback(&self, trimmed: &str, norm_q: &str, items: &mut Vec<TitleSummary>) {
        let has_match = |list: &[TitleSummary]| {
            list.iter()
                .any(|i| matches_normalized_query(&i.title, i.original_title.as_deref(), norm_q))
        };
        let variants = yo_query_variants(trimmed);
        if !variants.is_empty() {
            let pages = join_all(variants.iter().map(|v| self.search_page(v))).await;
            for page in pages.into_iter().flatten() {
                merge_unique(items, page);
            }
        }
        if !has_match(items) {
            if let Some(anchor) = anchor_word_query(trimmed) {
                if let Ok(page) = self.search_page(&anchor).await {
                    let filtered: Vec<_> = page
                        .into_iter()
                        .filter(|i| matches_normalized_query(&i.title, i.original_title.as_deref(), norm_q))
                        .collect();
                    merge_unique(items, filtered);
                }
            }
        }
        prioritize_normalized_matches(items, norm_q);
    }

    async fn search_by_year(&self, title_q: &str, year: i32) -> Vec<TitleSummary> {
        let mut year_hits = self.search_typed_with_year(title_q, year).await;
        if year_hits.is_empty() {
            let variants = yo_query_variants(title_q);
            if !variants.is_empty() {
                let pages = join_all(variants.iter().map(|v| self.search_typed_with_year(v, year))).await;
                for page in pages {
                    merge_unique(&mut year_hits, page);
                }
            }
        }
        if year_hits.is_empty() {
            if let Ok(page) = self.search_page(title_q).await {
                let near: Vec<_> = page
                    .into_iter()
                    .filter(|i| i.year.is_some_and(|y| (y - year).abs() <= 1))
                    .collect();
                merge_unique(&mut year_hits, near);
            }
        }
        prioritize_normalized_matches(&mut year_hits, &normalize_search_text(title_q));
        year_hits
    }

    async fn discover_movie_page(
        &self,
        genre_id: Option<i32>,
        runtime: Option<(i32, i32)>,
        min_rating: Option<f32>,
        page: u32,
    ) -> Result<(Vec<TitleSummary>, u32), CatalogError> {
        let mut params = vec![
            ("include_adult", "false".to_owned()),
            ("include_video", "false".to_owned()),
            ("sort_by", "popularity.desc".to_owned()),
            ("vote_count.gte", "50".to_owned()),
            ("page", page.to_string()),
        ];
        if let Some(g) = genre_id {
            params.push(("with_genres", g.to_string()));
        }
        if let Some((lo, hi)) = runtime {
            params.push(("with_runtime.gte", lo.to_string()));
            params.push(("with_runtime.lte", hi.to_string()));
        }
        if let Some(r) = min_rating {
            params.push(("vote_average.gte", format!("{r:.1}")));
        }
        let refs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let page: DiscoverPage = self.get("/discover/movie", &refs).await?;
        let items = page
            .results
            .into_iter()
            .filter_map(|r| r.into_summary(Some(MediaKind::Movie)))
            .collect();
        Ok((items, page.total_pages))
    }
}

fn merge_unique(dst: &mut Vec<TitleSummary>, src: Vec<TitleSummary>) {
    for item in src {
        if !dst
            .iter()
            .any(|existing| existing.kind == item.kind && existing.tmdb_id == item.tmdb_id)
        {
            dst.push(item);
        }
    }
}

fn prioritize_normalized_matches(items: &mut [TitleSummary], norm_query: &str) {
    items.sort_by_key(|item| !matches_normalized_query(&item.title, item.original_title.as_deref(), norm_query));
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

#[derive(Deserialize)]
struct DiscoverPage {
    results: Vec<ListItem>,
    #[serde(default)]
    total_pages: u32,
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

struct TmdbShelf {
    meta: ShelfMeta,
    path: &'static str,
    query: &'static [(&'static str, &'static str)],
    recent_days: Option<i64>,
}

const TMDB_SHELVES: &[TmdbShelf] = &[
    TmdbShelf {
        meta: ShelfMeta {
            id: "trending_tv",
            title: "Популярные сериалы",
            subtitle: None,
            categories: &["for_you", "tv"],
            kind: Some(MediaKind::Tv),
            genre_tags: &[],
            base_priority: 95,
        },
        path: "/trending/tv/week",
        query: &[],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "trending_movie",
            title: "Популярные фильмы",
            subtitle: None,
            categories: &["for_you", "movies"],
            kind: Some(MediaKind::Movie),
            genre_tags: &[],
            base_priority: 95,
        },
        path: "/trending/movie/week",
        query: &[],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "fresh_movies",
            title: "Свежие новинки кино",
            subtitle: None,
            categories: &["for_you", "movies", "fresh"],
            kind: Some(MediaKind::Movie),
            genre_tags: &[],
            base_priority: 88,
        },
        path: "/discover/movie",
        query: &[
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "40"),
            ("include_adult", "false"),
        ],
        recent_days: Some(450),
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "fresh_tv",
            title: "Новые и актуальные сериалы",
            subtitle: None,
            categories: &["for_you", "tv", "fresh"],
            kind: Some(MediaKind::Tv),
            genre_tags: &[],
            base_priority: 88,
        },
        path: "/discover/tv",
        query: &[
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "25"),
            ("include_adult", "false"),
        ],
        recent_days: Some(450),
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "top_movies",
            title: "С высоким рейтингом",
            subtitle: None,
            categories: &["for_you", "movies"],
            kind: Some(MediaKind::Movie),
            genre_tags: &[],
            base_priority: 82,
        },
        path: "/movie/top_rated",
        query: &[],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "top_tv",
            title: "Культовые сериалы",
            subtitle: None,
            categories: &["for_you", "tv"],
            kind: Some(MediaKind::Tv),
            genre_tags: &[],
            base_priority: 82,
        },
        path: "/tv/top_rated",
        query: &[],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "scifi_movies",
            title: "Фантастика и киберпанк",
            subtitle: None,
            categories: &["for_you", "movies", "scifi"],
            kind: Some(MediaKind::Movie),
            genre_tags: &["фантастика"],
            base_priority: 72,
        },
        path: "/discover/movie",
        query: &[
            ("with_genres", "878"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "250"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "scifi_tv",
            title: "Фантастические и фэнтези-сериалы",
            subtitle: None,
            categories: &["for_you", "tv", "scifi"],
            kind: Some(MediaKind::Tv),
            genre_tags: &["фантастика", "фэнтези"],
            base_priority: 72,
        },
        path: "/discover/tv",
        query: &[
            ("with_genres", "10765"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "100"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "thriller_movies",
            title: "Напряжённые триллеры",
            subtitle: None,
            categories: &["for_you", "movies", "thriller"],
            kind: Some(MediaKind::Movie),
            genre_tags: &["триллер"],
            base_priority: 70,
        },
        path: "/discover/movie",
        query: &[
            ("with_genres", "53"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "250"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "crime_tv",
            title: "Детективы и криминальные сериалы",
            subtitle: None,
            categories: &["for_you", "tv", "thriller"],
            kind: Some(MediaKind::Tv),
            genre_tags: &["криминал", "детектив", "триллер"],
            base_priority: 70,
        },
        path: "/discover/tv",
        query: &[
            ("with_genres", "80,9648"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "80"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "comedy_movies",
            title: "Комедии для лёгкого вечера",
            subtitle: None,
            categories: &["for_you", "movies", "comedy"],
            kind: Some(MediaKind::Movie),
            genre_tags: &["комедия"],
            base_priority: 68,
        },
        path: "/discover/movie",
        query: &[
            ("with_genres", "35"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "250"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "comedy_tv",
            title: "Комедийные сериалы",
            subtitle: None,
            categories: &["for_you", "tv", "comedy"],
            kind: Some(MediaKind::Tv),
            genre_tags: &["комедия"],
            base_priority: 66,
        },
        path: "/discover/tv",
        query: &[
            ("with_genres", "35"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "100"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "action_movies",
            title: "Зрелищные боевики",
            subtitle: None,
            categories: &["for_you", "movies", "action"],
            kind: Some(MediaKind::Movie),
            genre_tags: &["боевик"],
            base_priority: 68,
        },
        path: "/discover/movie",
        query: &[
            ("with_genres", "28"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "250"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "drama_movies",
            title: "Сильные драмы",
            subtitle: None,
            categories: &["for_you", "movies", "drama"],
            kind: Some(MediaKind::Movie),
            genre_tags: &["драма"],
            base_priority: 64,
        },
        path: "/discover/movie",
        query: &[
            ("with_genres", "18"),
            ("sort_by", "vote_average.desc"),
            ("vote_count.gte", "1000"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "drama_tv",
            title: "Драматические сериалы",
            subtitle: None,
            categories: &["for_you", "tv", "drama"],
            kind: Some(MediaKind::Tv),
            genre_tags: &["драма"],
            base_priority: 64,
        },
        path: "/discover/tv",
        query: &[
            ("with_genres", "18"),
            ("sort_by", "vote_average.desc"),
            ("vote_count.gte", "500"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "anime_tv",
            title: "Аниме-сериалы",
            subtitle: None,
            categories: &["for_you", "tv", "animation"],
            kind: Some(MediaKind::Tv),
            genre_tags: &["мультфильм", "аниме"],
            base_priority: 65,
        },
        path: "/discover/tv",
        query: &[
            ("with_genres", "16"),
            ("with_original_language", "ja"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "40"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "animation_movies",
            title: "Мультфильмы для всей семьи",
            subtitle: None,
            categories: &["for_you", "movies", "animation"],
            kind: Some(MediaKind::Movie),
            genre_tags: &["мультфильм", "семейный"],
            base_priority: 63,
        },
        path: "/discover/movie",
        query: &[
            ("with_genres", "16"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "250"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "ru_movies",
            title: "Российское кино",
            subtitle: None,
            categories: &["for_you", "movies", "ru"],
            kind: Some(MediaKind::Movie),
            genre_tags: &[],
            base_priority: 62,
        },
        path: "/discover/movie",
        query: &[
            ("with_original_language", "ru"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "25"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "ru_tv",
            title: "Российские сериалы",
            subtitle: None,
            categories: &["for_you", "tv", "ru"],
            kind: Some(MediaKind::Tv),
            genre_tags: &[],
            base_priority: 62,
        },
        path: "/discover/tv",
        query: &[
            ("with_original_language", "ru"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "15"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "fantasy_movies",
            title: "Фэнтези и приключения",
            subtitle: None,
            categories: &["for_you", "movies", "scifi"],
            kind: Some(MediaKind::Movie),
            genre_tags: &["фэнтези", "приключения"],
            base_priority: 60,
        },
        path: "/discover/movie",
        query: &[
            ("with_genres", "14,12"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "300"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "horror_movies",
            title: "Ужасы и мистика",
            subtitle: None,
            categories: &["for_you", "movies", "thriller"],
            kind: Some(MediaKind::Movie),
            genre_tags: &["ужасы"],
            base_priority: 58,
        },
        path: "/discover/movie",
        query: &[
            ("with_genres", "27"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "200"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "hidden_gems",
            title: "Неочевидные шедевры",
            subtitle: Some("Высокая оценка зрителей"),
            categories: &["for_you", "movies"],
            kind: Some(MediaKind::Movie),
            genre_tags: &[],
            base_priority: 59,
        },
        path: "/discover/movie",
        query: &[
            ("sort_by", "vote_average.desc"),
            ("vote_average.gte", "7.7"),
            ("vote_count.gte", "400"),
            ("vote_count.lte", "4500"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "doc_movies",
            title: "Документальное кино",
            subtitle: None,
            categories: &["for_you", "movies"],
            kind: Some(MediaKind::Movie),
            genre_tags: &["документальный"],
            base_priority: 52,
        },
        path: "/discover/movie",
        query: &[
            ("with_genres", "99"),
            ("sort_by", "vote_average.desc"),
            ("vote_count.gte", "150"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
    TmdbShelf {
        meta: ShelfMeta {
            id: "kdrama_tv",
            title: "Корейские дорамы и хиты",
            subtitle: None,
            categories: &["for_you", "tv"],
            kind: Some(MediaKind::Tv),
            genre_tags: &[],
            base_priority: 56,
        },
        path: "/discover/tv",
        query: &[
            ("with_original_language", "ko"),
            ("sort_by", "popularity.desc"),
            ("vote_count.gte", "40"),
            ("include_adult", "false"),
        ],
        recent_days: None,
    },
];

fn find_tmdb_shelf(id: &str) -> Option<&'static TmdbShelf> {
    TMDB_SHELVES.iter().find(|s| s.meta.id == id)
}

#[async_trait]
impl Catalog for TmdbCatalog {
    async fn home_for(&self, taste: &UserTaste) -> Result<HomeFeed, CatalogError> {
        let metas: Vec<ShelfMeta> = TMDB_SHELVES.iter().map(|s| s.meta).collect();
        let ranked = rank_shelves(&metas, taste);
        let initial_needed = INITIAL_HOME_ROWS.saturating_sub(taste.personal_rows.len());

        let (initial_metas, deferred_metas) = ranked.split_at(initial_needed.min(ranked.len()));
        let initial_specs: Vec<&'static TmdbShelf> =
            initial_metas.iter().filter_map(|m| find_tmdb_shelf(m.id)).collect();

        let fetched = join_all(
            initial_specs
                .iter()
                .map(|spec| self.fetch_shelf_filtered(spec, 1, taste)),
        )
        .await;

        let mut rows: Vec<CatalogRow> = taste.personal_rows.iter().take(INITIAL_HOME_ROWS).cloned().collect();
        let mut first_err: Option<CatalogError> = None;
        for (spec, res) in initial_specs.into_iter().zip(fetched) {
            match res {
                Ok((items, page, has_more)) if !items.is_empty() => rows.push(CatalogRow {
                    id: spec.meta.id.to_owned(),
                    title: spec.meta.title.to_owned(),
                    subtitle: spec.meta.subtitle.map(str::to_owned),
                    categories: spec.meta.categories.iter().map(|c| (*c).to_owned()).collect(),
                    loaded: true,
                    has_more,
                    page,
                    items,
                }),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(shelf = spec.meta.id, error = %e, "initial home shelf failed");
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        if rows.is_empty() {
            if let Some(err) = first_err {
                return Err(err);
            }
        }
        for meta in deferred_metas {
            rows.push(deferred_stub(meta));
        }

        let categories = categories_for(&rows);
        Ok(HomeFeed {
            rows,
            categories,
            demo: false,
        })
    }

    async fn home_shelves(&self, ids: &[String], taste: &UserTaste) -> Result<Vec<CatalogRow>, CatalogError> {
        let futs = ids.iter().map(|id| async move {
            if let Some((kind, tmdb_id)) = parse_because_id(id) {
                return because_shelf_row(self, kind, tmdb_id, taste).await.ok();
            }
            let spec = find_tmdb_shelf(id)?;
            match self.fetch_shelf_filtered(spec, 1, taste).await {
                Ok((items, page, has_more)) => Some(CatalogRow {
                    id: spec.meta.id.to_owned(),
                    title: spec.meta.title.to_owned(),
                    subtitle: spec.meta.subtitle.map(str::to_owned),
                    categories: spec.meta.categories.iter().map(|c| (*c).to_owned()).collect(),
                    loaded: true,
                    has_more,
                    page,
                    items,
                }),
                Err(e) => {
                    tracing::warn!(shelf = id, error = %e, "lazy home shelf failed");
                    None
                }
            }
        });
        Ok(join_all(futs).await.into_iter().flatten().collect())
    }

    async fn shelf_page(&self, id: &str, page: u32, taste: &UserTaste) -> Result<ShelfPage, CatalogError> {
        if let Some((kind, tmdb_id)) = parse_because_id(id) {
            return because_shelf_page(self, kind, tmdb_id, taste).await;
        }
        let spec = find_tmdb_shelf(id).ok_or(CatalogError::NotFound)?;
        let (items, page_num, has_more) = self.fetch_shelf_filtered(spec, page, taste).await?;
        Ok(ShelfPage {
            id: spec.meta.id.to_owned(),
            title: spec.meta.title.to_owned(),
            subtitle: spec.meta.subtitle.map(str::to_owned),
            page: page_num,
            has_more,
            items,
        })
    }

    async fn search(&self, query: &str) -> Result<SearchResults, CatalogError> {
        let trimmed = query.trim();
        let items = if trimmed.is_empty() {
            Vec::new()
        } else {
            let mut items = self.search_page(trimmed).await?;
            let norm_q = normalize_search_text(trimmed);
            let has_match = |list: &[TitleSummary]| {
                list.iter()
                    .any(|i| matches_normalized_query(&i.title, i.original_title.as_deref(), &norm_q))
            };
            if items.is_empty() {
                if let Some((title_q, year)) = split_query_year(trimmed) {
                    items = self.search_by_year(title_q, year).await;
                } else if !norm_q.is_empty() {
                    self.search_with_yo_fallback(trimmed, &norm_q, &mut items).await;
                }
            } else if !norm_q.is_empty() && !has_match(&items) {
                self.search_with_yo_fallback(trimmed, &norm_q, &mut items).await;
            }
            items
        };
        Ok(SearchResults {
            query: query.to_owned(),
            items,
        })
    }

    async fn random_movie(&self, filter: &RandomMovieFilter) -> Result<RandomMoviePick, CatalogError> {
        let bounds = filter.effective_bounds();
        let (mut active_genre, mut active_runtime, mut active_rating) =
            (bounds.genre_id, bounds.runtime_range, bounds.min_rating);
        let (mut items, mut total_pages) = self
            .discover_movie_page(active_genre, active_runtime, active_rating, 1)
            .await?;
        let mut relaxed = false;

        if items.is_empty() && (active_runtime.is_some() || active_rating.is_some()) {
            active_runtime = None;
            active_rating = None;
            (items, total_pages) = self.discover_movie_page(active_genre, None, None, 1).await?;
            relaxed = true;
        }
        if items.is_empty() && active_genre.is_some() {
            active_genre = None;
            (items, total_pages) = self.discover_movie_page(None, None, None, 1).await?;
            relaxed = true;
        }
        if items.is_empty() {
            return Err(CatalogError::NotFound);
        }

        let max_page = total_pages.clamp(1, 20);
        if max_page > 1 {
            let page_num = rand::rng().random_range(1..=max_page);
            if page_num != 1 {
                if let Ok((page_items, _)) = self
                    .discover_movie_page(active_genre, active_runtime, active_rating, page_num)
                    .await
                {
                    if !page_items.is_empty() {
                        items = page_items;
                    }
                }
            }
        }

        let with_poster: Vec<&TitleSummary> = items.iter().filter(|i| i.poster_path.is_some()).collect();
        let pool: Vec<&TitleSummary> = if with_poster.is_empty() {
            items.iter().collect()
        } else {
            with_poster
        };
        let chosen = pool[rand::rng().random_range(0..pool.len())].clone();
        let details = match self.details(MediaKind::Movie, chosen.tmdb_id).await {
            Ok(d) => d,
            Err(_) => TitleDetails {
                summary: chosen,
                genres: Vec::new(),
                runtime_min: None,
                tagline: None,
                seasons: Vec::new(),
                imdb_id: None,
                last_episode: None,
            },
        };
        Ok(RandomMoviePick { details, relaxed })
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

    #[test]
    fn merges_unique_and_prioritizes_normalized_matches() {
        let item = |kind, id, title: &str| TitleSummary {
            kind,
            tmdb_id: id,
            title: title.into(),
            original_title: None,
            year: None,
            poster_path: None,
            backdrop_path: None,
            rating: None,
            overview: None,
        };
        let mut list = vec![item(MediaKind::Movie, 1, "Другой фильм")];
        merge_unique(
            &mut list,
            vec![
                item(MediaKind::Movie, 1, "Дубликат"),
                item(MediaKind::Movie, 2, "Зелёная линия"),
            ],
        );
        prioritize_normalized_matches(&mut list, &normalize_search_text("зеленая линия"));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].tmdb_id, 2);
        assert_eq!(list[0].title, "Зелёная линия");
    }
}
