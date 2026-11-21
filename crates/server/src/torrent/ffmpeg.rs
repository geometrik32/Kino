//! ffprobe/ffmpeg: what is inside a video file, and turning it into something
//! a browser can play (H.264 + AAC in fragmented MP4) on the fly.

use std::process::Stdio;

use kino_core::MediaTrack;
use serde::Deserialize;
use tokio::process::{Child, Command};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Probe {
    pub duration: Option<f64>,
    /// ffprobe format names, e.g. "matroska,webm" or "mov,mp4,m4a,3gp,3g2,mj2".
    pub container: String,
    pub video_codec: Option<String>,
    pub audio: Vec<MediaTrack>,
    pub subtitles: Vec<MediaTrack>,
}

impl Probe {
    /// Whether a browser plays the file without our help, with the default audio.
    pub fn browser_ok(&self) -> bool {
        let container = self.container.starts_with("mov,mp4");
        let video = matches!(self.video_codec.as_deref(), Some("h264" | "vp9" | "av1"));
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
        self.video_codec.as_deref() == Some("h264")
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
    channels: Option<i32>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
    #[serde(default)]
    disposition: std::collections::HashMap<String, i32>,
}

/// Subtitle formats that can become WebVTT (not pictures like PGS).
const TEXT_SUBS: &[&str] = &["subrip", "ass", "ssa", "webvtt", "mov_text", "text"];

pub fn parse_probe(json: &[u8]) -> Result<Probe, serde_json::Error> {
    let raw: Raw = serde_json::from_slice(json)?;
    let mut p = Probe {
        duration: raw.format.as_ref().and_then(|f| f.duration.as_deref()?.parse().ok()),
        container: raw.format.and_then(|f| f.format_name).unwrap_or_default(),
        ..Probe::default()
    };
    let (mut a, mut s) = (0, 0);
    for st in raw.streams {
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
                p.video_codec = Some(codec.clone())
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

/// Runs ffprobe on a file or URL.
pub async fn probe(ffprobe: &str, input: &str) -> anyhow::Result<Probe> {
    let mut args = vec!["-v", "quiet", "-print_format", "json", "-show_streams", "-show_format"];
    if is_http(input) {
        args.extend(["-rw_timeout", "30000000"]);
    }
    args.push(input);
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        Command::new(ffprobe).args(args).kill_on_drop(true).output(),
    )
    .await??;
    anyhow::ensure!(out.status.success(), "ffprobe failed");
    Ok(parse_probe(&out.stdout)?)
}

/// Hardware for encoding video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    if start > 0 {
        a.extend(["-ss".into(), start.to_string()]);
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
                a.extend(["-vf", "scale_vaapi=format=nv12", "-c:v", "h264_vaapi", "-qp", "23"].map(String::from))
            }
            HwAccel::None => a.extend(
                [
                    "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p",
                ]
                .map(String::from),
            ),
        }
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
            "pipe:1",
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
        .stderr(Stdio::null())
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
        assert!(a.contains("-ss 600 -i http://ts/stream"));
        assert!(a.contains("-map 0:a:1?"));
        assert!(a.contains("h264_qsv"));
        assert!(a.contains("-dn"));
        let a = transcode_args("f.mkv", 0, 0, true, HwAccel::Qsv).join(" ");
        assert!(!a.contains("rw_timeout") && !a.contains("hwaccel") && !a.contains("-ss") && a.contains("-c:v copy"));
        assert_eq!(subtitle_args("f.mkv", 2, 0)[6], "0:s:2");
        let s = subtitle_args("http://ts/stream", 0, 90).join(" ");
        assert!(s.contains("-rw_timeout 30000000") && s.contains("-ss 90 -i http://ts/stream"));
        assert!(s.contains("-vn -an -dn"));
    }
}
