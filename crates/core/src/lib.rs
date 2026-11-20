//! Types shared between the server, the bot and the Dioxus app.
//!
//! Everything here is plain data with serde, so it compiles both natively and
//! for `wasm32-unknown-unknown`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod release;
pub use release::*;

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

/// Answer of `POST /api/auth/link`: the phone and desktop apps log in through the bot.
/// The app opens `url`, the user presses Start there, and the app polls
/// `GET /api/auth/link/{code}` until it gets the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginLink {
    pub code: String,
    /// `https://t.me/<bot>?start=login_<code>`: works with or without the Telegram app.
    pub url: String,
    /// `tg://resolve?...`: opens the Telegram app directly.
    pub app_url: String,
    /// How long the code lives, seconds.
    pub expires_in: i64,
}

/// Body of `POST /api/auth/link`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoginLinkRequest {
    /// Invite code, for a first login.
    pub invite: Option<String>,
}

/// Answer of `GET /api/auth/link/{code}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoginPoll {
    /// The user has not pressed Start yet.
    Pending,
    /// The Telegram user has no invite.
    NotInvited,
    Done(AuthResponse),
}

/// Prefix of the bot's `/start` payload for app logins.
pub const LOGIN_PREFIX: &str = "login_";

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

static ASSET_BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Where the server lives, for clients that are not served by it (the phone and
/// desktop apps). The website leaves it unset and keeps using relative URLs.
/// Set it once at startup, before the first screen is drawn.
pub fn set_asset_base(base: impl Into<String>) {
    let _ = ASSET_BASE.set(base.into().trim_end_matches('/').to_owned());
}

/// `path` (starting with `/`) as a URL the current client can load.
pub fn asset_url(path: &str) -> String {
    format!("{}{}", ASSET_BASE.get().map(String::as_str).unwrap_or(""), path)
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

/// Defaults used when picking releases and tracks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserPrefs {
    /// "1080p", "2160p", ...
    pub quality: Option<String>,
    /// Preferred voice-over, e.g. "LostFilm".
    pub voice: Option<String>,
    /// Subtitle language, e.g. "Русский"; `None` means no subtitles.
    pub subtitle_lang: Option<String>,
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
