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

/// Removes preloads past their removal time, with their files once nobody
/// else keeps the same release.
pub async fn expire_preloads(state: &AppState) -> anyhow::Result<usize> {
    let due = userdata::preloads_due(&state.pool).await?;
    let mut removed = 0usize;
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

/// Copies download progress and live telemetry from qBittorrent into the preload records.
pub async fn sync_preloads(state: &AppState) -> anyhow::Result<()> {
    let Some(qbit) = &state.media.qbit else { return Ok(()) };
    let hashes = userdata::preloads_active(&state.pool).await?;
    if hashes.is_empty() {
        state.media.preload_telemetry.write().await.clear();
        return Ok(());
    }
    let infos = qbit.info(&hashes).await?;
    let mut telemetry_updates = HashMap::with_capacity(hashes.len());
    for hash in &hashes {
        let (st, progress, size, speed_bps, dl_phase) = match infos.iter().find(|i| i.hash.eq_ignore_ascii_case(hash)) {
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
        if matches!(st, PreloadState::Queued | PreloadState::Downloading) {
            telemetry_updates.insert(hash.to_ascii_lowercase(), (speed_bps, dl_phase));
        }
        let transitioned = userdata::preload_progress(&state.pool, hash, st, progress, size).await?;
        if st == PreloadState::Ready && !transitioned.is_empty() {
            #[derive(Default)]
            struct ReadyGroup {
                users: Vec<i64>,
                raw_title: Option<String>,
            }
            let mut by_title: HashMap<(MediaKind, i64, i32), ReadyGroup> = HashMap::new();
            for (uid, kind, tmdb_id, season, title) in transitioned {
                let entry = by_title.entry((kind, tmdb_id, season)).or_default();
                entry.users.push(uid);
                if entry.raw_title.is_none() {
                    entry.raw_title = title;
                }
            }
            for ((kind, tmdb_id, season), grp) in by_title {
                let parsed = grp.raw_title.as_deref().map(crate::torrent::title::parse_title);
                let quality = parsed.as_ref().map(|p| p.quality.as_str());
                let voice = parsed.as_ref().map(|p| p.voice.as_str()).filter(|v| !v.is_empty());
                state.sync.notify_preload_ready(
                    &grp.users,
                    kind,
                    tmdb_id,
                    season,
                    hash,
                    grp.raw_title.as_deref(),
                    quality,
                    voice,
                );
            }
        }
    }
    *state.media.preload_telemetry.write().await = telemetry_updates;
    Ok(())
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
    let base = api_base.trim_end_matches('/');
    let resp = client
        .post(format!("{base}/bot{token}/sendMessage"))
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "reply_markup": { "inline_keyboard": [[{ "text": "Смотреть", "web_app": { "url": open_url } }]] },
        }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Telegram request failed: {}", e.without_url()))?;
    anyhow::ensure!(resp.status().is_success(), "Telegram answered {}", resp.status());
    Ok(())
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
