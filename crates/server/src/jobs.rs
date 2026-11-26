//! Background jobs: new-episode notifications through the Telegram bot and
//! removal of preloads after they were watched.

use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

use chrono::{FixedOffset, Utc};
use kino_core::{episode_code, MediaKind, PreloadState, TitleStatus};
use sqlx::Row;

use crate::{api::AppState, userdata};

/// Checks followed series for new episodes and tells each user once.
///
/// The first check of a series only records the latest episode, so nobody gets
/// a message for episodes that were out before they started watching.
/// A watched series with a new episode goes back to "Смотрю".
///
/// `send(telegram_id, text, open_url)`: `open_url` is the Mini App link that opens the series.
pub async fn check_new_episodes<F, Fut>(state: &AppState, send: F) -> anyhow::Result<usize>
where
    F: Fn(i64, String, String) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let mut sent = 0;
    for f in userdata::followed_series(&state.pool).await? {
        let Ok(d) = state.catalog.details(MediaKind::Tv, f.tmdb_id).await else {
            continue;
        };
        let Some(latest) = d.last_episode else { continue };
        match f.notified {
            Some(seen) if latest <= seen => continue,
            None => {}
            Some(_) => {
                let text = format!(
                    "Вышла новая серия «{}»: {}.",
                    d.summary.title,
                    episode_code(latest.0, latest.1),
                );
                let open_url = format!("{}/?open=tv_{}", state.config.public_url, f.tmdb_id);
                if let Err(e) = send(f.telegram_id, text, open_url).await {
                    tracing::warn!(error = %e, user = f.user_id, "new-episode message failed");
                    continue;
                }
                sent += 1;
                if matches!(f.status, TitleStatus::Watched | TitleStatus::WaitingEpisode) {
                    userdata::set_status(
                        &state.pool,
                        f.user_id,
                        MediaKind::Tv,
                        f.tmdb_id,
                        Some(TitleStatus::Watching),
                    )
                    .await?;
                }
            }
        }
        userdata::set_notified(&state.pool, f.user_id, f.tmdb_id, latest).await?;
    }
    Ok(sent)
}

/// Checks "Жду" titles (movies and unreleased series) and transitions released ones to "Хочу посмотреть", notifying the user.
pub async fn check_released_movies<F, Fut>(state: &AppState, send: F) -> anyhow::Result<usize>
where
    F: Fn(i64, String, String) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let msk = FixedOffset::east_opt(3 * 3600).expect("UTC+3");
    let today = Utc::now().with_timezone(&msk).format("%Y-%m-%d").to_string();
    let rows = sqlx::query(
        "SELECT s.user_id, u.telegram_id, s.media_kind, s.tmdb_id
         FROM title_state s JOIN users u ON u.id = s.user_id
         WHERE s.status = 'waiting_release'",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut sent = 0usize;
    for r in rows {
        let user_id: i64 = r.try_get("user_id")?;
        let telegram_id: i64 = r.try_get("telegram_id")?;
        let kind_raw: String = r.try_get("media_kind")?;
        let Some(kind) = MediaKind::parse(&kind_raw) else {
            continue;
        };
        let tmdb_id: i64 = r.try_get("tmdb_id")?;
        let Ok(d) = state.catalog.details(kind, tmdb_id).await else {
            continue;
        };
        if !d.is_unreleased(&today) {
            userdata::set_status(&state.pool, user_id, kind, tmdb_id, Some(TitleStatus::Want)).await?;
            let noun = match kind {
                MediaKind::Movie => "фильм",
                MediaKind::Tv => "сериал",
            };
            let text = format!("Вышел {noun} «{}», который вы ждали.", d.summary.title);
            let open_url = format!("{}/?open={}_{tmdb_id}", state.config.public_url, kind.as_str());
            if let Err(e) = send(telegram_id, text, open_url).await {
                tracing::warn!(error = %e, user = user_id, "release notification failed");
            } else {
                sent += 1;
            }
        }
    }
    Ok(sent)
}

/// Removes preloads and expired partial episodes past their removal time, with their files once nobody
/// else keeps the same release.
pub async fn expire_preloads(state: &AppState) -> anyhow::Result<usize> {
    let mut removed = 0usize;
    let due_eps = userdata::preload_episodes_due(&state.pool).await?;
    let mut affected_hashes: Vec<String> = Vec::new();
    for (pid, infohash, season, episode) in due_eps {
        userdata::preload_episode_mark_deleted(&state.pool, pid, season, episode).await?;
        if !affected_hashes.iter().any(|h| h.eq_ignore_ascii_case(&infohash)) {
            affected_hashes.push(infohash);
        }
        removed += 1;
    }
    for hash in affected_hashes {
        match userdata::preload_in_use(&state.pool, &hash).await {
            Ok(false) => {
                if let Some(qbit) = &state.media.qbit {
                    if let Err(e) = qbit.delete(&hash).await {
                        tracing::warn!(error = %e, infohash = hash, "could not delete expired partial preload");
                    }
                }
            }
            Ok(true) => {
                if let Err(e) = sync_qbit_file_priorities(state, &hash).await {
                    tracing::warn!(error = %e, infohash = hash, "could not update qBittorrent file priorities");
                }
            }
            Err(e) => tracing::warn!(error = %e, "preload lookup failed"),
        }
    }

    let due = userdata::preloads_due(&state.pool).await?;
    for (id, infohash) in &due {
        if let Some(qbit) = &state.media.qbit {
            match userdata::preload_in_use_except(&state.pool, infohash, *id).await {
                Ok(false) => {
                    if let Err(e) = qbit.delete(infohash).await {
                        tracing::warn!(error = %e, infohash, "could not delete expired preloaded files; will retry");
                        continue;
                    }
                }
                Ok(true) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "preload lookup failed");
                    continue;
                }
            }
        }
        userdata::preload_mark_deleted(&state.pool, *id).await?;
        removed += 1;
    }
    Ok(removed)
}

/// How long a release the indexer once listed is kept after it was last seen.
const SOURCE_KEEP_DAYS: i32 = 30;

/// Forgets releases (and their `.torrent` files) that were not listed for a month,
/// unless someone still has them in progress, as their chosen release or as a
/// download. Searching again brings back what is still on the trackers.
pub async fn prune_sources(state: &AppState) -> anyhow::Result<u64> {
    let done = sqlx::query(
        "DELETE FROM torrent_source s
         WHERE s.updated_at < now() - make_interval(days => $1)
           AND NOT EXISTS (SELECT 1 FROM watch_progress p WHERE p.release_infohash IN (s.key, s.infohash))
           AND NOT EXISTS (SELECT 1 FROM release_choice c WHERE c.infohash IN (s.key, s.infohash))
           AND NOT EXISTS (SELECT 1 FROM preload l WHERE l.infohash IN (s.key, s.infohash))",
    )
    .bind(SOURCE_KEEP_DAYS)
    .execute(&state.pool)
    .await?;
    Ok(done.rows_affected())
}

/// Deletes a release from the downloader unless another preload still uses it.
pub async fn remove_files(state: &AppState, infohash: &str) {
    let Some(qbit) = &state.media.qbit else { return };
    match userdata::preload_in_use(&state.pool, infohash).await {
        Ok(false) => {
            if let Err(e) = qbit.delete(infohash).await {
                tracing::warn!(error = %e, infohash, "could not delete the preloaded files");
            }
        }
        Ok(true) => {}
        Err(e) => tracing::warn!(error = %e, "preload lookup failed"),
    }
}

/// Aligns qBittorrent's file priorities for `hash` with all active `preload` / `preload_episode` rows.
pub async fn sync_qbit_file_priorities(state: &AppState, hash: &str) -> anyhow::Result<()> {
    let Some(qbit) = &state.media.qbit else { return Ok(()) };
    let wanted = userdata::preload_wanted_for_hash(&state.pool, hash).await?;
    let qfiles = qbit.file_states(hash).await?;
    if qfiles.is_empty() {
        return Ok(());
    }
    if wanted.whole_torrent {
        let to_enable: Vec<i32> = qfiles
            .iter()
            .filter(|f| f.priority == 0)
            .map(|f| f.qbit_index)
            .collect();
        qbit.set_file_priority(hash, &to_enable, 1).await?;
        return Ok(());
    }
    let entries: Vec<crate::torrent::files::FileEntry> = qfiles.iter().map(|f| f.as_file_entry()).collect();
    let wanted_1based = crate::torrent::files::indices_for_episodes(&entries, &wanted.episodes);
    let to_enable: Vec<i32> = qfiles
        .iter()
        .filter(|f| wanted_1based.contains(&f.index) && f.priority == 0)
        .map(|f| f.qbit_index)
        .collect();
    let to_disable: Vec<i32> = qfiles
        .iter()
        .filter(|f| !wanted_1based.contains(&f.index) && f.priority > 0)
        .map(|f| f.qbit_index)
        .collect();
    qbit.set_file_priority(hash, &to_disable, 0).await?;
    qbit.set_file_priority(hash, &to_enable, 1).await?;
    Ok(())
}

/// Populates `preload_episode` for existing ready TV preloads that predate per-episode tracking.
pub async fn backfill_preload_episodes(state: &AppState) -> anyhow::Result<()> {
    let Some(qbit) = &state.media.qbit else { return Ok(()) };
    let missing = userdata::preloads_tv_ready_needing_backfill(&state.pool).await?;
    for (preload_id, hash, season_hint) in missing {
        let Ok(qfiles) = qbit.file_states(&hash).await else {
            continue;
        };
        let default_s = season_hint.max(1);
        for f in qfiles {
            if !crate::torrent::files::is_video(&f.path) || f.priority == 0 {
                continue;
            }
            let (ep_s, Some(ep_e)) = crate::torrent::files::episode_of(&f.path) else {
                continue;
            };
            let s = ep_s.unwrap_or(default_s);
            let done = f.progress >= 0.9999 || f.progress == 0.0;
            if !done {
                continue;
            }
            let sz = i64::try_from(f.length).ok().filter(|&b| b > 0);
            let _ = userdata::upsert_preload_episode_progress(
                &state.pool,
                preload_id,
                s,
                ep_e,
                Some(f.index),
                Some(&f.path),
                sz,
                PreloadState::Ready,
                1.0,
                false,
            )
            .await;
        }
    }
    Ok(())
}

/// Copies download progress and live telemetry from qBittorrent into the preload records.
pub async fn sync_preloads(state: &AppState) -> anyhow::Result<()> {
    let Some(qbit) = &state.media.qbit else { return Ok(()) };
    if let Err(e) = backfill_preload_episodes(state).await {
        tracing::debug!(error = %e, "preload episode backfill failed");
    }
    let active_rows = userdata::preloads_active_rows(&state.pool).await?;
    if active_rows.is_empty() {
        state.media.preload_telemetry.write().await.clear();
        return Ok(());
    }
    let mut hashes: Vec<String> = Vec::with_capacity(active_rows.len());
    for row in &active_rows {
        if !hashes.iter().any(|h| h.eq_ignore_ascii_case(&row.infohash)) {
            hashes.push(row.infohash.clone());
        }
    }
    let infos = qbit.info(&hashes).await?;
    let mut telemetry_updates = HashMap::with_capacity(hashes.len());

    #[derive(Default)]
    struct ReadyGroup {
        users: Vec<i64>,
        tg_users: Vec<(i64, i64)>,
        raw_title: Option<String>,
        size_bytes: Option<i64>,
    }
    let mut ready_groups: HashMap<(MediaKind, i64, i32, i32, String), ReadyGroup> = HashMap::new();

    for hash in &hashes {
        let rows_for_hash: Vec<&userdata::ActivePreloadRow> = active_rows
            .iter()
            .filter(|r| r.infohash.eq_ignore_ascii_case(hash))
            .collect();
        let any_tv = rows_for_hash.iter().any(|r| r.kind == MediaKind::Tv);
        let qfiles = if any_tv {
            qbit.file_states(hash).await.unwrap_or_default()
        } else {
            Vec::new()
        };
        let info = infos.iter().find(|i| i.hash.eq_ignore_ascii_case(hash));
        let (tor_st, tor_progress, tor_size, speed_bps, dl_phase) = match info {
            None => (PreloadState::Failed, 0.0, None, 0, kino_core::PreloadPhase::Failed),
            Some(i) if i.failed() => (
                PreloadState::Failed,
                i.progress,
                Some(i.size),
                0,
                kino_core::PreloadPhase::Failed,
            ),
            Some(i) if i.done() => (
                PreloadState::Ready,
                1.0,
                Some(i.size),
                0,
                kino_core::PreloadPhase::Ready,
            ),
            Some(i) => (
                PreloadState::Downloading,
                i.progress,
                Some(i.size),
                i.speed_bps(),
                i.dl_phase(),
            ),
        };
        if matches!(tor_st, PreloadState::Queued | PreloadState::Downloading) {
            telemetry_updates.insert(hash.to_ascii_lowercase(), (speed_bps, dl_phase));
        }

        if any_tv && !qfiles.is_empty() && !matches!(tor_st, PreloadState::Failed) {
            let wanted = userdata::preload_wanted_for_hash(&state.pool, hash).await?;
            if !wanted.whole_torrent {
                let entries: Vec<crate::torrent::files::FileEntry> = qfiles.iter().map(|f| f.as_file_entry()).collect();
                let wanted_1based = crate::torrent::files::indices_for_episodes(&entries, &wanted.episodes);
                let mismatch = qfiles.iter().any(|f| {
                    let want = wanted_1based.contains(&f.index);
                    (want && f.priority == 0) || (!want && f.priority > 0)
                });
                if mismatch {
                    let _ = sync_qbit_file_priorities(state, hash).await;
                }
            }
        }

        for row in rows_for_hash {
            if row.kind == MediaKind::Movie || (row.whole_torrent && qfiles.is_empty()) {
                let transitioned =
                    userdata::update_preload_row_progress(&state.pool, row.id, tor_st, tor_progress, tor_size).await?;
                if transitioned {
                    let grp = ready_groups
                        .entry((row.kind, row.tmdb_id, row.season, 0, hash.to_ascii_lowercase()))
                        .or_default();
                    if !grp.users.contains(&row.user_id) {
                        grp.users.push(row.user_id);
                    }
                    if !grp.tg_users.iter().any(|&(uid, _)| uid == row.user_id) {
                        grp.tg_users.push((row.user_id, row.telegram_id));
                    }
                    if grp.raw_title.is_none() {
                        grp.raw_title = row.title.clone();
                    }
                    if grp.size_bytes.is_none() {
                        grp.size_bytes = tor_size;
                    }
                }
                continue;
            }

            let file_entries: Vec<crate::torrent::files::FileEntry> =
                qfiles.iter().map(|f| f.as_file_entry()).collect();

            if row.whole_torrent {
                let default_s = row.season.max(1);
                for f in &qfiles {
                    if !crate::torrent::files::is_video(&f.path) || f.priority == 0 {
                        continue;
                    }
                    let (ep_s, Some(ep_e)) = crate::torrent::files::episode_of(&f.path) else {
                        continue;
                    };
                    let s = ep_s.unwrap_or(default_s);
                    let ep_done = f.progress >= 0.9999 || (tor_st == PreloadState::Ready && f.priority > 0);
                    let ep_st = if tor_st == PreloadState::Failed {
                        PreloadState::Failed
                    } else if ep_done {
                        PreloadState::Ready
                    } else {
                        PreloadState::Downloading
                    };
                    let ep_prog = if ep_done { 1.0 } else { f.progress };
                    let sz = i64::try_from(f.length).ok().filter(|&b| b > 0);
                    let _ = userdata::upsert_preload_episode_progress(
                        &state.pool,
                        row.id,
                        s,
                        ep_e,
                        Some(f.index),
                        Some(&f.path),
                        sz,
                        ep_st,
                        ep_prog,
                        true,
                    )
                    .await;
                }
                let transitioned =
                    userdata::update_preload_row_progress(&state.pool, row.id, tor_st, tor_progress, tor_size).await?;
                if transitioned {
                    let grp = ready_groups
                        .entry((row.kind, row.tmdb_id, row.season, 0, hash.to_ascii_lowercase()))
                        .or_default();
                    if !grp.users.contains(&row.user_id) {
                        grp.users.push(row.user_id);
                    }
                    if !grp.tg_users.iter().any(|&(uid, _)| uid == row.user_id) {
                        grp.tg_users.push((row.user_id, row.telegram_id));
                    }
                    if grp.raw_title.is_none() {
                        grp.raw_title = row.title.clone();
                    }
                    if grp.size_bytes.is_none() {
                        grp.size_bytes = tor_size;
                    }
                }
            } else {
                // Partial download: update each requested episode individually from qBittorrent file states.
                let mut total_bytes: i64 = 0;
                let mut has_bytes = false;
                let mut weighted_done = 0.0f64;
                let mut weighted_total = 0.0f64;
                let mut ready_cnt = 0usize;
                let mut dl_cnt = 0usize;
                let mut fail_cnt = 0usize;
                let ep_count = row.episodes.len();

                for ep in &row.episodes {
                    let matched = if qfiles.is_empty() {
                        None
                    } else {
                        crate::torrent::files::pick(&file_entries, Some((ep.season, ep.episode)))
                            .and_then(|fe| qfiles.iter().find(|qf| qf.index == fe.index))
                    };
                    let (ep_st, ep_prog, ep_idx, ep_path, ep_sz) = match (tor_st, matched) {
                        (PreloadState::Failed, _) => (
                            PreloadState::Failed,
                            0.0,
                            ep.file_index,
                            ep.file_path.as_deref(),
                            ep.size_bytes,
                        ),
                        (_, Some(qf)) => {
                            let done = qf.progress >= 0.9999 || (tor_st == PreloadState::Ready && qf.priority > 0);
                            let st = if done {
                                PreloadState::Ready
                            } else {
                                PreloadState::Downloading
                            };
                            let prog = if done { 1.0 } else { qf.progress };
                            let sz = i64::try_from(qf.length).ok().filter(|&b| b > 0).or(ep.size_bytes);
                            (st, prog, Some(qf.index), Some(qf.path.as_str()), sz)
                        }
                        (_, None) if !qfiles.is_empty() => (
                            PreloadState::Failed,
                            0.0,
                            ep.file_index,
                            ep.file_path.as_deref(),
                            ep.size_bytes,
                        ),
                        _ => (
                            if ep.state == PreloadState::Ready {
                                PreloadState::Ready
                            } else {
                                tor_st
                            },
                            ep.progress,
                            ep.file_index,
                            ep.file_path.as_deref(),
                            ep.size_bytes,
                        ),
                    };
                    match ep_st {
                        PreloadState::Ready => ready_cnt += 1,
                        PreloadState::Failed => fail_cnt += 1,
                        PreloadState::Queued | PreloadState::Downloading => dl_cnt += 1,
                        PreloadState::Deleted => {}
                    }
                    if let Some(b) = ep_sz.filter(|&b| b > 0) {
                        has_bytes = true;
                        total_bytes += b;
                        weighted_done += (b as f64) * f64::from(ep_prog.clamp(0.0, 1.0));
                        weighted_total += b as f64;
                    }
                    let ep_became_ready = userdata::upsert_preload_episode_progress(
                        &state.pool,
                        row.id,
                        ep.season,
                        ep.episode,
                        ep_idx,
                        ep_path,
                        ep_sz,
                        ep_st,
                        ep_prog,
                        false,
                    )
                    .await?;
                    if ep_became_ready {
                        let grp = ready_groups
                            .entry((row.kind, row.tmdb_id, ep.season, ep.episode, hash.to_ascii_lowercase()))
                            .or_default();
                        if !grp.users.contains(&row.user_id) {
                            grp.users.push(row.user_id);
                        }
                        if !grp.tg_users.iter().any(|&(uid, _)| uid == row.user_id) {
                            grp.tg_users.push((row.user_id, row.telegram_id));
                        }
                        if grp.raw_title.is_none() {
                            grp.raw_title = row.title.clone();
                        }
                        if grp.size_bytes.is_none() {
                            grp.size_bytes = ep_sz;
                        }
                    }
                }
                let next_st = if ep_count == 0 {
                    tor_st
                } else if dl_cnt > 0 {
                    PreloadState::Downloading
                } else if ready_cnt == ep_count {
                    PreloadState::Ready
                } else if fail_cnt == ep_count {
                    PreloadState::Failed
                } else if ready_cnt > 0 {
                    PreloadState::Ready
                } else {
                    PreloadState::Failed
                };
                let next_prog = if has_bytes && weighted_total > 0.0 {
                    (weighted_done / weighted_total) as f32
                } else {
                    tor_progress
                };
                let _ = userdata::update_preload_row_progress(
                    &state.pool,
                    row.id,
                    next_st,
                    next_prog,
                    has_bytes.then_some(total_bytes),
                )
                .await?;
            }
        }
    }

    let has_ready_tv = !ready_groups.is_empty();
    for ((kind, tmdb_id, season, episode, hash), grp) in ready_groups {
        let parsed = grp.raw_title.as_deref().map(crate::torrent::title::parse_title);
        let quality = parsed.as_ref().map(|p| p.quality.clone());
        let voice = parsed.as_ref().map(|p| p.voice.clone()).filter(|v| !v.is_empty());
        state.sync.notify_preload_ready(
            &grp.users,
            kind,
            tmdb_id,
            season,
            episode,
            &hash,
            grp.raw_title.as_deref(),
            quality.as_deref(),
            voice.as_deref(),
        );
        spawn_preload_ready_telegram(
            state,
            PreloadReadyTelegramJob {
                recipients: grp.tg_users,
                kind,
                tmdb_id,
                season,
                episode,
                infohash: hash,
                raw_title: grp.raw_title,
                quality,
                voice,
                size_bytes: grp.size_bytes,
            },
        );
    }

    *state.media.preload_telemetry.write().await = telemetry_updates;
    if has_ready_tv {
        state.media.segment_notify.notify_one();
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct PreloadReadyTelegramJob {
    recipients: Vec<(i64, i64)>,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    infohash: String,
    raw_title: Option<String>,
    quality: Option<String>,
    voice: Option<String>,
    size_bytes: Option<i64>,
}

const CATALOG_NOTIFY_TIMEOUT: Duration = Duration::from_secs(8);
const TELEGRAM_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(6)];

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

fn short_quality(q: &str) -> Option<&str> {
    let trimmed = q.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(match kino_core::normalize_quality_key(trimmed) {
        "2160p" => "4K",
        other => other,
    })
}

fn human_size(bytes: i64) -> Option<String> {
    u64::try_from(bytes).ok().filter(|&b| b > 0).map(kino_core::format_size)
}

/// Formats the Telegram HTML message sent when a user's server download completes.
pub fn format_preload_ready_message(
    kind: MediaKind,
    season: i32,
    catalog_title: Option<&str>,
    raw_title: Option<&str>,
    quality: Option<&str>,
    voice: Option<&str>,
    size_bytes: Option<i64>,
) -> String {
    format_preload_ready_episode_message(kind, season, 0, catalog_title, raw_title, quality, voice, size_bytes)
}

/// Formats the Telegram HTML message sent when a movie, season, or individual episode completes downloading.
#[allow(clippy::too_many_arguments)]
pub fn format_preload_ready_episode_message(
    kind: MediaKind,
    season: i32,
    episode: i32,
    catalog_title: Option<&str>,
    raw_title: Option<&str>,
    quality: Option<&str>,
    voice: Option<&str>,
    size_bytes: Option<i64>,
) -> String {
    let clean_title = catalog_title.map(str::trim).filter(|s| !s.is_empty());
    let is_single_ep = kind == MediaKind::Tv && season > 0 && episode > 0;
    let mut text = match (kind, season, clean_title) {
        (MediaKind::Movie, _, Some(name)) => format!("✅ <b>Фильм «{}» скачался</b>", escape_html(name)),
        (MediaKind::Movie, _, None) => "✅ <b>Фильм скачался</b>".to_owned(),
        (MediaKind::Tv, s, Some(name)) if s > 0 && episode > 0 => format!(
            "✅ <b>Серия {} сериала «{}» скачалась</b>",
            kino_core::episode_code(s, episode),
            escape_html(name)
        ),
        (MediaKind::Tv, s, None) if s > 0 && episode > 0 => {
            format!(
                "✅ <b>Серия {} сериала скачалась</b>",
                kino_core::episode_code(s, episode)
            )
        }
        (MediaKind::Tv, s, Some(name)) if s > 0 => {
            format!("✅ <b>Сезон {s} сериала «{}» скачался</b>", escape_html(name))
        }
        (MediaKind::Tv, s, None) if s > 0 => format!("✅ <b>Сезон {s} сериала скачался</b>"),
        (MediaKind::Tv, _, Some(name)) => format!("✅ <b>Сериал «{}» скачался</b>", escape_html(name)),
        (MediaKind::Tv, _, None) => "✅ <b>Сериал скачался</b>".to_owned(),
    };
    if clean_title.is_none() {
        if let Some(raw) = raw_title.map(str::trim).filter(|s| !s.is_empty()) {
            text.push_str(&format!("\n<i>{}</i>", escape_html(raw)));
        }
    }
    let mut meta = Vec::new();
    if let Some(q) = quality.and_then(short_quality) {
        meta.push(q.to_owned());
    }
    if let Some(v) = voice.map(str::trim).filter(|v| !v.is_empty()) {
        meta.push(v.to_owned());
    }
    if let Some(sz) = size_bytes.and_then(human_size) {
        meta.push(sz);
    }
    if meta.is_empty() {
        let ready = if is_single_ep { "Готова" } else { "Готов" };
        text.push_str(&format!("\n{ready} к просмотру с сервера."));
    } else {
        let ready = if is_single_ep { "готова" } else { "готов" };
        text.push_str(&format!(
            "\n{} — {ready} к просмотру с сервера.",
            escape_html(&meta.join(" · "))
        ));
    }
    text
}

fn spawn_preload_ready_telegram(state: &AppState, job: PreloadReadyTelegramJob) {
    let Some(api_base) = state.config.telegram_api_url.clone() else {
        return;
    };
    if state.config.bot_token.trim().is_empty() || job.recipients.is_empty() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        send_preload_ready_telegram(&state, &api_base, job).await;
    });
}

fn preload_ready_open_url_with_episode(
    public_url: &str,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    infohash: &str,
) -> String {
    let base_url = public_url.trim_end_matches('/');
    let hash = infohash.to_ascii_lowercase();
    if kind == MediaKind::Tv && season > 0 && episode > 0 {
        format!("{base_url}/?open=tv_{tmdb_id}&play=1&season={season}&episode={episode}&release={hash}")
    } else if kind == MediaKind::Tv && season > 0 {
        format!("{base_url}/?open=tv_{tmdb_id}&play=1&season={season}&release={hash}")
    } else {
        format!("{base_url}/?open={}_{tmdb_id}&play=1&release={hash}", kind.as_str())
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn preload_ready_open_url(public_url: &str, kind: MediaKind, tmdb_id: i64, season: i32, infohash: &str) -> String {
    preload_ready_open_url_with_episode(public_url, kind, tmdb_id, season, 0, infohash)
}

async fn send_preload_ready_telegram(state: &AppState, api_base: &str, job: PreloadReadyTelegramJob) {
    let catalog_title =
        match tokio::time::timeout(CATALOG_NOTIFY_TIMEOUT, state.catalog.details(job.kind, job.tmdb_id)).await {
            Ok(Ok(d)) => {
                let trimmed = d.summary.title.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_owned())
            }
            Ok(Err(e)) => {
                tracing::debug!(error = %e, tmdb_id = job.tmdb_id, "catalog lookup failed for preload notification");
                None
            }
            Err(_) => {
                tracing::debug!(
                    tmdb_id = job.tmdb_id,
                    "catalog lookup timed out for preload notification"
                );
                None
            }
        };
    let text = format_preload_ready_episode_message(
        job.kind,
        job.season,
        job.episode,
        catalog_title.as_deref(),
        job.raw_title.as_deref(),
        job.quality.as_deref(),
        job.voice.as_deref(),
        job.size_bytes,
    );
    let open_url = preload_ready_open_url_with_episode(
        &state.config.public_url,
        job.kind,
        job.tmdb_id,
        job.season,
        job.episode,
        &job.infohash,
    );
    for (user_id, telegram_id) in job.recipients {
        if telegram_id <= 0 {
            continue;
        }
        let mut attempt = 0usize;
        loop {
            match telegram_send_inner(
                &state.telegram,
                api_base,
                &state.config.bot_token,
                telegram_id,
                &text,
                &open_url,
                "▶ Смотреть",
                Some("HTML"),
            )
            .await
            {
                Ok(()) => {
                    tracing::info!(
                        user = user_id,
                        telegram_id,
                        kind = job.kind.as_str(),
                        tmdb_id = job.tmdb_id,
                        season = job.season,
                        episode = job.episode,
                        "sent preload-ready telegram notification"
                    );
                    break;
                }
                Err(SendError::Transient(e)) if attempt < TELEGRAM_RETRY_DELAYS.len() => {
                    let delay = TELEGRAM_RETRY_DELAYS[attempt];
                    attempt += 1;
                    tracing::debug!(
                        error = %e,
                        user = user_id,
                        telegram_id,
                        attempt,
                        "retrying preload-ready telegram message"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(SendError::Permanent(e) | SendError::Transient(e)) => {
                    tracing::warn!(
                        error = %e,
                        user = user_id,
                        telegram_id,
                        kind = job.kind.as_str(),
                        tmdb_id = job.tmdb_id,
                        "preload-ready telegram message failed"
                    );
                    break;
                }
            }
        }
    }
}

#[derive(Debug)]
enum SendError {
    Permanent(anyhow::Error),
    Transient(anyhow::Error),
}

#[allow(clippy::too_many_arguments)]
async fn telegram_send_inner(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: i64,
    text: &str,
    open_url: &str,
    button_text: &str,
    parse_mode: Option<&str>,
) -> Result<(), SendError> {
    let base = api_base.trim_end_matches('/');
    let mut body = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "reply_markup": { "inline_keyboard": [[{ "text": button_text, "web_app": { "url": open_url } }]] },
    });
    if let Some(mode) = parse_mode {
        body["parse_mode"] = serde_json::Value::String(mode.to_owned());
    }
    let resp = client
        .post(format!("{base}/bot{token}/sendMessage"))
        .json(&body)
        .send()
        .await
        .map_err(|e| SendError::Transient(anyhow::anyhow!("Telegram request failed: {}", e.without_url())))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let err = anyhow::anyhow!("Telegram answered {status}");
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        Err(SendError::Transient(err))
    } else {
        Err(SendError::Permanent(err))
    }
}

/// Sends a message through the Telegram Bot API, with a button that opens `open_url` as a Mini App.
pub async fn telegram_send(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: i64,
    text: String,
    open_url: String,
) -> anyhow::Result<()> {
    telegram_send_inner(client, api_base, token, chat_id, &text, &open_url, "Смотреть", None)
        .await
        .map_err(|e| match e {
            SendError::Permanent(err) | SendError::Transient(err) => err,
        })
}

/// Runs the jobs forever: preload expiry hourly, the episode check every `every`.
pub fn spawn(state: AppState, every: Duration) {
    let client = crate::config::http_client(state.config.telegram_proxy.as_deref(), Duration::from_secs(30))
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "TELEGRAM_PROXY is wrong, sending to Telegram directly");
            reqwest::Client::new()
        });
    let token = Arc::new(state.config.bot_token.clone());
    let api_base = Arc::new(
        state
            .config
            .telegram_api_url
            .clone()
            .unwrap_or_else(|| "https://api.telegram.org".into()),
    );
    let sync_state = state.clone();
    let seg_state = state.clone();
    tokio::spawn(async move {
        // Brief grace delay on startup so initial HTTP traffic settles before analyzing ready disk files.
        tokio::time::sleep(Duration::from_secs(15)).await;
        loop {
            match crate::segments::analyze_ready_preloads(&seg_state).await {
                Ok(n) if n > 0 => tracing::info!(analyzed = n, "analyzed preloaded episodes for intro/credits"),
                Ok(_) => {}
                Err(e) => tracing::debug!(error = %e, "preloaded segment analysis failed"),
            }
            let _ = tokio::time::timeout(Duration::from_secs(600), seg_state.media.segment_notify.notified()).await;
        }
    });
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            if let Err(e) = sync_preloads(&sync_state).await {
                tracing::warn!(error = %e, "preload progress update failed");
            }
        }
    });
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        let mut since_check = every;
        loop {
            tick.tick().await;
            if let Err(e) = expire_preloads(&state).await {
                tracing::warn!(error = %e, "preload expiry failed");
            }
            match prune_sources(&state).await {
                Ok(n) if n > 0 => tracing::info!(removed = n, "old releases forgotten"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "pruning old releases failed"),
            }
            match crate::db::auto_drop_stale_titles(&state.pool).await {
                Ok(n) if n > 0 => tracing::info!(dropped = n, "stale watching titles auto-dropped"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "auto-dropping stale titles failed"),
            }
            state.media.pause_cache.prune().await;
            since_check += Duration::from_secs(3600);
            if since_check >= every {
                since_check = Duration::ZERO;
                let send = |chat, text, open_url| {
                    let (client, api_base, token) = (client.clone(), api_base.clone(), token.clone());
                    async move { telegram_send(&client, &api_base, &token, chat, text, open_url).await }
                };
                match check_new_episodes(&state, &send).await {
                    Ok(n) if n > 0 => tracing::info!(sent = n, "new-episode notifications"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "new-episode check failed"),
                }
                match check_released_movies(&state, &send).await {
                    Ok(n) if n > 0 => tracing::info!(sent = n, "movie-release notifications"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "movie-release check failed"),
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_preload_ready_messages_with_details_and_fallbacks() {
        let movie = format_preload_ready_message(
            MediaKind::Movie,
            0,
            Some("Интерстеллар"),
            Some("Interstellar (2014) BDRip 2160p"),
            Some("2160p"),
            Some("Дубляж"),
            Some(28_400_000_000),
        );
        assert_eq!(
            movie,
            "✅ <b>Фильм «Интерстеллар» скачался</b>\n4K · Дубляж · 26.4 ГБ — готов к просмотру с сервера."
        );

        let tv_season = format_preload_ready_message(
            MediaKind::Tv,
            2,
            Some("Во все тяжкие & Co <test>"),
            None,
            Some("1080p"),
            Some("LostFilm"),
            Some(18_700_000_000),
        );
        assert_eq!(
            tv_season,
            "✅ <b>Сезон 2 сериала «Во все тяжкие &amp; Co &lt;test&gt;» скачался</b>\n1080p · LostFilm · 17.4 ГБ — готов к просмотру с сервера."
        );

        let tv_pack = format_preload_ready_message(MediaKind::Tv, 0, Some("Тьма"), None, None, None, None);
        assert_eq!(
            tv_pack,
            "✅ <b>Сериал «Тьма» скачался</b>\nГотов к просмотру с сервера."
        );

        let fallback = format_preload_ready_message(
            MediaKind::Movie,
            0,
            None,
            Some("Raw <Release> & Name"),
            Some("720p"),
            None,
            Some(500_000_000),
        );
        assert_eq!(
            fallback,
            "✅ <b>Фильм скачался</b>\n<i>Raw &lt;Release&gt; &amp; Name</i>\n720p · 477 МБ — готов к просмотру с сервера."
        );

        let hash = "Ab".repeat(20);
        assert_eq!(
            preload_ready_open_url("https://kino.example.com/", MediaKind::Movie, 603, 0, &hash),
            format!(
                "https://kino.example.com/?open=movie_603&play=1&release={}",
                "ab".repeat(20)
            )
        );
        assert_eq!(
            preload_ready_open_url("https://kino.example.com", MediaKind::Tv, 1396, 2, &hash),
            format!(
                "https://kino.example.com/?open=tv_1396&play=1&season=2&release={}",
                "ab".repeat(20)
            )
        );
        assert_eq!(
            preload_ready_open_url("https://kino.example.com", MediaKind::Tv, 1396, 0, &hash),
            format!(
                "https://kino.example.com/?open=tv_1396&play=1&release={}",
                "ab".repeat(20)
            )
        );
        let ep_msg = format_preload_ready_episode_message(
            MediaKind::Tv,
            1,
            8,
            Some("Аутсорс"),
            None,
            Some("1080p"),
            Some("HDRezka"),
            Some(2_100_000_000),
        );
        assert_eq!(
            ep_msg,
            "✅ <b>Серия S01E08 сериала «Аутсорс» скачалась</b>\n1080p · HDRezka · 2.0 ГБ — готова к просмотру с сервера."
        );
        assert_eq!(
            preload_ready_open_url_with_episode("https://kino.example.com", MediaKind::Tv, 250703, 1, 8, &hash),
            format!(
                "https://kino.example.com/?open=tv_250703&play=1&season=1&episode=8&release={}",
                "ab".repeat(20)
            )
        );
    }
}
