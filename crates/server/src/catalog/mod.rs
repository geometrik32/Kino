//! Film and series metadata. Real data comes from TMDB; without an API key the
//! server falls back to a small built-in demo catalog so the UI can be tried.

mod demo;
pub mod personalize;
mod tmdb;

use async_trait::async_trait;
use kino_core::{
    CatalogRow, HomeFeed, MediaKind, PersonDetails, SearchResults, Season, ShelfPage, TitleDetails, TitleExtras,
};

pub use demo::DemoCatalog;
pub use personalize::UserTaste;
pub use tmdb::TmdbCatalog;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("not found")]
    NotFound,
    #[error("metadata service error: {0}")]
    Upstream(String),
}

/// The catalog the settings ask for: TMDB with a key, the demo one without.
/// Shared by the server and the bot, so both see the same titles.
pub fn from_settings(
    api_key: Option<&str>,
    language: &str,
    proxy: Option<&str>,
) -> anyhow::Result<std::sync::Arc<dyn Catalog>> {
    from_settings_with_image_cache(api_key, language, proxy, None)
}

/// Same as [`from_settings`], optionally attaching a bounded disk cache for TMDB images.
pub fn from_settings_with_image_cache(
    api_key: Option<&str>,
    language: &str,
    proxy: Option<&str>,
    image_cache: Option<(std::path::PathBuf, u64)>,
) -> anyhow::Result<std::sync::Arc<dyn Catalog>> {
    Ok(match api_key {
        Some(key) => {
            let mut cat = TmdbCatalog::new(key.to_owned(), language.to_owned(), proxy)?;
            if let Some((dir, max_mb)) = image_cache {
                cat = cat.with_image_cache(dir, max_mb * 1024 * 1024);
            }
            std::sync::Arc::new(cat)
        }
        None => {
            tracing::warn!("TMDB_API_KEY is not set: using the built-in demo catalog");
            std::sync::Arc::new(DemoCatalog)
        }
    })
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RandomMovieFilter {
    /// TMDB genre ID, if selected.
    pub genre_id: Option<i32>,
    /// Genre name in Russian lowercase (for `DemoCatalog`).
    pub genre_name: Option<String>,
    /// Approximate target runtime in minutes.
    pub target_runtime_min: Option<i32>,
    /// Minimum TMDB rating (`0.0..=10.0`).
    pub min_rating: Option<f32>,
    /// Allowed deviation in percent (`0..=100`) applied to runtime and rating bounds.
    pub deviation_pct: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveRandomBounds {
    pub genre_id: Option<i32>,
    pub runtime_range: Option<(i32, i32)>,
    pub min_rating: Option<f32>,
}

impl RandomMovieFilter {
    pub fn effective_bounds(&self) -> EffectiveRandomBounds {
        let pct = self.deviation_pct.min(100) as f32 / 100.0;
        let runtime_range = self.target_runtime_min.filter(|&m| m > 0).map(|target| {
            let delta = if self.deviation_pct == 0 {
                5
            } else {
                ((target as f32 * pct).round() as i32).max(5)
            };
            ((target - delta).max(15), target + delta)
        });
        let min_rating = self.min_rating.filter(|&r| r > 0.0).map(|min_r| {
            let adjusted = (min_r * (1.0 - pct)).clamp(0.0, 10.0);
            (adjusted * 10.0).round() / 10.0
        });
        EffectiveRandomBounds {
            genre_id: self.genre_id,
            runtime_range,
            min_rating,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RandomMoviePick {
    pub details: TitleDetails,
    /// True when no movie matched the exact/deviated filters and bounds had to be relaxed.
    pub relaxed: bool,
}

#[async_trait]
pub trait Catalog: Send + Sync {
    async fn home(&self) -> Result<HomeFeed, CatalogError> {
        self.home_for(&UserTaste::default()).await
    }
    async fn home_for(&self, taste: &UserTaste) -> Result<HomeFeed, CatalogError>;
    async fn home_shelves(&self, ids: &[String], taste: &UserTaste) -> Result<Vec<CatalogRow>, CatalogError>;
    async fn shelf_page(&self, id: &str, page: u32, taste: &UserTaste) -> Result<ShelfPage, CatalogError>;
    async fn search(&self, query: &str) -> Result<SearchResults, CatalogError>;
    async fn random_movie(&self, filter: &RandomMovieFilter) -> Result<RandomMoviePick, CatalogError>;
    async fn details(&self, kind: MediaKind, tmdb_id: i64) -> Result<TitleDetails, CatalogError>;
    /// Cast, recommendations and similar titles.
    async fn extras(&self, kind: MediaKind, tmdb_id: i64) -> Result<TitleExtras, CatalogError>;
    /// An actor with their filmography.
    async fn person(&self, id: i64) -> Result<PersonDetails, CatalogError>;
    async fn season(&self, tmdb_id: i64, number: i32) -> Result<Season, CatalogError>;
    /// Raw image bytes and content type for `/api/img/{size}/{file}`.
    async fn image(&self, size: &str, file: &str) -> Result<(Vec<u8>, String), CatalogError>;
}

/// Year from a TMDB date string (`2010-07-15`).
pub(crate) fn year_of(date: Option<&str>) -> Option<i32> {
    date.and_then(|d| d.get(..4)).and_then(|y| y.parse().ok())
}

const MAX_YO_POSITIONS: usize = 8;

/// Normalizes a title or query for comparison: lowercase, `ё` folded to `е`,
/// and punctuation collapsed to single spaces.
pub(crate) fn normalize_search_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(if lower == 'ё' { 'е' } else { lower });
            }
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Whether `title` or `original` matches a normalized query (as a substring or all words).
pub(crate) fn matches_normalized_query(title: &str, original: Option<&str>, norm_query: &str) -> bool {
    if norm_query.is_empty() {
        return false;
    }
    let norm_title = normalize_search_text(title);
    let norm_orig = original.map(normalize_search_text);
    let contains_all = |haystack: &str| {
        haystack.contains(norm_query) || norm_query.split_whitespace().all(|word| haystack.contains(word))
    };
    contains_all(&norm_title) || norm_orig.as_deref().is_some_and(contains_all)
}

fn push_unique(variants: &mut Vec<String>, trimmed: &str, candidate: String) {
    if candidate != trimmed && !variants.contains(&candidate) {
        variants.push(candidate);
    }
}

/// Candidate queries to work around strict `е` / `ё` matching in TMDB search.
pub(crate) fn yo_query_variants(query: &str) -> Vec<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut variants = Vec::new();

    if trimmed.contains(['ё', 'Ё']) {
        let folded: String = trimmed
            .chars()
            .map(|c| match c {
                'ё' => 'е',
                'Ё' => 'Е',
                other => other,
            })
            .collect();
        push_unique(&mut variants, trimmed, folded);
    }

    for (byte_idx, ch) in trimmed.char_indices() {
        let replacement = match ch {
            'е' => 'ё',
            'Е' => 'Ё',
            _ => continue,
        };
        let mut candidate = String::with_capacity(trimmed.len() + 2);
        candidate.push_str(&trimmed[..byte_idx]);
        candidate.push(replacement);
        candidate.push_str(&trimmed[byte_idx + ch.len_utf8()..]);
        push_unique(&mut variants, trimmed, candidate);
        if variants.len() >= MAX_YO_POSITIONS {
            break;
        }
    }

    variants
}

/// Longest word without `е`/`ё` in a multi-word query, used as a fallback anchor.
pub(crate) fn anchor_word_query(query: &str) -> Option<String> {
    let words: Vec<&str> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words.len() < 2 {
        return None;
    }
    words
        .into_iter()
        .filter(|w| {
            w.chars().count() >= 3
                && !w.contains(['е', 'Е', 'ё', 'Ё'])
                && !(w.len() == 4 && w.bytes().all(|b| b.is_ascii_digit()))
        })
        .max_by_key(|w| w.chars().count())
        .map(str::to_owned)
}

/// Splits a trailing 4-digit year (`"Одиссей 1997"` or `"Одиссей (1997)"`) from a search query
/// when there is a non-empty title before it.
pub(crate) fn split_query_year(query: &str) -> Option<(&str, i32)> {
    let trimmed = query.trim();
    let (prefix, year_str) = if let Some(inner) = trimmed.strip_suffix(')') {
        let (head, y) = inner.rsplit_once('(')?;
        (head, y.trim())
    } else if let Some(inner) = trimmed.strip_suffix(']') {
        let (head, y) = inner.rsplit_once('[')?;
        (head, y.trim())
    } else {
        let idx = trimmed.rfind(char::is_whitespace)?;
        (&trimmed[..idx], trimmed[idx..].trim())
    };
    if year_str.len() != 4 || !year_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i32 = year_str.parse().ok()?;
    if !(1930..=2049).contains(&year) {
        return None;
    }
    let title = prefix.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, ',' | '/' | '-' | '–'));
    title.chars().any(char::is_alphanumeric).then_some((title, year))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_yo_case_and_punctuation() {
        assert_eq!(normalize_search_text("Зелёная линия"), "зеленая линия");
        assert_eq!(
            normalize_search_text("  Тёмный-рыцарь: Начало! "),
            "темный рыцарь начало"
        );
        assert!(matches_normalized_query(
            "Зелёная линия",
            None,
            &normalize_search_text("зеленая линия")
        ));
        assert!(matches_normalized_query(
            "Тёмный рыцарь",
            Some("The Dark Knight"),
            &normalize_search_text("темный")
        ));
    }

    #[test]
    fn generates_yo_variants_for_e_and_yo() {
        assert_eq!(
            yo_query_variants("Зеленая линия"),
            vec!["Зёленая линия", "Зелёная линия"]
        );
        assert_eq!(yo_query_variants("Ёлки"), vec!["Елки"]);
        assert!(yo_query_variants("Матрица").is_empty());
    }

    #[test]
    fn picks_anchor_word_without_e_or_yo() {
        assert_eq!(anchor_word_query("Зеленая линия").as_deref(), Some("линия"));
        assert_eq!(anchor_word_query("линия"), None);
        assert_eq!(anchor_word_query("Все везде"), None);
    }

    #[test]
    fn splits_trailing_year_from_query() {
        assert_eq!(split_query_year("Одиссей 1997"), Some(("Одиссей", 1997)));
        assert_eq!(split_query_year("Одиссея (1997)"), Some(("Одиссея", 1997)));
        assert_eq!(split_query_year("Дюна, 1984"), Some(("Дюна", 1984)));
        assert_eq!(split_query_year("1917"), None);
        assert_eq!(split_query_year("1984"), None);
        assert_eq!(split_query_year("Фильм 1080"), None);
    }

    #[test]
    fn random_filter_applies_deviation_to_runtime_and_rating() {
        let f = RandomMovieFilter {
            genre_id: Some(878),
            genre_name: Some("фантастика".into()),
            target_runtime_min: Some(120),
            min_rating: Some(7.5),
            deviation_pct: 20,
        };
        let b = f.effective_bounds();
        assert_eq!(b.genre_id, Some(878));
        assert_eq!(b.runtime_range, Some((96, 144)));
        assert_eq!(b.min_rating, Some(6.0));

        let strict = RandomMovieFilter { deviation_pct: 0, ..f };
        let sb = strict.effective_bounds();
        assert_eq!(sb.runtime_range, Some((115, 125)));
        assert_eq!(sb.min_rating, Some(7.5));
    }
}
