//! Intro / recap / end-credits segment storage, open-source lookups (IntroDB & AniSkip)
//! and background analysis of preloaded episodes on disk.

pub mod analyze;

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use kino_core::{MediaKind, MediaSegment, SegmentKind, SegmentSource};
use serde::Deserialize;
use sqlx::{PgPool, Row};

use crate::{api::AppState, torrent::files};

const OPEN_LOOKUP_TTL_DAYS: i32 = 7;
const OPEN_FETCH_TIMEOUT: Duration = Duration::from_secs(6);
const HEAD_SCAN_MAX_SEC: f64 = 600.0;
const TAIL_SCAN_MAX_SEC: f64 = 240.0;

fn merge_priority(s: &MediaSegment, exact_release: bool) -> (u8, f32) {
    let tier = match (exact_release, s.source) {
        (true, SegmentSource::Chapters) => 6,
        (true, SegmentSource::LocalAnalysis) => 5,
        (_, SegmentSource::IntroDb) => 4,
        (_, SegmentSource::AniSkip) => 3,
        (false, SegmentSource::Chapters) => 2,
        (false, SegmentSource::LocalAnalysis) => 1,
    };
    (tier, s.confidence)
}

fn valid_for_duration(s: &MediaSegment, duration: Option<f64>) -> bool {
    if !s.start_sec.is_finite() || !s.end_sec.is_finite() || s.end_sec <= s.start_sec + 2.0 || s.start_sec < 0.0 {
        return false;
    }
    match duration {
        Some(d) if d.is_finite() && d > 10.0 => {
            s.start_sec < d - 1.5 && (s.kind != SegmentKind::Outro || s.start_sec >= d * 0.45)
        }
        _ => true,
    }
}

/// Loads previously computed or fetched segments from PostgreSQL for playback.
/// Prefers segments specific to `infohash`, then open-source segments (`''`), then
/// compatible local analyses from other releases matching `duration` within 20 s.
pub async fn load_segments_for_play(
    pool: &PgPool,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    infohash: &str,
    duration: Option<f64>,
) -> Vec<MediaSegment> {
    let rows = match sqlx::query(
        "SELECT release_infohash, segment_kind, start_sec, end_sec, source, confidence, episode_duration
         FROM media_segment
         WHERE media_kind = $1 AND tmdb_id = $2 AND season = $3 AND episode = $4",
    )
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(season)
    .bind(episode)
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "media_segment lookup failed");
            return Vec::new();
        }
    };

    let hash_lower = infohash.to_ascii_lowercase();
    let mut best: BTreeMap<SegmentKind, (u8, f32, MediaSegment)> = BTreeMap::new();
    for r in rows {
        let rel_hash: String = r.try_get("release_infohash").unwrap_or_default();
        let kind_raw: String = r.try_get("segment_kind").unwrap_or_default();
        let src_raw: String = r.try_get("source").unwrap_or_default();
        let Some(seg_kind) = SegmentKind::parse(&kind_raw) else {
            continue;
        };
        let Some(src) = SegmentSource::parse(&src_raw) else {
            continue;
        };
        let ep_dur: Option<f64> = r.try_get("episode_duration").ok().flatten();
        let exact_release = !hash_lower.is_empty() && rel_hash.eq_ignore_ascii_case(&hash_lower);
        let is_open = rel_hash.is_empty();
        // Reuse another rip's local analysis when durations closely align (e.g. 720p vs 1080p WEB-DL).
        if !exact_release && !is_open {
            if let (Some(d1), Some(d2)) = (duration, ep_dur) {
                if (d1 - d2).abs() > 20.0 {
                    continue;
                }
            }
        }
        let mut seg = MediaSegment {
            kind: seg_kind,
            start_sec: r.try_get("start_sec").unwrap_or(0.0),
            end_sec: r.try_get("end_sec").unwrap_or(0.0),
            source: src,
            confidence: r.try_get("confidence").unwrap_or(1.0),
        };
        if let Some(d) = duration.filter(|d| d.is_finite() && *d > 0.0) {
            seg.end_sec = seg.end_sec.min(d);
        }
        if !valid_for_duration(&seg, duration) {
            continue;
        }
        let prio = merge_priority(&seg, exact_release);
        let replace = best.get(&seg_kind).is_none_or(|(pt, pc, _)| prio > (*pt, *pc));
        if replace {
            best.insert(seg_kind, (prio.0, prio.1, seg));
        }
    }
    best.into_values().map(|(_, _, seg)| seg).collect()
}

#[allow(clippy::too_many_arguments)]
pub async fn save_segments(
    pool: &PgPool,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    infohash: &str,
    episode_duration: Option<f64>,
    segments: &[MediaSegment],
) -> Result<(), sqlx::Error> {
    let hash = infohash.to_ascii_lowercase();
    for seg in segments {
        if !valid_for_duration(seg, episode_duration) {
            continue;
        }
        sqlx::query(
            "INSERT INTO media_segment
                (media_kind, tmdb_id, season, episode, release_infohash, segment_kind, start_sec, end_sec, source, confidence, episode_duration, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now())
             ON CONFLICT (media_kind, tmdb_id, season, episode, release_infohash, segment_kind) DO UPDATE SET
                start_sec = EXCLUDED.start_sec,
                end_sec = EXCLUDED.end_sec,
                source = EXCLUDED.source,
                confidence = EXCLUDED.confidence,
                episode_duration = COALESCE(EXCLUDED.episode_duration, media_segment.episode_duration),
                updated_at = now()",
        )
        .bind(kind.as_str())
        .bind(tmdb_id)
        .bind(season)
        .bind(episode)
        .bind(&hash)
        .bind(seg.kind.as_str())
        .bind(seg.start_sec)
        .bind(seg.end_sec)
        .bind(seg.source.as_str())
        .bind(seg.confidence)
        .bind(episode_duration)
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn open_lookup_is_fresh(pool: &PgPool, kind: MediaKind, tmdb_id: i64, season: i32, episode: i32) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM media_segment_lookup
             WHERE media_kind = $1 AND tmdb_id = $2 AND season = $3 AND episode = $4
               AND checked_at > now() - make_interval(days => $5)
         )",
    )
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(season)
    .bind(episode)
    .bind(OPEN_LOOKUP_TTL_DAYS)
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}

async fn mark_open_lookup(pool: &PgPool, kind: MediaKind, tmdb_id: i64, season: i32, episode: i32, found: bool) {
    let status = if found { "found" } else { "empty" };
    let _ = sqlx::query(
        "INSERT INTO media_segment_lookup (media_kind, tmdb_id, season, episode, status, checked_at)
         VALUES ($1, $2, $3, $4, $5, now())
         ON CONFLICT (media_kind, tmdb_id, season, episode) DO UPDATE SET
           status = EXCLUDED.status,
           checked_at = now()",
    )
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(season)
    .bind(episode)
    .bind(status)
    .execute(pool)
    .await;
}

/// Returns segments from our DB; if open sources were not queried recently for this episode,
/// fetches IntroDB (and AniSkip for animated shows) once and caches the result in PostgreSQL.
pub async fn resolve_or_fetch_segments(
    state: &AppState,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    infohash: &str,
    duration: Option<f64>,
) -> Vec<MediaSegment> {
    let cached = load_segments_for_play(&state.pool, kind, tmdb_id, season, episode, infohash, duration).await;
    if open_lookup_is_fresh(&state.pool, kind, tmdb_id, season, episode).await {
        return cached;
    }

    let Ok(details) = tokio::time::timeout(OPEN_FETCH_TIMEOUT, state.catalog.details(kind, tmdb_id)).await else {
        return cached;
    };
    let Ok(details) = details else {
        return cached;
    };

    let mut fetched = Vec::new();
    if let Some(imdb_id) = details.imdb_id.as_deref().filter(|id| id.starts_with("tt")) {
        match fetch_introdb(&state.media.http, kind, imdb_id, season, episode).await {
            Ok(segs) => fetched.extend(segs),
            Err(e) => tracing::debug!(error = %e, imdb_id, season, episode, "IntroDB lookup failed"),
        }
    }

    if kind == MediaKind::Tv && !fetched.iter().any(|s| s.kind == SegmentKind::Intro) && is_animated(&details.genres) {
        match fetch_aniskip(&state.media.http, tmdb_id, season, episode, duration).await {
            Ok(segs) => {
                for s in segs {
                    if !fetched.iter().any(|x| x.kind == s.kind) {
                        fetched.push(s);
                    }
                }
            }
            Err(e) => tracing::debug!(error = %e, tmdb_id, season, episode, "AniSkip lookup failed"),
        }
    }

    let found = !fetched.is_empty();
    if found {
        if let Err(e) = save_segments(&state.pool, kind, tmdb_id, season, episode, "", duration, &fetched).await {
            tracing::warn!(error = %e, "saving fetched segments failed");
        }
    }
    mark_open_lookup(&state.pool, kind, tmdb_id, season, episode, found).await;

    if found {
        load_segments_for_play(&state.pool, kind, tmdb_id, season, episode, infohash, duration).await
    } else {
        cached
    }
}

fn is_animated(genres: &[String]) -> bool {
    genres.iter().any(|g| {
        let gl = g.to_lowercase();
        gl.contains("мульт") || gl.contains("аниме") || gl.contains("anim")
    })
}

#[derive(Deserialize)]
struct IntroDbResponse {
    intro: Option<IntroDbSeg>,
    recap: Option<IntroDbSeg>,
    outro: Option<IntroDbSeg>,
    post_credits: Option<IntroDbSeg>,
}

#[derive(Deserialize)]
struct IntroDbSeg {
    start_sec: Option<f64>,
    end_sec: Option<f64>,
    confidence: Option<f32>,
}

fn push_introdb_seg(out: &mut Vec<MediaSegment>, kind: SegmentKind, seg: Option<IntroDbSeg>) {
    let Some(seg) = seg else { return };
    let (Some(start_sec), Some(end_sec)) = (seg.start_sec, seg.end_sec) else {
        return;
    };
    if start_sec.is_finite() && end_sec.is_finite() && start_sec >= 0.0 && end_sec > start_sec + 2.0 {
        out.push(MediaSegment {
            kind,
            start_sec,
            end_sec,
            source: SegmentSource::IntroDb,
            confidence: seg.confidence.unwrap_or(0.85).clamp(0.0, 1.0),
        });
    }
}

/// Parses an IntroDB `/segments` JSON payload.
pub fn parse_introdb_json(bytes: &[u8]) -> Result<Vec<MediaSegment>, serde_json::Error> {
    let resp: IntroDbResponse = serde_json::from_slice(bytes)?;
    let mut out = Vec::new();
    push_introdb_seg(&mut out, SegmentKind::Intro, resp.intro);
    push_introdb_seg(&mut out, SegmentKind::Recap, resp.recap);
    push_introdb_seg(&mut out, SegmentKind::Outro, resp.outro);
    push_introdb_seg(&mut out, SegmentKind::PostCredits, resp.post_credits);
    Ok(out)
}

async fn fetch_introdb(
    http: &reqwest::Client,
    kind: MediaKind,
    imdb_id: &str,
    season: i32,
    episode: i32,
) -> anyhow::Result<Vec<MediaSegment>> {
    let mut req = http.get("https://api.introdb.app/segments").timeout(OPEN_FETCH_TIMEOUT);
    req = match kind {
        MediaKind::Movie => req.query(&[("imdb_id", imdb_id), ("is_movie", "true")]),
        MediaKind::Tv => req.query(&[
            ("imdb_id", imdb_id),
            ("season", &season.to_string()),
            ("episode", &episode.to_string()),
        ]),
    };
    let resp = req.send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let resp = resp.error_for_status()?;
    let bytes = resp.bytes().await?;
    Ok(parse_introdb_json(&bytes)?)
}

#[derive(Deserialize)]
struct ArmEntry {
    myanimelist: Option<i64>,
    #[serde(rename = "themoviedb-season")]
    tmdb_season: Option<i32>,
    media: Option<String>,
}

#[derive(Deserialize)]
struct AniSkipResponse {
    #[serde(default)]
    found: bool,
    #[serde(default)]
    results: Vec<AniSkipItem>,
}

#[derive(Deserialize)]
struct AniSkipItem {
    #[serde(rename = "skipType")]
    skip_type: String,
    interval: AniSkipInterval,
}

#[derive(Deserialize)]
struct AniSkipInterval {
    #[serde(rename = "startTime")]
    start_time: f64,
    #[serde(rename = "endTime")]
    end_time: f64,
}

/// Parses an AniSkip v2 JSON payload.
pub fn parse_aniskip_json(bytes: &[u8]) -> Result<Vec<MediaSegment>, serde_json::Error> {
    let resp: AniSkipResponse = serde_json::from_slice(bytes)?;
    if !resp.found {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for item in resp.results {
        let kind = match item.skip_type.as_str() {
            "op" | "mixed-op" => SegmentKind::Intro,
            "ed" | "mixed-ed" => SegmentKind::Outro,
            "recap" => SegmentKind::Recap,
            _ => continue,
        };
        let (s, e) = (item.interval.start_time, item.interval.end_time);
        if s.is_finite()
            && e.is_finite()
            && s >= 0.0
            && e > s + 2.0
            && !out.iter().any(|x: &MediaSegment| x.kind == kind)
        {
            out.push(MediaSegment {
                kind,
                start_sec: s,
                end_sec: e,
                source: SegmentSource::AniSkip,
                confidence: 0.85,
            });
        }
    }
    Ok(out)
}

async fn fetch_aniskip(
    http: &reqwest::Client,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    duration: Option<f64>,
) -> anyhow::Result<Vec<MediaSegment>> {
    let arm_resp = http
        .get("https://arm.haglund.dev/api/v2/themoviedb")
        .query(&[("id", &tmdb_id.to_string())])
        .timeout(OPEN_FETCH_TIMEOUT)
        .send()
        .await?
        .error_for_status()?;
    let entries: Vec<ArmEntry> = arm_resp.json().await?;
    let mal_id = entries
        .iter()
        .find(|e| {
            e.myanimelist.is_some()
                && e.media.as_deref().is_none_or(|m| m.eq_ignore_ascii_case("tv"))
                && e.tmdb_season == Some(season)
        })
        .or_else(|| {
            entries
                .iter()
                .find(|e| e.myanimelist.is_some() && e.tmdb_season == Some(season))
        })
        .and_then(|e| e.myanimelist);
    let Some(mal_id) = mal_id else {
        return Ok(Vec::new());
    };
    let ep_len = duration.map(|d| format!("{d:.3}")).unwrap_or_else(|| "0".to_owned());
    let resp = http
        .get(format!("https://api.aniskip.com/v2/skip-times/{mal_id}/{episode}"))
        .query(&[
            ("types", "op"),
            ("types", "ed"),
            ("types", "mixed-op"),
            ("types", "mixed-ed"),
            ("types", "recap"),
            ("episodeLength", ep_len.as_str()),
        ])
        .timeout(OPEN_FETCH_TIMEOUT)
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let bytes = resp.error_for_status()?.bytes().await?;
    Ok(parse_aniskip_json(&bytes)?)
}

#[derive(Debug, Clone)]
struct ReadyEpFile {
    season: i32,
    episode: i32,
    file_path: String,
    size_bytes: Option<i64>,
    analyzed: bool,
}

async fn resolve_local_disk_path(preload_dir: &std::path::Path, rel: &str) -> Option<PathBuf> {
    let p = std::path::Path::new(rel);
    if p.as_os_str().is_empty() || !p.components().all(|c| matches!(c, std::path::Component::Normal(_))) {
        return None;
    }
    let joined = preload_dir.join(p);
    let meta = tokio::fs::symlink_metadata(&joined).await.ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    let canon_dir = tokio::fs::canonicalize(preload_dir).await.ok()?;
    let canon_file = tokio::fs::canonicalize(&joined).await.ok()?;
    canon_file.starts_with(&canon_dir).then_some(canon_file)
}

/// Scans downloaded (`ready`) TV episodes on disk that have not been analyzed yet,
/// extracts embedded chapters, compares audio Chromaprints across episodes of the same
/// season/release for repeating intros/outros, and detects black-screen credits tails.
pub async fn analyze_ready_preloads(state: &AppState) -> anyhow::Result<usize> {
    let Some(preload_dir) = &state.media.preload_dir else {
        return Ok(0);
    };
    let rows = sqlx::query(
        "SELECT DISTINCT ON (p.infohash, pe.season, pe.episode)
                p.tmdb_id, p.infohash, pe.season, pe.episode, pe.file_path, pe.size_bytes,
                EXISTS (
                    SELECT 1 FROM media_analysis_done d
                    WHERE d.release_infohash = p.infohash AND d.season = pe.season AND d.episode = pe.episode
                ) AS analyzed
         FROM preload p
         JOIN preload_episode pe ON pe.preload_id = p.id
         WHERE p.media_kind = 'tv'
           AND p.state <> 'deleted'
           AND pe.state = 'ready'
           AND pe.file_path IS NOT NULL
         ORDER BY p.infohash, pe.season, pe.episode",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut groups: HashMap<(i64, String, i32), Vec<ReadyEpFile>> = HashMap::new();
    for r in rows {
        let infohash: String = r.try_get("infohash")?;
        let tmdb_id: i64 = r.try_get("tmdb_id")?;
        let season: i32 = r.try_get("season")?;
        let episode: i32 = r.try_get("episode")?;
        let file_path: String = r.try_get("file_path")?;
        let size_bytes: Option<i64> = r.try_get("size_bytes")?;
        let analyzed: bool = r.try_get("analyzed")?;
        if !files::is_video(&file_path) {
            continue;
        }
        groups
            .entry((tmdb_id, infohash, season))
            .or_default()
            .push(ReadyEpFile {
                season,
                episode,
                file_path,
                size_bytes,
                analyzed,
            });
    }

    let _permit = Arc::clone(&state.media.analysis_sem).acquire_owned().await?;
    let mut processed = 0usize;

    for ((tmdb_id, hash, season), mut eps) in groups {
        eps.sort_by_key(|e| e.episode);
        // Only run group processing if at least one episode needs analysis; if >=2 episodes exist
        // and any was analyzed alone earlier without peers, only unanalyzed ones trigger passes cleanly.
        let unanalyzed_cnt = eps.iter().filter(|e| !e.analyzed).count();
        if unanalyzed_cnt == 0 {
            continue;
        }

        // Resolve disk paths + local probe metadata (chapters + duration).
        struct EpInfo {
            item: ReadyEpFile,
            disk_path: PathBuf,
            meta: analyze::LocalProbeMeta,
            segments: Vec<MediaSegment>,
            head_fp: Option<Vec<u32>>,
            tail_fp: Option<(f64, Vec<u32>)>,
        }

        let mut prepared = Vec::<EpInfo>::new();
        for item in eps {
            let Some(disk_path) = resolve_local_disk_path(preload_dir, &item.file_path).await else {
                continue;
            };
            let meta = match analyze::probe_local_file(&state.media.ffprobe, &disk_path).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(error = %e, path = %disk_path.display(), "local probe failed during segment analysis");
                    continue;
                }
            };
            let segments = meta.chapters.clone();
            prepared.push(EpInfo {
                item,
                disk_path,
                meta,
                segments,
                head_fp: None,
                tail_fp: None,
            });
        }

        if prepared.is_empty() {
            continue;
        }

        let mut dirty = vec![false; prepared.len()];

        // Extract Chromaprint fingerprints for episodes needing intro or outro when >= 2 episodes are present.
        if prepared.len() >= 2 {
            for ep in &mut prepared {
                if ep.item.analyzed {
                    // Load any existing DB segments so we only scan peers if intro/outro is still missing.
                    let existing = load_segments_for_play(
                        &state.pool,
                        MediaKind::Tv,
                        tmdb_id,
                        season,
                        ep.item.episode,
                        &hash,
                        ep.meta.duration,
                    )
                    .await;
                    for s in existing {
                        if !ep.segments.iter().any(|x| x.kind == s.kind) {
                            ep.segments.push(s);
                        }
                    }
                }
                if !ep.meta.has_audio {
                    continue;
                }
                let Some(dur) = ep.meta.duration.filter(|&d| d >= 40.0) else {
                    continue;
                };
                let need_head = !ep.segments.iter().any(|s| s.kind == SegmentKind::Intro);
                let need_tail = !ep.segments.iter().any(|s| s.kind == SegmentKind::Outro);
                if need_head {
                    let head_len = (dur * 0.50).min(HEAD_SCAN_MAX_SEC);
                    if let Ok(fp) =
                        analyze::extract_chromaprint_window(&state.media.ffmpeg, &ep.disk_path, 0.0, head_len).await
                    {
                        ep.head_fp = Some(fp);
                    }
                }
                if need_tail {
                    let tail_len = (dur * 0.35).min(TAIL_SCAN_MAX_SEC);
                    let tail_start = (dur - tail_len).max(0.0);
                    if let Ok(fp) =
                        analyze::extract_chromaprint_window(&state.media.ffmpeg, &ep.disk_path, tail_start, tail_len)
                            .await
                    {
                        ep.tail_fp = Some((tail_start, fp));
                    }
                }
            }

            // Pairwise match head intros & tail outros across up to 3 peer episodes;
            // also update already-scanned peer episodes if a new download completes their pair.
            let n = prepared.len();
            for i in 0..n {
                if prepared[i].item.analyzed {
                    continue;
                }
                let dur_i = prepared[i].meta.duration.unwrap_or(0.0);
                // Match head intro against peer episodes.
                if !prepared[i].segments.iter().any(|s| s.kind == SegmentKind::Intro) {
                    if let Some(fp_a) = prepared[i].head_fp.as_ref() {
                        let mut hits = Vec::<(f64, f64, f32)>::new();
                        let mut peer_updates = Vec::<(usize, MediaSegment)>::new();
                        for (j, peer) in prepared.iter().enumerate() {
                            if j == i {
                                continue;
                            }
                            if let Some(fp_b) = peer.head_fp.as_ref() {
                                let max_sec = (dur_i * 0.40).min(180.0);
                                if let Some(m) = analyze::find_matching_span(fp_a, fp_b, 0.0, 0.0, 12.0, max_sec) {
                                    hits.push((m.a_start_sec, m.a_end_sec, m.similarity));
                                    if !peer.segments.iter().any(|s| s.kind == SegmentKind::Intro) {
                                        peer_updates.push((
                                            j,
                                            MediaSegment {
                                                kind: SegmentKind::Intro,
                                                start_sec: m.b_start_sec,
                                                end_sec: m.b_end_sec,
                                                source: SegmentSource::LocalAnalysis,
                                                confidence: (0.72 + m.similarity * 0.16).clamp(0.0, 0.95),
                                            },
                                        ));
                                    }
                                    if hits.len() >= 3 {
                                        break;
                                    }
                                }
                            }
                        }
                        if let Some(best_intro) = consensus_segment(&hits, SegmentKind::Intro) {
                            prepared[i].segments.push(best_intro);
                            dirty[i] = true;
                        }
                        for (j, seg) in peer_updates {
                            if !prepared[j].segments.iter().any(|s| s.kind == SegmentKind::Intro) {
                                prepared[j].segments.push(seg);
                                dirty[j] = true;
                            }
                        }
                    }
                }
                // Match tail outro against peer episodes.
                if !prepared[i].segments.iter().any(|s| s.kind == SegmentKind::Outro) {
                    if let Some((start_a, fp_a)) = prepared[i].tail_fp.as_ref() {
                        let mut hits = Vec::<(f64, f64, f32)>::new();
                        let mut peer_updates = Vec::<(usize, MediaSegment)>::new();
                        for (j, peer) in prepared.iter().enumerate() {
                            if j == i {
                                continue;
                            }
                            let dur_j = peer.meta.duration.unwrap_or(0.0);
                            if let Some((start_b, fp_b)) = peer.tail_fp.as_ref() {
                                let max_sec = (dur_i * 0.35).min(180.0);
                                if let Some(m) =
                                    analyze::find_matching_span(fp_a, fp_b, *start_a, *start_b, 12.0, max_sec)
                                {
                                    if m.a_start_sec >= dur_i * 0.50 {
                                        hits.push((m.a_start_sec, m.a_end_sec, m.similarity));
                                        if m.b_start_sec >= dur_j * 0.50
                                            && !peer.segments.iter().any(|s| s.kind == SegmentKind::Outro)
                                        {
                                            peer_updates.push((
                                                j,
                                                MediaSegment {
                                                    kind: SegmentKind::Outro,
                                                    start_sec: m.b_start_sec,
                                                    end_sec: m.b_end_sec,
                                                    source: SegmentSource::LocalAnalysis,
                                                    confidence: (0.72 + m.similarity * 0.16).clamp(0.0, 0.95),
                                                },
                                            ));
                                        }
                                        if hits.len() >= 3 {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(best_outro) = consensus_segment(&hits, SegmentKind::Outro) {
                            prepared[i].segments.push(best_outro);
                            dirty[i] = true;
                        }
                        for (j, seg) in peer_updates {
                            if !prepared[j].segments.iter().any(|s| s.kind == SegmentKind::Outro) {
                                prepared[j].segments.push(seg);
                                dirty[j] = true;
                            }
                        }
                    }
                }
            }
        }

        // For any unanalyzed episode still missing Outro, fall back to black-screen credits detection on the tail;
        // also save updated peer segments discovered reciprocally.
        for (idx, ep) in prepared.iter_mut().enumerate() {
            if ep.item.analyzed && !dirty[idx] {
                continue;
            }
            let dur = ep.meta.duration;
            if !ep.item.analyzed && ep.meta.has_video && !ep.segments.iter().any(|s| s.kind == SegmentKind::Outro) {
                if let Some(d) = dur {
                    let large = ep.item.size_bytes.is_some_and(|b| b > 4_000_000_000);
                    if let Ok(Some(black_outro)) =
                        analyze::detect_black_credits(&state.media.ffmpeg, &ep.disk_path, d, large).await
                    {
                        ep.segments.push(black_outro);
                    }
                }
            }
            if !ep.segments.is_empty() {
                if let Err(e) = save_segments(
                    &state.pool,
                    MediaKind::Tv,
                    tmdb_id,
                    season,
                    ep.item.episode,
                    &hash,
                    dur,
                    &ep.segments,
                )
                .await
                {
                    tracing::warn!(error = %e, episode = ep.item.episode, "failed saving analyzed segments");
                    continue;
                }
            }
            if !ep.item.analyzed {
                let _ = sqlx::query(
                    "INSERT INTO media_analysis_done (release_infohash, season, episode, file_path, duration_sec, analyzed_at)
                     VALUES ($1, $2, $3, $4, $5, now())
                     ON CONFLICT (release_infohash, season, episode) DO UPDATE SET
                       file_path = EXCLUDED.file_path,
                       duration_sec = COALESCE(EXCLUDED.duration_sec, media_analysis_done.duration_sec),
                       analyzed_at = now()",
                )
                .bind(hash.to_ascii_lowercase())
                .bind(ep.item.season)
                .bind(ep.item.episode)
                .bind(&ep.item.file_path)
                .bind(dur)
                .execute(&state.pool)
                .await;
                processed += 1;
            }
        }
    }

    Ok(processed)
}

fn consensus_segment(hits: &[(f64, f64, f32)], kind: SegmentKind) -> Option<MediaSegment> {
    if hits.is_empty() {
        return None;
    }
    let &(start_sec, end_sec, sim) = hits
        .iter()
        .max_by(|a, b| ((a.1 - a.0) * f64::from(a.2)).total_cmp(&((b.1 - b.0) * f64::from(b.2))))?;
    let peers_bonus = if hits.len() >= 2 { 0.12 } else { 0.0 };
    let confidence = (0.72 + sim * 0.16 + peers_bonus).clamp(0.0, 0.95);
    Some(MediaSegment {
        kind,
        start_sec,
        end_sec,
        source: SegmentSource::LocalAnalysis,
        confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_introdb_and_aniskip_payloads() {
        let introdb = br#"{
            "imdb_id":"tt0903747",
            "season":2,
            "episode":1,
            "intro":{"start_sec":77,"end_sec":94,"confidence":0.95},
            "recap":{"start_sec":95,"end_sec":259,"confidence":0.8},
            "outro":{"start_sec":2791,"end_sec":2843,"confidence":1.0},
            "post_credits":null
        }"#;
        let segs = parse_introdb_json(introdb).unwrap();
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].kind, SegmentKind::Intro);
        assert_eq!(segs[0].start_sec, 77.0);
        assert_eq!(segs[1].kind, SegmentKind::Recap);
        assert_eq!(segs[2].kind, SegmentKind::Outro);

        let aniskip = br#"{
            "found":true,
            "results":[
                {"interval":{"startTime":90.0,"endTime":150.0},"skipType":"op","skipId":"1","episodeLength":1440.0},
                {"interval":{"startTime":1340.0,"endTime":1430.0},"skipType":"ed","skipId":"2","episodeLength":1440.0}
            ]
        }"#;
        let a_segs = parse_aniskip_json(aniskip).unwrap();
        assert_eq!(a_segs.len(), 2);
        assert_eq!(a_segs[0].kind, SegmentKind::Intro);
        assert_eq!(a_segs[0].source, SegmentSource::AniSkip);
        assert_eq!(a_segs[1].kind, SegmentKind::Outro);
    }
}
