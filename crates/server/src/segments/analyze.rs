//! Local media segment detection for downloaded episode files:
//! - Embedded container chapters (`ffprobe -show_chapters`, local disk only)
//! - Cross-episode audio fingerprint matching (`ffmpeg -f chromaprint -fp_format raw`)
//! - End-credits black-frame detection (`ffmpeg blackdetect` in the final minutes)

use std::{collections::HashSet, path::Path, time::Duration};

use kino_core::{MediaSegment, SegmentKind, SegmentSource};
use serde::Deserialize;
use tokio::process::Command;

/// Seconds per Chromaprint sub-fingerprint emitted by FFmpeg's `chromaprint` muxer
/// (`1365 / 11025 Hz`).
pub const CHROMA_STEP_SEC: f64 = 1365.0 / 11025.0;
/// Warm-up delay (seconds) before FFmpeg Chromaprint emits sample index 0.
pub const CHROMA_OFFSET_SEC: f64 = 2.7;

const MIN_MATCH_SEC: f64 = 12.0;
const MAX_MATCH_SEC: f64 = 180.0;
const COARSE_STRIDE: usize = 8;
const MAX_HAMMING_COARSE: u32 = 11;
const MAX_HAMMING_FINE: u32 = 10;
const SMOOTH_RADIUS: usize = 12;
const MIN_CREDITS_BLACK_SEC: f64 = 12.0;

#[derive(Debug, Clone, Default)]
pub struct LocalProbeMeta {
    pub duration: Option<f64>,
    pub chapters: Vec<MediaSegment>,
    pub has_audio: bool,
    pub has_video: bool,
}

#[derive(Deserialize)]
struct RawChapterProbe {
    format: Option<RawFormat>,
    #[serde(default)]
    streams: Vec<RawStream>,
    #[serde(default)]
    chapters: Vec<RawChapter>,
}

#[derive(Deserialize)]
struct RawFormat {
    duration: Option<String>,
}

#[derive(Deserialize)]
struct RawStream {
    codec_type: Option<String>,
}

#[derive(Deserialize)]
struct RawChapter {
    start_time: Option<String>,
    end_time: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

/// Parses chapter titles and timestamps from `ffprobe -show_chapters -show_format -show_streams`.
pub fn parse_chapters_probe(json: &[u8]) -> Result<LocalProbeMeta, serde_json::Error> {
    let raw: RawChapterProbe = serde_json::from_slice(json)?;
    let duration = raw
        .format
        .and_then(|f| f.duration)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0);
    let has_audio = raw.streams.iter().any(|s| s.codec_type.as_deref() == Some("audio"));
    let has_video = raw.streams.iter().any(|s| s.codec_type.as_deref() == Some("video"));

    let mut chapters = Vec::<MediaSegment>::new();
    for ch in raw.chapters {
        let Some(start) = ch.start_time.as_deref().and_then(|s| s.trim().parse::<f64>().ok()) else {
            continue;
        };
        let Some(end) = ch.end_time.as_deref().and_then(|s| s.trim().parse::<f64>().ok()) else {
            continue;
        };
        if !start.is_finite() || !end.is_finite() || end <= start + 5.0 {
            continue;
        }
        let title = ch
            .tags
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("title"))
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        let Some(kind) = classify_chapter_title(title) else {
            continue;
        };
        if let Some(dur) = duration {
            if start >= dur - 2.0 {
                continue;
            }
            if kind == SegmentKind::Intro && (start > dur * 0.60 || end - start > 240.0) {
                continue;
            }
            if kind == SegmentKind::Outro && start < dur * 0.45 {
                continue;
            }
        }
        let seg = MediaSegment {
            kind,
            start_sec: start.max(0.0),
            end_sec: duration.map_or(end, |d| end.min(d)),
            source: SegmentSource::Chapters,
            confidence: 0.95,
        };
        if !chapters.iter().any(|c| c.kind == kind) {
            chapters.push(seg);
        }
    }
    Ok(LocalProbeMeta {
        duration,
        chapters,
        has_audio,
        has_video,
    })
}

fn classify_chapter_title(raw: &str) -> Option<SegmentKind> {
    let t = raw.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    if t.contains("post-credit") || t.contains("post credit") || t.contains("after credit") || t.contains("после титр")
    {
        return Some(SegmentKind::PostCredits);
    }
    if t.contains("recap") || t.contains("previously") || t.contains("повтор") || t.contains("ранее") {
        return Some(SegmentKind::Recap);
    }
    if t.contains("outro")
        || t.contains("credit")
        || t.contains("ending")
        || t == "ed"
        || t.starts_with("ed ")
        || t.ends_with(" ed")
        || t.contains("титр")
        || t.contains("эндинг")
        || t.contains("концов")
    {
        return Some(SegmentKind::Outro);
    }
    if t.contains("intro")
        || t.contains("opening")
        || t == "op"
        || t.starts_with("op ")
        || t.ends_with(" op")
        || t.contains("main title")
        || t.contains("theme song")
        || t.contains("заставк")
        || t.contains("опенинг")
    {
        return Some(SegmentKind::Intro);
    }
    None
}

/// Probes duration, stream presence and chapters on a local disk file (never HTTP).
pub async fn probe_local_file(ffprobe: &str, path: &Path) -> anyhow::Result<LocalProbeMeta> {
    let args = [
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
        "-show_chapters",
    ];
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        Command::new(ffprobe).args(args).arg(path).kill_on_drop(true).output(),
    )
    .await??;
    if !out.status.success() {
        anyhow::bail!(
            "ffprobe failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_chapters_probe(&out.stdout)?)
}

/// Decodes `ffmpeg -f chromaprint -fp_format raw` bytes into `u32` sub-fingerprints.
pub fn parse_raw_chromaprint(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&c| u32::from_le_bytes(c))
        .collect()
}

/// Extracts raw Chromaprint sub-fingerprints for `[seek_sec, seek_sec + len_sec]` of audio track 0.
pub async fn extract_chromaprint_window(
    ffmpeg: &str,
    path: &Path,
    seek_sec: f64,
    len_sec: f64,
) -> anyhow::Result<Vec<u32>> {
    let ss = format!("{:.3}", seek_sec.max(0.0));
    let dur = format!("{:.3}", len_sec.max(5.0));
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-threads", "1"]);
    if seek_sec > 0.0 {
        cmd.args(["-ss", &ss]);
    }
    cmd.arg("-i")
        .arg(path)
        .args([
            "-t",
            &dur,
            "-vn",
            "-sn",
            "-dn",
            "-map",
            "0:a:0?",
            "-ac",
            "1",
            "-ar",
            "11025",
            "-f",
            "chromaprint",
            "-fp_format",
            "raw",
            "pipe:1",
        ])
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(30), cmd.output()).await??;
    if !out.status.success() {
        anyhow::bail!(
            "chromaprint failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_raw_chromaprint(&out.stdout))
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchedBlock {
    pub a_start_sec: f64,
    pub a_end_sec: f64,
    pub b_start_sec: f64,
    pub b_end_sec: f64,
    pub similarity: f32,
}

fn non_zero_distinct_ok(slice: &[u32]) -> bool {
    let mut set = HashSet::<u32>::new();
    let mut non_zero = 0usize;
    for &x in slice {
        if x != 0 {
            non_zero += 1;
            set.insert(x);
        }
    }
    non_zero >= slice.len().saturating_mul(7) / 10 && set.len() >= 12.max(non_zero / 8)
}

fn evaluate_lag(a: &[u32], b: &[u32], lag: isize, min_items: usize, max_items: usize) -> Option<(usize, usize, f32)> {
    let ov_start = 0isize.max(-lag) as usize;
    let ov_end = (a.len() as isize).min(b.len() as isize - lag);
    if ov_end <= ov_start as isize {
        return None;
    }
    let ov_end = ov_end as usize;
    let ov_len = ov_end - ov_start;
    if ov_len < min_items {
        return None;
    }

    let b_start = (ov_start as isize + lag) as usize;
    let mut dists = Vec::with_capacity(ov_len);
    let mut active = Vec::with_capacity(ov_len);
    for (&va, &vb) in a[ov_start..ov_end].iter().zip(&b[b_start..b_start + ov_len]) {
        if va != 0 && vb != 0 {
            dists.push((va ^ vb).count_ones());
            active.push(1usize);
        } else {
            dists.push(16);
            active.push(0usize);
        }
    }

    let mut pref_dist = Vec::with_capacity(ov_len + 1);
    let mut pref_act = Vec::with_capacity(ov_len + 1);
    pref_dist.push(0u32);
    pref_act.push(0usize);
    let (mut sum_d, mut sum_a) = (0u32, 0usize);
    for (&d, &act) in dists.iter().zip(&active) {
        sum_d += d;
        sum_a += act;
        pref_dist.push(sum_d);
        pref_act.push(sum_a);
    }

    let mut best_run: Option<(usize, usize)> = None;
    let mut cur_start: Option<usize> = None;
    for k in 0..ov_len {
        let lo = k.saturating_sub(SMOOTH_RADIUS);
        let hi = (k + SMOOTH_RADIUS + 1).min(ov_len);
        let win = hi - lo;
        let act_cnt = pref_act[hi] - pref_act[lo];
        let d_sum = pref_dist[hi] - pref_dist[lo];
        let ok = act_cnt * 10 >= win * 7 && (d_sum as f64) <= (win as f64) * 10.5;
        if ok {
            cur_start.get_or_insert(k);
        } else if let Some(s) = cur_start.take() {
            if k - s >= min_items && best_run.is_none_or(|(bs, be)| k - s > be - bs) {
                best_run = Some((s, k));
            }
        }
    }
    if let Some(s) = cur_start {
        if ov_len - s >= min_items && best_run.is_none_or(|(bs, be)| ov_len - s > be - bs) {
            best_run = Some((s, ov_len));
        }
    }
    let (mut s, mut e) = best_run?;

    while s < e && (active[s] == 0 || dists[s] > MAX_HAMMING_FINE) {
        s += 1;
    }
    while e > s && (active[e - 1] == 0 || dists[e - 1] > MAX_HAMMING_FINE) {
        e -= 1;
    }
    let len = e.saturating_sub(s);
    if len < min_items || len > max_items {
        return None;
    }
    let a_s = ov_start + s;
    let a_e = ov_start + e;
    if !non_zero_distinct_ok(&a[a_s..a_e]) {
        return None;
    }
    let mean_dist = (pref_dist[e] - pref_dist[s]) as f64 / (len as f64);
    if mean_dist > 10.5 {
        return None;
    }
    let sim = (1.0 - mean_dist / 16.0).clamp(0.0, 1.0) as f32;
    Some((a_s, a_e, sim))
}

fn index_to_time(window_start: f64, idx: usize, is_end: bool) -> f64 {
    if !is_end && idx <= 2 && window_start <= 0.5 {
        return 0.0;
    }
    let offset = if is_end { CHROMA_OFFSET_SEC } else { 0.0 };
    (window_start + offset + (idx as f64) * CHROMA_STEP_SEC).max(0.0)
}

/// Finds the longest matching audio block between two Chromaprint streams (`a` and `b`),
/// returning timestamp ranges inside both streams.
pub fn find_matching_span(
    a: &[u32],
    b: &[u32],
    a_window_start: f64,
    b_window_start: f64,
    min_dur_sec: f64,
    max_dur_sec: f64,
) -> Option<MatchedBlock> {
    let min_items = ((min_dur_sec.max(MIN_MATCH_SEC)) / CHROMA_STEP_SEC).ceil() as usize;
    let max_items = ((max_dur_sec.min(MAX_MATCH_SEC)) / CHROMA_STEP_SEC).floor() as usize;
    if a.len() < min_items || b.len() < min_items || max_items < min_items {
        return None;
    }
    let min_coarse_hits = (min_items / COARSE_STRIDE).max(6);
    let min_lag = -(b.len() as isize) + min_items as isize;
    let max_lag = a.len() as isize - min_items as isize;

    let mut candidate_lags = Vec::<(isize, usize)>::new();
    for lag in min_lag..=max_lag {
        let ov_start = 0isize.max(-lag) as usize;
        let ov_end = ((a.len() as isize).min(b.len() as isize - lag)) as usize;
        let mut best_hits = 0usize;
        let mut cur_hits = 0usize;
        let mut misses = 0usize;
        let mut i = ov_start;
        while i < ov_end {
            let j = (i as isize + lag) as usize;
            let va = a[i];
            let vb = b[j];
            if va != 0 && vb != 0 && (va ^ vb).count_ones() <= MAX_HAMMING_COARSE {
                cur_hits += 1;
                misses = 0;
                if cur_hits > best_hits {
                    best_hits = cur_hits;
                }
            } else if cur_hits > 0 {
                misses += 1;
                if misses > 1 {
                    cur_hits = 0;
                    misses = 0;
                }
            }
            i += COARSE_STRIDE;
        }
        if best_hits >= min_coarse_hits {
            candidate_lags.push((lag, best_hits));
        }
    }
    candidate_lags.sort_by_key(|x| std::cmp::Reverse(x.1));

    let mut chosen = Vec::<isize>::new();
    for (lag, _) in candidate_lags {
        if !chosen.iter().any(|&c| (c - lag).abs() <= 2) {
            chosen.push(lag);
            if chosen.len() >= 6 {
                break;
            }
        }
    }

    let mut best: Option<(usize, usize, isize, f32)> = None;
    for base_lag in chosen {
        for lag in (base_lag - 2)..=(base_lag + 2) {
            if let Some((a_s, a_e, sim)) = evaluate_lag(a, b, lag, min_items, max_items) {
                let score = ((a_e - a_s) as f32) * (0.5 + sim);
                let replace = best.is_none_or(|(bs, be, _, bsim)| score > ((be - bs) as f32) * (0.5 + bsim));
                if replace {
                    best = Some((a_s, a_e, lag, sim));
                }
            }
        }
    }

    let (a_s, a_e, lag, sim) = best?;
    let b_s = (a_s as isize + lag) as usize;
    let b_e = (a_e as isize + lag) as usize;
    Some(MatchedBlock {
        a_start_sec: index_to_time(a_window_start, a_s, false),
        a_end_sec: index_to_time(a_window_start, a_e, true),
        b_start_sec: index_to_time(b_window_start, b_s, false),
        b_end_sec: index_to_time(b_window_start, b_e, true),
        similarity: sim,
    })
}

/// Parses FFmpeg `blackdetect` stderr output in `[seek_offset_sec, episode_duration]`
/// and extracts end-credits when a sustained dark interval begins in the final third.
pub fn parse_blackdetect_credits(stderr: &str, seek_offset_sec: f64, episode_duration: f64) -> Option<MediaSegment> {
    if !episode_duration.is_finite() || episode_duration < 60.0 {
        return None;
    }
    let mut intervals = Vec::<(f64, f64)>::new();
    for line in stderr.lines() {
        let Some(bs_idx) = line.find("black_start:") else {
            continue;
        };
        let rest = &line[bs_idx..];
        let mut start = None;
        let mut end = None;
        for tok in rest.split_whitespace() {
            if let Some(v) = tok.strip_prefix("black_start:") {
                start = v.parse::<f64>().ok();
            } else if let Some(v) = tok.strip_prefix("black_end:") {
                end = v.parse::<f64>().ok();
            }
        }
        if let (Some(s), Some(e)) = (start, end) {
            if s.is_finite() && e.is_finite() && e > s {
                let abs_s = (seek_offset_sec + s).max(0.0);
                let abs_e = (seek_offset_sec + e).min(episode_duration);
                if abs_e > abs_s {
                    if let Some((_, prev_e)) = intervals.last_mut() {
                        // Bridge brief studio cards (< 4s) between consecutive credit rolls.
                        if abs_s - *prev_e <= 4.0 {
                            *prev_e = (*prev_e).max(abs_e);
                            continue;
                        }
                    }
                    intervals.push((abs_s, abs_e));
                }
            }
        }
    }

    let min_start = episode_duration * 0.65;
    let best = intervals
        .into_iter()
        .filter(|&(s, e)| {
            (e - s) >= MIN_CREDITS_BLACK_SEC
                && s >= min_start
                && s < episode_duration - 10.0
                && e >= episode_duration - 60.0
        })
        .max_by(|(s1, e1), (s2, e2)| (e1 - s1).total_cmp(&(e2 - s2)))?;

    Some(MediaSegment {
        kind: SegmentKind::Outro,
        start_sec: best.0,
        end_sec: best.1,
        source: SegmentSource::LocalAnalysis,
        confidence: 0.72,
    })
}

/// Runs fast FFmpeg `blackdetect` on the tail of a local video file to detect black-background credits.
pub async fn detect_black_credits(
    ffmpeg: &str,
    path: &Path,
    episode_duration: f64,
    large_file: bool,
) -> anyhow::Result<Option<MediaSegment>> {
    let tail_len = (episode_duration * 0.30).min(210.0);
    if tail_len < 20.0 {
        return Ok(None);
    }
    let ss = (episode_duration - tail_len).max(0.0);
    let ss_str = format!("{ss:.3}");
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "info", "-threads", "1", "-ss", &ss_str]);
    if large_file {
        cmd.args(["-skip_frame", "nokey"]);
    }
    cmd.arg("-i")
        .arg(path)
        .args([
            "-an",
            "-sn",
            "-dn",
            "-vf",
            "scale=160:-2,fps=2,blackdetect=d=0.4:pix_th=0.08:pic_th=0.88",
            "-f",
            "null",
            "-",
        ])
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(45), cmd.output()).await??;
    if !out.status.success() {
        anyhow::bail!("blackdetect failed ({})", out.status);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Ok(parse_blackdetect_credits(&stderr, ss, episode_duration))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pseudo_fp(seed: u32, len: usize) -> Vec<u32> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x
            })
            .collect()
    }

    #[test]
    fn matches_repeated_intro_across_two_episodes_with_noise_and_silence() {
        // Silence (zeros) at start of both episodes must not match.
        let mut ep1 = vec![0u32; 160];
        ep1.extend(pseudo_fp(1111, 200)); // ~25s cold open
        let intro = pseudo_fp(99999, 250); // ~31s intro
        ep1.extend_from_slice(&intro);
        ep1.extend(pseudo_fp(2222, 600));

        let mut ep2 = vec![0u32; 160];
        ep2.extend(pseudo_fp(3333, 420)); // ~52s different cold open
                                          // Simulate light encoding bit flips (3 bits per frame).
        let noisy_intro: Vec<u32> = intro.iter().map(|&v| v ^ 0b1010_0001).collect();
        ep2.extend(noisy_intro);
        ep2.extend(pseudo_fp(4444, 400));

        let hit = find_matching_span(&ep1, &ep2, 0.0, 0.0, 12.0, 160.0).expect("should find intro");
        let dur_a = hit.a_end_sec - hit.a_start_sec;
        let dur_b = hit.b_end_sec - hit.b_start_sec;
        assert!((28.0..=36.0).contains(&dur_a), "dur_a={dur_a}");
        assert!((28.0..=36.0).contains(&dur_b), "dur_b={dur_b}");
        assert!(
            hit.a_start_sec >= 40.0 && hit.a_start_sec <= 48.0,
            "a_start={}",
            hit.a_start_sec
        );
        assert!(
            hit.b_start_sec >= 68.0 && hit.b_start_sec <= 76.0,
            "b_start={}",
            hit.b_start_sec
        );
    }

    #[test]
    fn rejects_silence_only_or_unrelated_episodes() {
        let a = vec![0u32; 500];
        let b = vec![0u32; 500];
        assert!(find_matching_span(&a, &b, 0.0, 0.0, 12.0, 160.0).is_none());

        let c = pseudo_fp(100, 600);
        let d = pseudo_fp(200, 600);
        assert!(find_matching_span(&c, &d, 0.0, 0.0, 12.0, 160.0).is_none());
    }

    #[test]
    fn parses_chapters_and_blackdetect_credits() {
        let json = br#"{
            "format": {"duration": "3510.0"},
            "streams": [{"codec_type": "video"}, {"codec_type": "audio"}],
            "chapters": [
                {"start_time": "10.0", "end_time": "65.0", "tags": {"title": "Intro"}},
                {"start_time": "3428.0", "end_time": "3510.0", "tags": {"title": "End Credits"}}
            ]
        }"#;
        let meta = parse_chapters_probe(json).unwrap();
        assert_eq!(meta.duration, Some(3510.0));
        assert!(meta.has_audio && meta.has_video);
        assert_eq!(meta.chapters.len(), 2);
        assert_eq!(meta.chapters[0].kind, SegmentKind::Intro);
        assert_eq!(meta.chapters[1].kind, SegmentKind::Outro);

        let stderr = "\
[Parsed_blackdetect_2 @ 0x1] black_start:12.0 black_end:14.0 black_duration:2.0\n\
[Parsed_blackdetect_2 @ 0x1] black_start:128.0 black_end:152.0 black_duration:24.0\n\
[Parsed_blackdetect_2 @ 0x1] black_start:155.0 black_end:209.5 black_duration:54.5\n";
        let outro = parse_blackdetect_credits(stderr, 3300.0, 3510.0).expect("should detect credits");
        assert_eq!(outro.kind, SegmentKind::Outro);
        assert_eq!(outro.start_sec, 3428.0);
        assert_eq!(outro.end_sec, 3509.5);
    }
}
