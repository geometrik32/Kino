//! Film and series metadata. Real data comes from TMDB; without an API key the
//! server falls back to a small built-in demo catalog so the UI can be tried.

mod demo;
mod tmdb;

use async_trait::async_trait;
use kino_core::{HomeFeed, MediaKind, PersonDetails, SearchResults, Season, TitleDetails, TitleExtras};

pub use demo::DemoCatalog;
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
    Ok(match api_key {
        Some(key) => std::sync::Arc::new(TmdbCatalog::new(key.to_owned(), language.to_owned(), proxy)?),
        None => {
            tracing::warn!("TMDB_API_KEY is not set: using the built-in demo catalog");
            std::sync::Arc::new(DemoCatalog)
        }
    })
}

#[async_trait]
pub trait Catalog: Send + Sync {
    async fn home(&self) -> Result<HomeFeed, CatalogError>;
    async fn search(&self, query: &str) -> Result<SearchResults, CatalogError>;
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

/// Candidate queries to work around strict `е` / `ё` matching in TMDB search.
pub(crate) fn yo_query_variants(query: &str) -> Vec<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut variants = Vec::new();
    let mut push_unique = |candidate: String| {
        if candidate != trimmed && !variants.contains(&candidate) {
            variants.push(candidate);
        }
    };

    if trimmed.contains(['ё', 'Ё']) {
        let folded: String = trimmed
            .chars()
            .map(|c| match c {
                'ё' => 'е',
                'Ё' => 'Е',
                other => other,
            })
            .collect();
        push_unique(folded);
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
        push_unique(candidate);
        if variants.len() >= MAX_YO_POSITIONS + 1 {
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
        .filter(|w| w.chars().count() >= 3 && !w.contains(['е', 'Е', 'ё', 'Ё']))
        .max_by_key(|w| w.chars().count())
        .map(str::to_owned)
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
}
