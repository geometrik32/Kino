//! Types shared between the server, the bot and the Dioxus app.
//!
//! Everything here is plain data with serde, so it compiles both natively and
//! for `wasm32-unknown-unknown`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A film or a series, as TMDB distinguishes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Movie,
    Tv,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Movie => "movie",
            MediaKind::Tv => "tv",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "movie" => Some(MediaKind::Movie),
            "tv" => Some(MediaKind::Tv),
            _ => None,
        }
    }
}

/// Share of the duration after which an item counts as watched.
pub const FINISHED_RATIO: f64 = 0.9;

/// Whether a position counts as "watched to the end".
pub fn is_finished(position_sec: f64, duration_sec: f64) -> bool {
    duration_sec > 0.0 && position_sec >= duration_sec * FINISHED_RATIO
}

/// The torrent release a user picked, so playback can resume from the same one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseRef {
    /// BitTorrent info-hash (hex, lowercase).
    pub infohash: String,
    /// Index of the file inside the torrent, as TorrServer numbers them.
    pub file_index: i32,
    /// Human-readable release name shown in the UI.
    pub title: Option<String>,
}

/// Body of `PUT /api/progress`, sent by the player every few seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressUpdate {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    /// 0 for films.
    #[serde(default)]
    pub season: i32,
    /// 0 for films.
    #[serde(default)]
    pub episode: i32,
    pub position_sec: f64,
    pub duration_sec: f64,
    pub release: Option<ReleaseRef>,
}

/// One saved position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub season: i32,
    pub episode: i32,
    pub position_sec: f64,
    pub duration_sec: f64,
    pub finished: bool,
    pub release: Option<ReleaseRef>,
    pub updated_at: DateTime<Utc>,
}

/// A "Continue watching" card on the home screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinueItem {
    /// The last position the user had for this title.
    pub last: Progress,
    /// For a series whose last episode is finished: the episode to play next
    /// (same season, episode + 1). The client checks it against TMDB.
    pub next_episode: Option<(i32, i32)>,
    /// Name and poster, filled in by the server from the catalog.
    #[serde(default)]
    pub title: Option<TitleSummary>,
}

/// An entry of the "Want to watch" list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchlistItem {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub added_at: DateTime<Utc>,
}

/// Body of `PUT /api/release-choice`: the release picked for a film or a season.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseChoiceUpdate {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    /// 0 for films.
    #[serde(default)]
    pub season: i32,
    pub infohash: String,
    pub title: Option<String>,
    /// "1080p", "2160p", ...
    pub quality: Option<String>,
    /// Voice-over, e.g. "LostFilm".
    pub voice: Option<String>,
}

/// A remembered release choice: "Смотреть" uses it without asking again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseChoice {
    #[serde(flatten)]
    pub release: ReleaseChoiceUpdate,
    pub chosen_at: DateTime<Utc>,
}

/// Body of `POST /api/auth/webapp`: the raw `Telegram.WebApp.initData` string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAppAuthRequest {
    pub init_data: String,
    /// Invite code for first-time users (from the `startapp` parameter or a link).
    pub invite: Option<String>,
}

/// Body of `POST /api/auth/widget`: the fields the Telegram Login Widget returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetAuthRequest {
    pub fields: std::collections::BTreeMap<String, String>,
    pub invite: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    User,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Me {
    pub id: i64,
    pub telegram_id: i64,
    pub first_name: Option<String>,
    pub username: Option<String>,
    pub role: Role,
}

/// Successful login: a bearer token for `Authorization: Bearer <token>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub me: Me,
}

/// Error body returned by the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    /// Stable machine-readable code, e.g. `not_invited`.
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_threshold() {
        assert!(!is_finished(0.0, 0.0));
        assert!(!is_finished(89.0, 100.0));
        assert!(is_finished(90.0, 100.0));
    }

    #[test]
    fn media_kind_round_trip() {
        for k in [MediaKind::Movie, MediaKind::Tv] {
            assert_eq!(MediaKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(MediaKind::parse("anime"), None);
    }
}

// ---------------------------------------------------------------------------
// Catalog (metadata from TMDB)
// ---------------------------------------------------------------------------

/// A title as shown on a poster card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleSummary {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    /// Localised title (Russian when TMDB has it).
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
    /// TMDB image path such as `/abc.jpg`; see [`image_url`].
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    /// TMDB user rating, 0–10.
    pub rating: Option<f32>,
    pub overview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeasonSummary {
    pub number: i32,
    pub name: String,
    pub episode_count: i32,
    pub year: Option<i32>,
}

/// Everything the title page needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleDetails {
    pub summary: TitleSummary,
    pub genres: Vec<String>,
    /// Film length, or a typical episode length for a series.
    pub runtime_min: Option<i32>,
    pub tagline: Option<String>,
    /// Empty for films. Season 0 ("Specials") comes last if present.
    pub seasons: Vec<SeasonSummary>,
    pub imdb_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Episode {
    pub number: i32,
    pub name: String,
    pub overview: Option<String>,
    pub still_path: Option<String>,
    pub runtime_min: Option<i32>,
    /// `YYYY-MM-DD`.
    pub air_date: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Season {
    pub number: i32,
    pub name: String,
    pub episodes: Vec<Episode>,
}

/// A named row of posters on the home screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogRow {
    pub title: String,
    pub items: Vec<TitleSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HomeFeed {
    pub rows: Vec<CatalogRow>,
    /// True when the server runs on built-in demo data instead of TMDB.
    pub demo: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResults {
    pub query: String,
    pub items: Vec<TitleSummary>,
}

/// Poster/backdrop sizes TMDB serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageSize {
    Poster,
    Backdrop,
    Still,
}

impl ImageSize {
    pub fn tmdb(self) -> &'static str {
        match self {
            ImageSize::Poster => "w342",
            ImageSize::Backdrop => "w1280",
            ImageSize::Still => "w300",
        }
    }
}

/// URL of a TMDB image through the server's image proxy (TMDB's CDN is not
/// reachable everywhere, so the server fetches and caches it).
pub fn image_url(path: &str, size: ImageSize) -> String {
    format!("/api/img/{}{}", size.tmdb(), path)
}

/// "S02E06".
pub fn episode_code(season: i32, episode: i32) -> String {
    format!("S{season:02}E{episode:02}")
}

/// "1:02:03" or "41:12".
pub fn clock(sec: f64) -> String {
    let s = sec.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(episode_code(2, 6), "S02E06");
        assert_eq!(clock(2472.5), "41:12");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(image_url("/a.jpg", ImageSize::Poster), "/api/img/w342/a.jpg");
    }
}
