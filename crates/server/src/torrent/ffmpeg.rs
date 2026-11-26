//! ffprobe/ffmpeg: what is inside a video file, and turning it into something
//! a browser can play (H.264 + AAC in fragmented MP4) on the fly.

use std::process::Stdio;

use kino_core::MediaTrack;
use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Probe {
    pub duration: Option<f64>,
    /// ffprobe format names, e.g. "matroska,webm" or "mov,mp4,m4a,3gp,3g2,mj2".
    pub container: String,
    pub video_codec: Option<String>,
    pub video_pix_fmt: Option<String>,
    pub audio: Vec<MediaTrack>,
    pub subtitles: Vec<MediaTrack>,
}

impl Probe {
    /// Whether the video stream is standard 8-bit 4:2:0 that browsers can decode.
    fn video_is_8bit(&self) -> bool {
        match self.video_pix_fmt.as_deref() {
            None => true,
            Some("yuv420p" | "yuvj420p" | "nv12") => true,
            Some(_) => false,
        }
    }

    /// Whether a browser plays the file without our help, with the default audio.
    pub fn browser_ok(&self) -> bool {
        let container = self.container.starts_with("mov,mp4");
        let video = matches!(self.video_codec.as_deref(), Some("h264" | "vp9" | "av1")) && self.video_is_8bit();
        let audio = self
            .audio
            .iter()
            .find(|a| a.default)
            .or(self.audio.first())
            .is_none_or(|a| matches!(a.codec.as_str(), "aac" | "mp3" | "opus" | "vorbis"));
        container && video && audio
    }

    /// Whether the video stream can be copied as it is when re-packing.
    pub fn video_copyable(&self) -> bool {
        self.video_codec.as_deref() == Some("h264") && self.video_is_8bit()
    }
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    streams: Vec<RawStream>,
    format: Option<RawFormat>,
}

#[derive(Deserialize)]
struct RawFormat {
    format_name: Option<String>,
    duration: Option<String>,
}

#[derive(Deserialize)]
struct RawStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    pix_fmt: Option<String>,
    channels: Option<i32>,
    duration: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
    #[serde(default)]
    disposition: std::collections::HashMap<String, i32>,
}

/// Parses either plain seconds (`"2820.5"`) or `HH:MM:SS(.frac)` (Matroska `DURATION` tag).
fn parse_duration_str(raw: &str) -> Option<f64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(sec) = s.parse::<f64>() {
        return (sec.is_finite() && sec > 0.0).then_some(sec);
    }
    let mut parts = s.rsplit(':');
    let sec: f64 = parts.next()?.parse().ok()?;
    let min: f64 = parts.next()?.parse().ok()?;
    let hrs: f64 = match parts.next() {
        Some(h) => h.parse().ok()?,
        None => 0.0,
    };
    if parts.next().is_some() {
        return None;
    }
    let total = hrs * 3600.0 + min * 60.0 + sec;
    (total.is_finite() && total > 0.0).then_some(total)
}

/// Subtitle formats that can become WebVTT (not pictures like PGS).
const TEXT_SUBS: &[&str] = &["subrip", "ass", "ssa", "webvtt", "mov_text", "text"];

pub fn parse_probe(json: &[u8]) -> Result<Probe, serde_json::Error> {
    let raw: Raw = serde_json::from_slice(json)?;
    let mut p = Probe {
        duration: raw
            .format
            .as_ref()
            .and_then(|f| f.duration.as_deref().and_then(parse_duration_str)),
        container: raw.format.and_then(|f| f.format_name).unwrap_or_default(),
        ..Probe::default()
    };
    let (mut a, mut s) = (0, 0);
    for st in raw.streams {
        if p.duration.is_none() {
            p.duration = st.duration.as_deref().and_then(parse_duration_str).or_else(|| {
                st.tags
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("duration"))
                    .and_then(|(_, v)| parse_duration_str(v))
            });
        }
        let codec = st.codec_name.unwrap_or_default();
        let track = |index| MediaTrack {
            index,
            language: st.tags.get("language").filter(|l| l.as_str() != "und").cloned(),
            title: st.tags.get("title").cloned(),
            codec: codec.clone(),
            channels: st.channels,
            default: st.disposition.get("default") == Some(&1),
        };
        match st.codec_type.as_deref() {
            Some("video") if p.video_codec.is_none() && st.disposition.get("attached_pic") != Some(&1) => {
                p.video_codec = Some(codec.clone());
                p.video_pix_fmt = st.pix_fmt.clone();
            }
            Some("audio") => {
                p.audio.push(track(a));
                a += 1;
            }
            // Picture subtitles are skipped but still counted: ffmpeg's `0:s:N` counts them.
            Some("subtitle") => {
                if TEXT_SUBS.contains(&codec.as_str()) {
                    p.subtitles.push(track(s));
                }
                s += 1;
            }
            _ => {}
        }
    }
    Ok(p)
}

fn is_http(input: &str) -> bool {
    input.starts_with("http://") || input.starts_with("https://")
}

/// Runs ffprobe on a file or URL with the default 50-second timeout.
pub async fn probe(ffprobe: &str, input: &str) -> anyhow::Result<Probe> {
    probe_with_timeout(ffprobe, input, std::time::Duration::from_secs(50)).await
}

/// Runs ffprobe on a file or URL with an explicit timeout.
pub async fn probe_with_timeout(ffprobe: &str, input: &str, timeout: std::time::Duration) -> anyhow::Result<Probe> {
    let rw_timeout = timeout
        .saturating_sub(std::time::Duration::from_secs(5))
        .max(std::time::Duration::from_secs(5))
        .as_micros()
        .to_string();
    let mut args = vec!["-v", "error", "-print_format", "json", "-show_streams", "-show_format"];
    if is_http(input) {
        args.extend(["-rw_timeout", rw_timeout.as_str()]);
    }
    args.push(input);
    let out = tokio::time::timeout(timeout, Command::new(ffprobe).args(args).kill_on_drop(true).output()).await??;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail = stderr.trim();
        anyhow::bail!(
            "ffprobe failed ({}): {}",
            out.status,
            tail.chars().take(240).collect::<String>()
        );
    }
    Ok(parse_probe(&out.stdout)?)
}

/// Hardware for encoding video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HwAccel {
    None,
    /// Intel Quick Sync (needs `/dev/dri` in the container).
    Qsv,
    Vaapi,
}

impl HwAccel {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "qsv" => HwAccel::Qsv,
            "vaapi" => HwAccel::Vaapi,
            _ => HwAccel::None,
        }
    }
}

/// ffmpeg arguments for a browser-friendly fragmented MP4 to stdout.
pub fn transcode_args(input: &str, start: u64, audio: i32, copy_video: bool, hw: HwAccel) -> Vec<String> {
    let seek = (start > 0).then(|| start.to_string());
    transcode_args_seek(input, seek.as_deref(), audio, copy_video, hw)
}

/// ffmpeg arguments seeking to an arbitrary timestamp string (`"120"` or `"120.500000"`).
pub fn transcode_args_seek(
    input: &str,
    seek_ss: Option<&str>,
    audio: i32,
    copy_video: bool,
    hw: HwAccel,
) -> Vec<String> {
    let mut a = transcode_args_base(input, seek_ss, audio, copy_video, hw);
    a.push("pipe:1".into());
    a
}

/// ffmpeg arguments to pre-transcode a bounded `duration_secs` fMP4 prefix onto disk.
pub fn transcode_prefix_to_file_args(
    input: &str,
    start: u64,
    audio: i32,
    copy_video: bool,
    hw: HwAccel,
    duration_secs: u64,
    out_path: &str,
) -> Vec<String> {
    let seek = (start > 0).then(|| start.to_string());
    let mut a = transcode_args_base(input, seek.as_deref(), audio, copy_video, hw);
    a.extend(["-t".into(), duration_secs.to_string(), "-y".into(), out_path.into()]);
    a
}

/// Fast single-frame copy probe outputting the exact snapped source start timestamp in milliseconds.
pub fn probe_start_pts_args(input: &str, start: u64) -> Vec<String> {
    let mut a: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "error".into()];
    if is_http(input) {
        a.extend(["-rw_timeout".into(), "10000000".into()]);
    }
    if start > 0 {
        a.extend(["-noaccurate_seek".into(), "-ss".into(), start.to_string()]);
    }
    a.extend(
        [
            "-copyts",
            "-i",
            input,
            "-map",
            "0:v:0",
            "-c:v",
            "copy",
            "-frames:v",
            "1",
            "-f",
            "mkvtimestamp_v2",
            "pipe:1",
        ]
        .map(String::from),
    );
    a
}

fn transcode_args_base(input: &str, seek_ss: Option<&str>, audio: i32, copy_video: bool, hw: HwAccel) -> Vec<String> {
    let mut a: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "error".into()];
    if is_http(input) {
        a.extend(["-rw_timeout".into(), "30000000".into()]);
    }
    if !copy_video {
        match hw {
            HwAccel::Qsv => a.extend(["-hwaccel", "qsv", "-hwaccel_output_format", "qsv"].map(String::from)),
            HwAccel::Vaapi => a.extend(
                [
                    "-hwaccel",
                    "vaapi",
                    "-hwaccel_device",
                    "/dev/dri/renderD128",
                    "-hwaccel_output_format",
                    "vaapi",
                ]
                .map(String::from),
            ),
            HwAccel::None => {}
        }
    }
    if let Some(ss) = seek_ss {
        if copy_video {
            // Keep audio in sync with copied video when `-ss` snaps back to a preceding keyframe.
            a.push("-noaccurate_seek".into());
        }
        a.extend(["-ss".into(), ss.to_owned()]);
    }
    a.extend(["-i".into(), input.into()]);
    a.extend(["-map".into(), "0:v:0".into(), "-map".into(), format!("0:a:{audio}?")]);
    if copy_video {
        a.extend(["-c:v", "copy"].map(String::from));
    } else {
        match hw {
            HwAccel::Qsv => {
                a.extend(["-c:v", "h264_qsv", "-preset", "veryfast", "-global_quality", "23"].map(String::from))
            }
            HwAccel::Vaapi => {
                // Works both for hardware-decoded VAAPI surfaces and software-decoded fallback (AV1, MPEG4, 10-bit H.264).
                a.extend(
                    [
                        "-vf",
                        "format=nv12|vaapi,hwupload,scale_vaapi=format=nv12",
                        "-c:v",
                        "h264_vaapi",
                        "-qp",
                        "23",
                    ]
                    .map(String::from),
                )
            }
            HwAccel::None => a.extend(
                [
                    "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p",
                ]
                .map(String::from),
            ),
        }
        a.extend(["-g", "48"].map(String::from));
    }
    a.extend(
        [
            "-c:a",
            "aac",
            "-ac",
            "2",
            "-b:a",
            "192k",
            "-sn",
            "-dn",
            "-f",
            "mp4",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof",
        ]
        .map(String::from),
    );
    a
}

/// ffmpeg arguments that extract a subtitle track as WebVTT to stdout. With
/// `start`, times count from that second, to match a re-encoded stream.
pub fn subtitle_args(input: &str, index: i32, start: u64) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error"].map(String::from).to_vec();
    if is_http(input) {
        a.extend(["-rw_timeout".into(), "30000000".into()]);
    }
    if start > 0 {
        a.extend(["-ss".into(), start.to_string()]);
    }
    a.extend(
        [
            "-i",
            input,
            "-map",
            &format!("0:s:{index}"),
            "-vn",
            "-an",
            "-dn",
            "-f",
            "webvtt",
            "pipe:1",
        ]
        .map(String::from),
    );
    a
}

/// Starts ffmpeg with its output on a pipe. It is killed when the child is dropped.
pub fn spawn(ffmpeg: &str, args: &[String]) -> std::io::Result<Child> {
    Command::new(ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_probe() {
        let p = parse_probe(include_bytes!("../../tests/fixtures/ffprobe.json")).unwrap();
        assert_eq!(p.duration, Some(2820.5));
        assert_eq!(p.video_codec.as_deref(), Some("hevc"));
        assert_eq!(p.audio.len(), 2);
        assert_eq!(p.audio[0].title.as_deref(), Some("LostFilm"));
        assert_eq!(p.audio[0].label(), "LostFilm · Русский · 5.1");
        assert!(p.audio[0].default);
        assert_eq!(p.audio[1].language.as_deref(), Some("eng"));
        // The PGS track is skipped but the SRT keeps ffmpeg's number.
        assert_eq!(p.subtitles.len(), 1);
        assert_eq!(p.subtitles[0].index, 1);
        assert!(!p.browser_ok());
        assert!(!p.video_copyable());
    }

    #[test]
    fn args() {
        let a = transcode_args("http://ts/stream", 600, 1, false, HwAccel::Qsv).join(" ");
        assert!(a.contains("-rw_timeout 30000000"));
        assert!(a.contains("-hwaccel qsv"));
        assert!(!a.contains("-noaccurate_seek"));
        assert!(a.contains("-ss 600 -i http://ts/stream"));
        assert!(a.contains("-map 0:a:1?"));
        assert!(a.contains("h264_qsv"));
        assert!(a.contains("-dn"));
        let a = transcode_args("f.mkv", 600, 0, true, HwAccel::Vaapi).join(" ");
        assert!(a.contains("-noaccurate_seek -ss 600 -i f.mkv"));
        assert!(a.contains("-c:v copy"));
        let a = transcode_args("f.mkv", 0, 0, true, HwAccel::Qsv).join(" ");
        assert!(!a.contains("rw_timeout") && !a.contains("hwaccel") && !a.contains("-ss") && a.contains("-c:v copy"));
        assert_eq!(subtitle_args("f.mkv", 2, 0)[6], "0:s:2");
        let s = subtitle_args("http://ts/stream", 0, 90).join(" ");
        assert!(s.contains("-rw_timeout 30000000") && s.contains("-ss 90 -i http://ts/stream"));
        assert!(s.contains("-vn -an -dn"));

        let f = transcode_prefix_to_file_args("http://127.0.0.1/s", 120, 1, true, HwAccel::None, 30, "/tmp/r.mp4")
            .join(" ");
        assert!(f.contains("-noaccurate_seek -ss 120 -i http://127.0.0.1/s"));
        assert!(f.contains("-t 30 -y /tmp/r.mp4"));

        let p = probe_start_pts_args("http://127.0.0.1/s", 120).join(" ");
        assert!(p.contains("-noaccurate_seek -ss 120 -copyts -i http://127.0.0.1/s"));
        assert!(p.contains("-f mkvtimestamp_v2 pipe:1"));
    }

    #[test]
    fn duration_fallback_from_stream_tags() {
        let json = br#"{
            "format": { "format_name": "matroska,webm" },
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "hevc",
                    "tags": { "DURATION": "00:42:15.500000000" }
                }
            ]
        }"#;
        let p = parse_probe(json).unwrap();
        assert_eq!(p.duration, Some(42.0 * 60.0 + 15.5));
    }
}
