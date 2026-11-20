//! How far a torrent is from being watchable, for the progress shown while connecting.

use serde::{Deserialize, Serialize};

/// Where the connection to a release stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPhase {
    /// Looking for peers and the list of files; nothing is known about the size yet.
    #[default]
    Connecting,
    /// Loading the first part of the video; the percent is meaningful.
    Buffering,
    /// The first part is loaded.
    Ready,
}

/// Answer of `GET /api/play/progress/{release}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamProgress {
    pub phase: StreamPhase,
    /// Bytes of the first part loaded so far.
    pub loaded_bytes: u64,
    /// Bytes needed before playback starts; 0 while unknown.
    pub needed_bytes: u64,
    /// Peers we are exchanging data with.
    pub peers: u32,
    /// Of them, the ones that have the whole file.
    pub seeders: u32,
    /// Download speed, bytes per second.
    pub speed_bps: u64,
}

impl StreamProgress {
    /// 0–100 once the amount to load is known.
    pub fn percent(&self) -> Option<u8> {
        match self.phase {
            StreamPhase::Ready => Some(100),
            StreamPhase::Buffering if self.needed_bytes > 0 => {
                Some((self.loaded_bytes.saturating_mul(100) / self.needed_bytes).min(100) as u8)
            }
            _ => None,
        }
    }

    /// Seconds until the first part is loaded, at the current speed.
    pub fn eta_sec(&self) -> Option<u64> {
        if self.phase != StreamPhase::Buffering || self.speed_bps == 0 || self.needed_bytes == 0 {
            return None;
        }
        let left = self.needed_bytes.saturating_sub(self.loaded_bytes);
        Some(left.div_ceil(self.speed_bps))
    }

    /// What is going on, in a few words.
    pub fn headline(&self) -> &'static str {
        match self.phase {
            StreamPhase::Connecting if self.peers == 0 => "Ищем раздающих…",
            StreamPhase::Connecting => "Получаем данные раздачи…",
            StreamPhase::Buffering => "Подгружаем начало видео",
            StreamPhase::Ready => "Запускаем видео…",
        }
    }

    /// The line under the progress bar, e.g. "12 из 32 МБ · 8 раздающих · 2.4 МБ/с · ещё ≈ 9 с".
    pub fn details(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.phase == StreamPhase::Buffering && self.needed_bytes > 0 {
            parts.push(format!(
                "{} из {} МБ",
                trim_mb(self.loaded_bytes),
                trim_mb(self.needed_bytes)
            ));
        }
        if self.peers > 0 {
            let n = self.peers;
            parts.push(format!("{n} {}", plural_peers(n)));
        }
        if self.speed_bps > 0 {
            parts.push(format!("{} МБ/с", trim_mb(self.speed_bps)));
        }
        if let Some(eta) = self.eta_sec() {
            parts.push(format!("ещё ≈ {}", human_wait(eta)));
        }
        // No line break inside a part: "ещё ≈ 9 с" must not be split.
        parts
            .iter()
            .map(|p| p.replace(' ', "\u{a0}"))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// Megabytes with one decimal below 10, whole above: "2.4", "32".
fn trim_mb(bytes: u64) -> String {
    let m = bytes as f64 / 1_048_576.0;
    if m >= 10.0 {
        format!("{m:.0}")
    } else {
        format!("{m:.1}")
    }
}

fn plural_peers(n: u32) -> &'static str {
    if n % 10 == 1 && n % 100 != 11 {
        "раздающий"
    } else {
        "раздающих"
    }
}

/// "9 с", "1 мин 20 с".
pub fn human_wait(sec: u64) -> String {
    if sec < 60 {
        format!("{sec} с")
    } else if sec < 3600 {
        format!("{} мин {} с", sec / 60, sec % 60)
    } else {
        format!("{} ч {} мин", sec / 3600, sec / 60 % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_and_eta() {
        let mut p = StreamProgress {
            phase: StreamPhase::Connecting,
            loaded_bytes: 5 << 20,
            needed_bytes: 0,
            peers: 3,
            seeders: 1,
            speed_bps: 0,
        };
        assert_eq!(p.percent(), None);
        assert_eq!(p.eta_sec(), None);
        p.phase = StreamPhase::Buffering;
        p.needed_bytes = 20 << 20;
        p.speed_bps = 1 << 20;
        assert_eq!(p.percent(), Some(25));
        assert_eq!(p.eta_sec(), Some(15));
        assert_eq!(
            p.details().replace('\u{a0}', " "),
            "5.0 из 20 МБ · 3 раздающих · 1.0 МБ/с · ещё ≈ 15 с"
        );
        p.loaded_bytes = 40 << 20;
        assert_eq!(p.percent(), Some(100), "never above 100");
        p.phase = StreamPhase::Ready;
        assert_eq!(p.percent(), Some(100));
        assert_eq!(p.eta_sec(), None);
    }

    #[test]
    fn waits_read_naturally() {
        assert_eq!(human_wait(9), "9 с");
        assert_eq!(human_wait(80), "1 мин 20 с");
        assert_eq!(plural_peers(1), "раздающий");
        assert_eq!(plural_peers(5), "раздающих");
    }
}
