//! Types shared between the server, the bot and the Dioxus app.
//!
//! Everything here is plain data with serde, so it compiles both natively and
//! for `wasm32-unknown-unknown`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod release;
pub use release::*;
mod stream;
pub use stream::*;

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
    /// Device name for the watch history, e.g. "Телефон" or "Chrome на Windows".
    #[serde(default)]
    pub device: Option<String>,
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
    /// Not touched for a long time: the UI offers to mark it "Брошено".
    #[serde(default)]
    pub stale: bool,
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

/// Body of `POST /api/auth/link`: start logging in through the Telegram app.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoginLinkRequest {
    pub invite: Option<String>,
}

/// Answer of `POST /api/auth/link`: open `url` (the bot in Telegram), then poll
/// `GET /api/auth/link/{token}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginLink {
    pub token: String,
    pub url: String,
}

/// Answer of `GET /api/auth/link/{token}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum LoginLinkStatus {
    /// Not confirmed in Telegram yet.
    Pending,
    /// Confirmed; the link is used up.
    Done { auth: AuthResponse },
    /// Too old or unknown; start again.
    Expired,
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
    /// Relative path (such as `/api/avatar/...`) or URL of the user's Telegram photo.
    #[serde(default)]
    pub avatar_url: Option<String>,
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
    /// Latest aired episode `(season, episode)` of a series.
    #[serde(default)]
    pub last_episode: Option<(i32, i32)>,
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

/// An actor in the cast of a title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CastMember {
    pub id: i64,
    pub name: String,
    /// The role played, e.g. "Уолтер Уайт".
    pub character: Option<String>,
    /// TMDB image path of the photo; see [`image_url`] with [`ImageSize::Profile`].
    pub profile_path: Option<String>,
}

/// An actor's page: who they are and what they appeared in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonDetails {
    pub id: i64,
    pub name: String,
    /// Russian when TMDB has it, else empty.
    pub biography: Option<String>,
    /// `YYYY-MM-DD`.
    pub birthday: Option<String>,
    pub deathday: Option<String>,
    pub place_of_birth: Option<String>,
    pub profile_path: Option<String>,
    /// Films and series, best known first, each once.
    pub credits: Vec<TitleSummary>,
}

/// What the title page shows below the description: `GET /api/catalog/{kind}/{id}/extras`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TitleExtras {
    /// Billing order, the first ones are the leads.
    pub cast: Vec<CastMember>,
    /// "Рекомендации": what TMDB suggests to those who liked the title.
    pub recommendations: Vec<TitleSummary>,
    /// "Похожие": alike by genre and keywords.
    pub similar: Vec<TitleSummary>,
}

fn default_true() -> bool {
    true
}

fn default_page() -> u32 {
    1
}

/// A quick category filter chip shown on the home screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeCategory {
    pub id: String,
    pub label: String,
}

/// A named row of posters on the home screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogRow {
    /// Stable shelf identifier used for lazy loading and pagination.
    #[serde(default)]
    pub id: String,
    pub title: String,
    /// Optional short explanation shown next to the shelf title.
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Category chip IDs this row belongs to.
    #[serde(default)]
    pub categories: Vec<String>,
    /// False for deferred rows whose items are fetched on demand.
    #[serde(default = "default_true")]
    pub loaded: bool,
    /// True when more pages exist on `/api/catalog/shelf/{id}`.
    #[serde(default)]
    pub has_more: bool,
    /// 1-based page currently loaded (0 while `loaded == false`).
    #[serde(default = "default_page")]
    pub page: u32,
    pub items: Vec<TitleSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HomeFeed {
    pub rows: Vec<CatalogRow>,
    #[serde(default)]
    pub categories: Vec<HomeCategory>,
    /// True when the server runs on built-in demo data instead of TMDB.
    pub demo: bool,
}

/// One page of titles for a shelf (`GET /api/catalog/shelf/{id}?page=N`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShelfPage {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    pub page: u32,
    pub has_more: bool,
    pub items: Vec<TitleSummary>,
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
    /// Photo of an actor.
    Profile,
}

impl ImageSize {
    pub fn tmdb(self) -> &'static str {
        match self {
            ImageSize::Poster => "w342",
            ImageSize::Backdrop => "w1280",
            ImageSize::Still => "w300",
            ImageSize::Profile => "w185",
        }
    }
}

static ASSET_BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Where the server lives, for clients that are not served by it (the phone and
/// desktop apps). The website leaves it unset and keeps using relative URLs.
/// Set it once at startup, before the first screen is drawn.
pub fn set_asset_base(base: impl Into<String>) {
    let _ = ASSET_BASE.set(base.into().trim_end_matches('/').to_owned());
}

/// `path` (starting with `/`) as a URL the current client can load; full URLs pass through unchanged.
pub fn asset_url(path: &str) -> String {
    if path.starts_with('/') {
        format!("{}{}", ASSET_BASE.get().map(String::as_str).unwrap_or(""), path)
    } else {
        path.to_owned()
    }
}

/// URL of a TMDB image through the server's image proxy (TMDB's CDN is not
/// reachable everywhere, so the server fetches and caches it).
pub fn image_url(path: &str, size: ImageSize) -> String {
    asset_url(&format!("/api/img/{}{}", size.tmdb(), path))
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

// ---------------------------------------------------------------------------
// Per-user state of a title
// ---------------------------------------------------------------------------

/// Where a title is for a user. Changes automatically while watching; the
/// user can also set it by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleStatus {
    /// Started: set on the first play.
    Watching,
    /// The film, or every aired episode, is watched.
    Watched,
    /// "Хочу посмотреть"; cleared when playback starts.
    Want,
    /// Given up; hidden from "Continue watching".
    Dropped,
}

impl TitleStatus {
    pub const ALL: [TitleStatus; 4] = [
        TitleStatus::Watching,
        TitleStatus::Want,
        TitleStatus::Watched,
        TitleStatus::Dropped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TitleStatus::Watching => "watching",
            TitleStatus::Watched => "watched",
            TitleStatus::Want => "want",
            TitleStatus::Dropped => "dropped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            TitleStatus::Watching => "Смотрю",
            TitleStatus::Watched => "Просмотрено",
            TitleStatus::Want => "Хочу посмотреть",
            TitleStatus::Dropped => "Брошено",
        }
    }
}

/// Everything the user has set or the server has derived for one title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleState {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub status: Option<TitleStatus>,
    /// 1–5 stars.
    pub rating: Option<i16>,
    /// Audio and subtitle tracks picked in the player, restored next time.
    pub audio_track: Option<String>,
    pub subtitle_track: Option<String>,
    pub updated_at: DateTime<Utc>,
    /// Name and poster, filled in by the server for lists.
    #[serde(default)]
    pub title: Option<TitleSummary>,
}

/// Body of `PUT /api/title/{kind}/{id}/status`; `null` clears it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusUpdate {
    pub status: Option<TitleStatus>,
}

/// Body of `PUT /api/title/{kind}/{id}/rating`: 1–5, or `null` to remove.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RatingUpdate {
    pub rating: Option<i16>,
}

/// Body of `PUT /api/title/{kind}/{id}/tracks`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TracksUpdate {
    pub audio_track: Option<String>,
    pub subtitle_track: Option<String>,
}

/// Body of `POST /api/marks`: mark episodes (or a film) watched or not,
/// e.g. when they were watched somewhere else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkWatched {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    /// 0 for films.
    #[serde(default)]
    pub season: i32,
    /// Episode numbers; ignored for films.
    #[serde(default)]
    pub episodes: Vec<i32>,
    pub watched: bool,
}

/// One viewing session in the history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub season: i32,
    pub episode: i32,
    pub started_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub position_from: f64,
    pub position_to: f64,
    pub device: Option<String>,
    #[serde(default)]
    pub title: Option<TitleSummary>,
}

/// Defaults used when picking releases and tracks. Every field is optional:
/// `None` means "no preference".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserPrefs {
    /// "1080p", "2160p", ...
    pub quality: Option<String>,
    /// Studio of the voice-over. No longer set from the profile (there are too many
    /// studios to list); still used to pick an audio track when an old value is stored.
    pub voice: Option<String>,
    /// Subtitle language in the player, e.g. "Русский"; `None` means no subtitles.
    pub subtitle_lang: Option<String>,
    /// Audio language, e.g. "Русский": filters the releases and picks the audio track.
    #[serde(default)]
    pub language: Option<String>,
    /// Release filters: `Some(true)` wants it, `Some(false)` does not.
    #[serde(default)]
    pub hdr: Option<bool>,
    #[serde(default)]
    pub dolby_vision: Option<bool>,
    /// The release has subtitles.
    #[serde(default)]
    pub subtitles: Option<bool>,
    /// "H.264", "HEVC" or "AV1".
    #[serde(default)]
    pub codec: Option<String>,
    /// Plays in the browser without re-encoding.
    #[serde(default)]
    pub direct_play: Option<bool>,
    /// At least this many seeders.
    #[serde(default)]
    pub min_seeders: Option<u32>,
    /// At most this many gigabytes.
    #[serde(default)]
    pub max_size_gb: Option<u32>,
    /// How the releases are sorted: "seeders", "size", "bitrate", "name", "source", "date", "watched".
    #[serde(default)]
    pub sort: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreloadState {
    Queued,
    Downloading,
    Ready,
    Failed,
    /// Removed from the server's disk (after it was watched, or by hand).
    Deleted,
}

impl PreloadState {
    pub fn as_str(self) -> &'static str {
        match self {
            PreloadState::Queued => "queued",
            PreloadState::Downloading => "downloading",
            PreloadState::Ready => "ready",
            PreloadState::Failed => "failed",
            PreloadState::Deleted => "deleted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            PreloadState::Queued,
            PreloadState::Downloading,
            PreloadState::Ready,
            PreloadState::Failed,
            PreloadState::Deleted,
        ]
        .into_iter()
        .find(|p| p.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            PreloadState::Queued => "В очереди",
            PreloadState::Downloading => "Скачивается",
            PreloadState::Ready => "Готово",
            PreloadState::Failed => "Ошибка",
            PreloadState::Deleted => "Удалено",
        }
    }
}

/// Body of `POST /api/preload`: download a release to the server in advance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreloadRequest {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    /// 0 for films.
    #[serde(default)]
    pub season: i32,
    pub infohash: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreloadItem {
    pub id: i64,
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub season: i32,
    pub infohash: String,
    pub title: Option<String>,
    pub state: PreloadState,
    /// 0.0–1.0 while downloading.
    pub progress: f32,
    pub size_bytes: Option<i64>,
    pub requested_at: DateTime<Utc>,
    pub ready_at: Option<DateTime<Utc>>,
    /// Set once watched: the files are removed after this moment.
    pub delete_after: Option<DateTime<Utc>>,
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
