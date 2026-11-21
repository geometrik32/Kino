//! Real-time playback synchronization types shared between server and clients.

use serde::{Deserialize, Serialize};

use crate::{MediaKind, ReleaseChoiceUpdate};

/// One connected device inside a synchronized playback room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncMemberInfo {
    pub client_id: String,
    pub device: String,
    pub ready: bool,
    pub buffering: bool,
}

/// Body of `POST /api/sync/join`: register (or join) the live playback room for a title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncJoinRequest {
    pub client_id: String,
    pub device: String,
    pub kind: MediaKind,
    pub tmdb_id: i64,
    /// 0 for films.
    #[serde(default)]
    pub season: i32,
    /// 0 for films.
    #[serde(default)]
    pub episode: i32,
    pub release: ReleaseChoiceUpdate,
    /// True when the user explicitly picked a specific episode instead of Resume.
    #[serde(default)]
    pub explicit_episode: bool,
    /// True when the user explicitly picked a release in `ReleasePicker`.
    #[serde(default)]
    pub explicit_release: bool,
    /// Position used only when creating a fresh room on reconnect after a brief drop.
    #[serde(default)]
    pub reconnect_pos: Option<f64>,
}

/// Answer of `POST /api/sync/join`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncJoinResponse {
    /// Short-lived HMAC ticket passed to `GET /api/sync/ws?ticket=...`.
    pub ticket: String,
    pub state: SyncState,
    pub joined_existing: bool,
}

/// Body of `POST /api/sync/leave`: clean disconnect + final progress flush.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncLeaveRequest {
    pub client_id: String,
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub position_sec: f64,
    pub duration_sec: Option<f64>,
    #[serde(default)]
    pub finished: bool,
}

/// What triggered the latest [`SyncState`] revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncEventKind {
    Snapshot,
    Play,
    Pause,
    Seek,
    Rate,
    Episode,
    Release,
    Audio,
    Buffering,
    Resumed,
    Members,
    Heartbeat,
}

/// Authoritative room state broadcast by the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncState {
    pub revision: u64,
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub season: i32,
    pub episode: i32,
    pub release: ReleaseChoiceUpdate,
    /// Playback position in seconds at `server_now_ms`.
    pub position_sec: f64,
    /// Total duration in seconds when known.
    pub duration_sec: Option<f64>,
    /// True when intentionally paused by the user.
    pub paused: bool,
    /// True when temporarily held waiting for one or more members to buffer/seek.
    pub waiting: bool,
    /// Human-readable device names currently holding the barrier.
    #[serde(default)]
    pub waiting_for: Vec<String>,
    /// Playback speed (1.0 is normal).
    pub rate: f32,
    /// Label of the selected audio track, when known.
    #[serde(default)]
    pub audio_track: Option<String>,
    /// Server unix epoch milliseconds when `position_sec` was computed.
    pub server_now_ms: i64,
    /// Client that originated this update (`None` for server barrier/disconnect events).
    #[serde(default)]
    pub updated_by: Option<String>,
    pub event: SyncEventKind,
    pub members: Vec<SyncMemberInfo>,
}

/// Messages sent from a player client to the server over WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncClientMsg {
    /// Client loaded/seeked to `position_sec` and is ready to play.
    Ready {
        position_sec: f64,
        #[serde(default)]
        duration_sec: Option<f64>,
        #[serde(default)]
        file_index: Option<i32>,
    },
    /// Client is buffering mid-playback (debounced).
    Buffering {
        position_sec: f64,
    },
    Play {
        position_sec: f64,
    },
    Pause {
        position_sec: f64,
    },
    Seek {
        position_sec: f64,
    },
    Rate {
        rate: f32,
        position_sec: f64,
    },
    Episode {
        season: i32,
        episode: i32,
        #[serde(default)]
        release: Option<ReleaseChoiceUpdate>,
    },
    Release {
        release: ReleaseChoiceUpdate,
    },
    Audio {
        audio_track: Option<String>,
        position_sec: f64,
    },
    /// Periodic heartbeat & drift report while playing.
    Report {
        position_sec: f64,
        #[serde(default)]
        duration_sec: Option<f64>,
        paused: bool,
        rate: f32,
    },
    /// Clock offset / RTT calibration ping.
    Ping {
        client_ms: i64,
    },
    /// Clean exit before closing the socket.
    Leave {
        position_sec: f64,
        #[serde(default)]
        duration_sec: Option<f64>,
        #[serde(default)]
        finished: bool,
    },
}

/// Messages sent from the server to a player client over WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncServerMsg {
    State(Box<SyncState>),
    Pong { client_ms: i64, server_ms: i64 },
    Error { message: String },
}
