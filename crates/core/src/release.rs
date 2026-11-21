//! Torrent releases offered for a title.

use serde::{Deserialize, Serialize};

use crate::{ReleaseChoiceUpdate, TitleSummary};

/// A torrent release offered for a title.
///
/// Comes from the indexer (Jackett), or from [`demo_releases`] when none is configured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseOption {
    /// BitTorrent info-hash (40 lowercase hex chars) when the indexer gave one.
    /// Otherwise a 40-char key starting with `u`, which the server turns into the
    /// real info-hash when the release is chosen (it downloads the `.torrent`).
    pub infohash: String,
    pub title: String,
    /// "2160p", "1080p", "720p".
    pub quality: String,
    pub size_bytes: u64,
    pub seeders: u32,
    /// Voice-over / translation, e.g. "LostFilm" or "Дубляж".
    pub voice: String,
    /// Audio language, e.g. "Русский".
    pub language: String,
    /// Where the release was found, e.g. "RuTracker".
    pub tracker: String,
    /// Upload date, `YYYY-MM-DD`.
    pub added: String,
    /// Release year of the video.
    pub year: Option<i32>,
    /// Season of a series pack.
    pub season: Option<i32>,
    pub hdr: bool,
    pub dolby_vision: bool,
    pub subtitles: bool,
    /// Whether a browser can play it as-is (H.264/AAC); otherwise the server transcodes.
    pub direct_play: bool,
    /// Video codec when known, e.g. "HEVC" or "H.264".
    #[serde(default)]
    pub codec: Option<String>,
    /// Bitrate in kbit/s: as written in the release name, else worked out from
    /// the size and the running time.
    #[serde(default)]
    pub bitrate_kbps: Option<u32>,
    /// `bitrate_kbps` is an estimate (size ÷ running time, so it includes the sound).
    #[serde(default)]
    pub bitrate_approx: bool,
}

/// Average bitrate of a file of `size_bytes` that runs `minutes`, in kbit/s.
pub fn estimate_bitrate_kbps(size_bytes: u64, minutes: f64) -> Option<u32> {
    if size_bytes == 0 || minutes < 1.0 {
        return None;
    }
    let kbps = size_bytes as f64 * 8.0 / 1000.0 / (minutes * 60.0);
    (kbps >= 1.0 && kbps < f64::from(u32::MAX)).then_some(kbps.round() as u32)
}

/// "8.4 Мбит/с", "12 Мбит/с", "640 Кбит/с".
pub fn format_bitrate(kbps: u32) -> String {
    let mbps = f64::from(kbps) / 1000.0;
    if kbps < 1000 {
        format!("{kbps} Кбит/с")
    } else if mbps >= 10.0 {
        format!("{mbps:.0} Мбит/с")
    } else {
        format!("{mbps:.1} Мбит/с")
    }
}

impl ReleaseOption {
    /// Short label for the title page, e.g. "LostFilm · 1080p · RuTracker".
    pub fn label(&self) -> String {
        format!("{} · {} · {}", self.voice, self.quality, self.tracker)
    }

    /// What to remember when the user picks this release.
    pub fn to_choice(&self, title: &TitleSummary, season: i32) -> ReleaseChoiceUpdate {
        ReleaseChoiceUpdate {
            kind: title.kind,
            tmdb_id: title.tmdb_id,
            season,
            infohash: self.infohash.clone(),
            title: Some(self.title.clone()),
            quality: Some(self.quality.clone()),
            voice: Some(self.voice.clone()),
        }
    }
}

impl ReleaseOption {
    /// Whether [`ReleaseOption::infohash`] is a real info-hash, not a placeholder key.
    pub fn has_infohash(&self) -> bool {
        is_infohash(&self.infohash)
    }
}

/// 40 lowercase hex characters.
pub fn is_infohash(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A real info-hash, or the `u…` placeholder key of a release not downloaded yet.
pub fn is_release_key(s: &str) -> bool {
    is_infohash(s)
        || (s.len() == 40
            && s.starts_with('u')
            && s[1..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// Answer of `GET /api/releases/{kind}/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseList {
    pub releases: Vec<ReleaseOption>,
    /// False when no indexer is configured and the list is demo data.
    pub configured: bool,
}

/// Sample releases, used when no indexer is configured and for screenshots.
pub fn demo_releases(title: &TitleSummary, season: Option<i32>) -> Vec<ReleaseOption> {
    let original = title.original_title.clone().unwrap_or_else(|| title.title.clone());
    let year = title.year;
    let tv = season.is_some();
    #[allow(clippy::type_complexity)]
    let rows: &[(
        char,
        &str,
        &str,
        f64,
        u32,
        &str,
        &str,
        &str,
        &str,
        i32,
        bool,
        bool,
        bool,
        bool,
    )] = &[
        // hash, quality, rip, GB, seeds, voice, language, tracker, added, season offset, hdr, dv, subs, direct
        (
            'a',
            "1080p",
            "WEB-DL",
            28.4,
            214,
            "LostFilm",
            "Русский",
            "RuTracker",
            "2024-03-08",
            0,
            false,
            false,
            true,
            true,
        ),
        (
            'b',
            "2160p",
            "WEB-DL HDR | HEVC",
            96.1,
            41,
            "Дубляж",
            "Русский",
            "Kinozal",
            "2025-01-19",
            0,
            true,
            false,
            true,
            false,
        ),
        (
            'c',
            "1080p",
            "BDRip | x264",
            21.7,
            87,
            "NewStudio",
            "Русский",
            "RuTor",
            "2023-11-02",
            0,
            false,
            false,
            false,
            true,
        ),
        (
            'd',
            "720p",
            "HDTVRip",
            11.0,
            3,
            "Кубик в Кубе",
            "Русский",
            "NNM-Club",
            "2019-06-14",
            0,
            false,
            false,
            false,
            true,
        ),
        (
            'e',
            "2160p",
            "BDRemux | DV | Atmos",
            214.0,
            2,
            "Оригинал",
            "Английский",
            "RuTracker",
            "2025-06-30",
            0,
            true,
            true,
            true,
            false,
        ),
        (
            'f',
            "1080p",
            "WEB-DL",
            26.9,
            56,
            "LostFilm",
            "Русский",
            "RuTor",
            "2024-02-11",
            0,
            false,
            false,
            false,
            true,
        ),
        (
            'g',
            "2160p",
            "WEB-DL SDR",
            64.3,
            19,
            "Дубляж",
            "Русский",
            "RuTracker",
            "2024-09-21",
            0,
            false,
            false,
            true,
            false,
        ),
        (
            'h',
            "1080p",
            "WEB-DLRip",
            14.2,
            128,
            "Дубляж",
            "Русский",
            "Kinozal",
            "2024-05-05",
            -1,
            false,
            false,
            false,
            true,
        ),
        (
            'i',
            "1080p",
            "WEB-DL",
            23.0,
            9,
            "Ozz",
            "Украинский",
            "Toloka",
            "2023-08-17",
            0,
            false,
            false,
            true,
            true,
        ),
        (
            'j',
            "720p",
            "WEB-DL",
            6.8,
            33,
            "Оригинал",
            "Английский",
            "RuTracker",
            "2022-12-01",
            1,
            false,
            false,
            true,
            true,
        ),
    ];
    rows.iter()
        .map(
            |&(h, q, rip, gb, seeds, voice, lang, tracker, added, ds, hdr, dv, subs, direct)| {
                let s = season.map(|s| (s + ds).max(1));
                let what = match s {
                    Some(s) => format!("S{s:02}"),
                    None => year.map(|y| y.to_string()).unwrap_or_default(),
                };
                let size_bytes = ((if tv { gb } else { gb / 2.6 }) * 1e9) as u64;
                ReleaseOption {
                    infohash: h.to_string().repeat(40),
                    title: format!("{} / {original} ({what}) {q} {rip} | {voice}", title.title),
                    quality: q.into(),
                    size_bytes,
                    seeders: seeds,
                    voice: voice.into(),
                    language: lang.into(),
                    tracker: tracker.into(),
                    added: added.into(),
                    year,
                    season: s,
                    hdr,
                    dolby_vision: dv,
                    subtitles: subs,
                    direct_play: direct,
                    codec: Some(if direct { "H.264" } else { "HEVC" }.into()),
                    // A film runs about 140 minutes, a season about ten 50-minute episodes.
                    bitrate_kbps: estimate_bitrate_kbps(size_bytes, if tv { 500.0 } else { 140.0 }),
                    // A few releases state the bitrate in their name; the rest are estimated.
                    bitrate_approx: !matches!(h, 'b' | 'g'),
                }
            },
        )
        .collect()
}

/// Body of `POST /api/play`: start (or resume) playing a release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayRequest {
    pub kind: crate::MediaKind,
    pub tmdb_id: i64,
    /// 0 for films.
    #[serde(default)]
    pub season: i32,
    /// 0 for films.
    #[serde(default)]
    pub episode: i32,
    /// [`ReleaseOption::infohash`] of the chosen release.
    pub release: String,
    /// Client uses a native player (ExoPlayer / libmpv) that decodes MKV/HEVC/AC3/DTS directly.
    #[serde(default)]
    pub native: bool,
}

/// An audio or subtitle track of the video file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaTrack {
    /// Position among tracks of the same type, from 0.
    pub index: i32,
    /// ISO 639-2 code, e.g. "rus".
    pub language: Option<String>,
    /// Track name from the file, e.g. "LostFilm".
    pub title: Option<String>,
    pub codec: String,
    pub channels: Option<i32>,
    pub default: bool,
}

impl MediaTrack {
    /// "LostFilm · rus · 5.1".
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(t) = self.title.as_ref().filter(|t| !t.trim().is_empty()) {
            parts.push(t.trim().to_owned());
        }
        if let Some(l) = &self.language {
            parts.push(language_name(l).to_owned());
        }
        match self.channels {
            Some(6) => parts.push("5.1".into()),
            Some(8) => parts.push("7.1".into()),
            _ => {}
        }
        if parts.is_empty() {
            format!("Дорожка {}", self.index + 1)
        } else {
            parts.join(" · ")
        }
    }
}

/// "rus" → "Русский"; unknown codes are shown as they are.
pub fn language_name(code: &str) -> &str {
    match code {
        "rus" | "ru" => "Русский",
        "eng" | "en" => "Английский",
        "ukr" | "uk" => "Украинский",
        "jpn" | "ja" => "Японский",
        "ger" | "deu" | "de" => "Немецкий",
        "fre" | "fra" | "fr" => "Французский",
        "spa" | "es" => "Испанский",
        "ita" | "it" => "Итальянский",
        "kor" | "ko" => "Корейский",
        "chi" | "zho" | "zh" => "Китайский",
        other => other,
    }
}

/// Answer of `POST /api/play`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayInfo {
    pub infohash: String,
    pub file_index: i32,
    pub file_name: String,
    /// Stream path, e.g. `/api/stream/<token>`. Add `?audio=N&start=S` to get a
    /// version the browser can play (re-encoded on the fly) from second `S`.
    pub stream: String,
    /// Whether the browser plays the file as it is (no `audio`/`start` needed).
    pub direct: bool,
    /// Seconds, when known.
    pub duration: Option<f64>,
    pub audio: Vec<MediaTrack>,
    pub subtitles: Vec<MediaTrack>,
    /// Remembered or preferred tracks.
    pub audio_track: Option<i32>,
    pub subtitle_track: Option<i32>,
    /// Where to resume, seconds.
    pub start_at: i32,
    /// The episode after this one, `(season, episode)`.
    pub next: Option<(i32, i32)>,
    /// Played from the server's disk (downloaded in advance).
    pub preloaded: bool,
    /// Client requested native direct playback (MKV/HEVC/AC3/DTS without server transcoding).
    #[serde(default)]
    pub native: bool,
}

/// Body of `POST /api/play/probe`: retries probing an active stream when
/// metadata was not yet ready at playback start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayProbeRequest {
    pub kind: crate::MediaKind,
    pub tmdb_id: i64,
    /// Stream path or full URL (`/api/stream/<token>`).
    pub stream: String,
    #[serde(default)]
    pub native: bool,
}

/// Answer of `POST /api/play/probe`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayProbe {
    /// Updated stream path when probing discovered e.g. copyable H.264 video.
    pub stream: Option<String>,
    pub direct: bool,
    pub duration: Option<f64>,
    pub audio: Vec<MediaTrack>,
    pub subtitles: Vec<MediaTrack>,
    pub audio_track: Option<i32>,
    pub subtitle_track: Option<i32>,
}

impl PlayInfo {
    /// Whether the client can play the file as it is with this audio track.
    pub fn plays_directly(&self, audio: Option<i32>) -> bool {
        if self.native {
            return true;
        }
        let default_audio = match audio {
            None => true,
            Some(a) => {
                self.audio.len() <= 1
                    || self.audio.iter().any(|t| t.index == a && t.default)
                    || (!self.audio.iter().any(|t| t.default) && self.audio.first().is_some_and(|t| t.index == a))
            }
        };
        self.direct && default_audio
    }

    /// Re-encoded stream with this audio track, starting at second `start`.
    pub fn transcoded_url(&self, audio: Option<i32>, start: f64) -> String {
        format!(
            "{}?audio={}&start={}",
            self.stream,
            audio.unwrap_or(0),
            start.max(0.0).floor() as i64
        )
    }

    /// WebVTT URL of a subtitle track; `start` must match the video's (0 when direct).
    pub fn subtitle_url(&self, index: i32, start: f64) -> String {
        let start = start.max(0.0).floor() as i64;
        if start > 0 {
            format!("{}/sub/{index}.vtt?start={start}", self.stream)
        } else {
            format!("{}/sub/{index}.vtt", self.stream)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(index: i32, default: bool) -> MediaTrack {
        MediaTrack {
            index,
            language: None,
            title: None,
            codec: "aac".into(),
            channels: None,
            default,
        }
    }

    #[test]
    fn play_urls() {
        let mut info = PlayInfo {
            infohash: "a".repeat(40),
            file_index: 1,
            file_name: "f.mp4".into(),
            stream: "/api/stream/T".into(),
            direct: true,
            duration: Some(100.0),
            audio: vec![track(0, true), track(1, false)],
            subtitles: vec![],
            audio_track: Some(0),
            subtitle_track: None,
            start_at: 0,
            next: None,
            preloaded: false,
            native: false,
        };
        assert!(info.plays_directly(Some(0)));
        assert!(!info.plays_directly(Some(1)), "another dub needs re-encoding");
        assert_eq!(info.transcoded_url(Some(1), 61.7), "/api/stream/T?audio=1&start=61");
        assert_eq!(info.subtitle_url(2, 61.7), "/api/stream/T/sub/2.vtt?start=61");
        assert_eq!(info.subtitle_url(2, 0.0), "/api/stream/T/sub/2.vtt");
        info.direct = false;
        assert!(!info.plays_directly(None));
        info.native = true;
        assert!(info.plays_directly(Some(1)), "native players switch tracks directly");
        assert!(is_release_key(&format!("u{}", "0".repeat(39))));
        assert!(!is_release_key("x"));
    }
}

#[cfg(test)]
mod bitrate_tests {
    use super::*;

    #[test]
    fn estimates_and_formats() {
        // 10 GB in two hours is about 11.1 Мбит/с.
        assert_eq!(estimate_bitrate_kbps(10_000_000_000, 120.0), Some(11_111));
        assert_eq!(estimate_bitrate_kbps(0, 120.0), None);
        assert_eq!(estimate_bitrate_kbps(1_000_000, 0.0), None);
        assert_eq!(format_bitrate(11_111), "11 Мбит/с");
        assert_eq!(format_bitrate(8_400), "8.4 Мбит/с");
        assert_eq!(format_bitrate(640), "640 Кбит/с");
    }
}
