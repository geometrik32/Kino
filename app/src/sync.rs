//! WebSocket bridge and clock-offset math for multi-device playback sync.

use kino_core::{MediaKind, SyncClientMsg, SyncServerMsg, SyncState};
use serde::Deserialize;

use crate::platform;

/// Raw events coming out of the JS WebSocket bridge.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsBridgeEvent {
    Open,
    Msg { data: String },
    Error,
    Closed,
}

/// Keeps track of `server_now_ms - client_now_ms` using Ping/Pong RTT samples.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClockSync {
    pub offset_ms: i64,
    pub best_rtt_ms: i64,
}

impl ClockSync {
    pub fn seed_from_server(&mut self, server_now_ms: i64, local_now_ms: i64) {
        self.offset_ms = server_now_ms - local_now_ms;
        self.best_rtt_ms = i64::MAX;
    }

    pub fn on_pong(&mut self, client_ms: i64, server_ms: i64, local_now_ms: i64) {
        let rtt = (local_now_ms - client_ms).max(0);
        if rtt > 5_000 {
            return;
        }
        let sample_offset = server_ms + rtt / 2 - local_now_ms;
        if self.best_rtt_ms == i64::MAX || rtt <= self.best_rtt_ms + 25 {
            self.offset_ms = sample_offset;
            self.best_rtt_ms = self.best_rtt_ms.min(rtt);
        } else {
            // Exponential moving average when RTT is noisier than our best sample.
            self.offset_ms = (self.offset_ms * 3 + sample_offset) / 4;
        }
    }

    /// Computes where the room's playback timeline should be right now.
    pub fn expected_position(&self, state: &SyncState, local_now_ms: i64) -> f64 {
        if state.paused || state.waiting {
            return state.position_sec.max(0.0);
        }
        let server_now_est = local_now_ms + self.offset_ms;
        let dt = ((server_now_est - state.server_now_ms).max(-2_000) as f64) / 1000.0;
        let raw = state.position_sec + dt * f64::from(state.rate);
        match state.duration_sec {
            Some(d) if d.is_finite() && d > 0.0 => raw.clamp(0.0, d),
            _ => raw.max(0.0),
        }
    }
}

pub fn set_stream_offset(offset_sec: f64) {
    let _ = dioxus::document::eval(&format!("window.__kinoSyncOffset = {offset_sec};"));
}

pub fn send(msg: &SyncClientMsg) {
    let Ok(json) = serde_json::to_string(msg) else {
        return;
    };
    let Ok(escaped) = serde_json::to_string(&json) else {
        return;
    };
    let _ = dioxus::document::eval(&format!(
        "if (window.__kinoSyncSend) {{ try {{ window.__kinoSyncSend({escaped}); }} catch (e) {{}} }}"
    ));
}

pub fn stop(leave: Option<&SyncClientMsg>) {
    let arg = match leave.and_then(|m| serde_json::to_string(m).ok()) {
        Some(json) => serde_json::to_string(&json).unwrap_or_else(|_| "null".into()),
        None => "null".into(),
    };
    let _ = dioxus::document::eval(&format!(
        "if (window.__kinoSyncStop) {{ try {{ window.__kinoSyncStop({arg}); }} catch (e) {{}} }}"
    ));
}

pub fn start_bridge(
    ticket: &str,
    token: &str,
    client_id: &str,
    kind: MediaKind,
    tmdb_id: i64,
) -> dioxus::document::Eval {
    let ws_url = platform::ws_url(&format!("/api/sync/ws?ticket={}", urlencoding::encode(ticket)));
    let leave_url = platform::abs("/api/sync/leave");
    let ws_js = serde_json::to_string(&ws_url).unwrap_or_else(|_| "\"\"".into());
    let leave_js = serde_json::to_string(&leave_url).unwrap_or_else(|_| "\"\"".into());
    let token_js = serde_json::to_string(token).unwrap_or_else(|_| "\"\"".into());
    let cid_js = serde_json::to_string(client_id).unwrap_or_else(|_| "\"\"".into());
    let kind_js = serde_json::to_string(kind.as_str()).unwrap_or_else(|_| "\"movie\"".into());

    let script = format!(
        "(function() {{
          if (window.__kinoSyncStop) {{
            try {{ window.__kinoSyncStop(null); }} catch (e) {{}}
          }}
          var ws = new WebSocket({ws_js});
          var stopped = false;
          var unloadHandler = function() {{
            if (stopped) return;
            stopped = true;
            var v = document.getElementById('kino-video');
            var off = Number(window.__kinoSyncOffset || 0);
            var pos = off + ((v && isFinite(v.currentTime)) ? v.currentTime : 0);
            var dur = (v && isFinite(v.duration)) ? v.duration : null;
            try {{
              if (ws && ws.readyState === 1) {{
                ws.send(JSON.stringify({{ type: 'leave', position_sec: pos, duration_sec: dur, finished: false }}));
                ws.close(1000, 'unload');
              }}
            }} catch (e) {{}}
            try {{
              fetch({leave_js}, {{
                method: 'POST',
                headers: {{ 'Content-Type': 'application/json', 'Authorization': 'Bearer ' + {token_js} }},
                body: JSON.stringify({{ client_id: {cid_js}, kind: {kind_js}, tmdb_id: {tmdb_id}, position_sec: pos, duration_sec: dur, finished: false }}),
                keepalive: true
              }});
            }} catch (e) {{}}
          }};
          window.addEventListener('pagehide', unloadHandler);
          window.addEventListener('beforeunload', unloadHandler);
          ws.onopen = function() {{
            if (!stopped) dioxus.send(JSON.stringify({{ type: 'open' }}));
          }};
          ws.onmessage = function(ev) {{
            if (!stopped && typeof ev.data === 'string') {{
              dioxus.send(JSON.stringify({{ type: 'msg', data: ev.data }}));
            }}
          }};
          ws.onerror = function() {{
            if (!stopped) dioxus.send(JSON.stringify({{ type: 'error' }}));
          }};
          ws.onclose = function() {{
            if (!stopped) dioxus.send(JSON.stringify({{ type: 'closed' }}));
          }};
          window.__kinoSyncSend = function(text) {{
            if (!stopped && ws && ws.readyState === 1) {{
              ws.send(text);
            }}
          }};
          window.__kinoSyncStop = function(leaveText) {{
            stopped = true;
            window.removeEventListener('pagehide', unloadHandler);
            window.removeEventListener('beforeunload', unloadHandler);
            window.__kinoSyncSend = null;
            window.__kinoSyncStop = null;
            try {{
              if (leaveText && ws && ws.readyState === 1) {{
                ws.send(leaveText);
              }}
            }} catch (e) {{}}
            try {{
              if (ws && (ws.readyState === 0 || ws.readyState === 1)) {{
                ws.close(1000, 'leave');
              }}
            }} catch (e) {{}}
          }};
        }})();"
    );
    dioxus::document::eval(&script)
}

pub fn parse_server_msg(raw: &str) -> Option<SyncServerMsg> {
    serde_json::from_str(raw).ok()
}
