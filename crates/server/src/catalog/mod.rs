//! Film and series metadata. Real data comes from TMDB; without an API key the
//! server falls back to a small built-in demo catalog so the UI can be tried.

mod demo;
mod tmdb;

use async_trait::async_trait;
use kino_core::{HomeFeed, MediaKind, SearchResults, Season, TitleDetails};

pub use demo::DemoCatalog;
pub use tmdb::TmdbCatalog;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("not found")]
    NotFound,
    #[error("metadata service error: {0}")]
    Upstream(String),
}

#[async_trait]
pub trait Catalog: Send + Sync {
    async fn home(&self) -> Result<HomeFeed, CatalogError>;
    async fn search(&self, query: &str) -> Result<SearchResults, CatalogError>;
    async fn details(&self, kind: MediaKind, tmdb_id: i64) -> Result<TitleDetails, CatalogError>;
    async fn season(&self, tmdb_id: i64, number: i32) -> Result<Season, CatalogError>;
    /// Raw image bytes and content type for `/api/img/{size}/{file}`.
    async fn image(&self, size: &str, file: &str) -> Result<(Vec<u8>, String), CatalogError>;
}

/// Year from a TMDB date string (`2010-07-15`).
pub(crate) fn year_of(date: Option<&str>) -> Option<i32> {
    date.and_then(|d| d.get(..4)).and_then(|y| y.parse().ok())
}
