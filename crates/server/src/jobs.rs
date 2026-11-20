//! Background jobs: new-episode notifications through the Telegram bot and
//! removal of preloads after they were watched.

use std::{future::Future, sync::Arc, time::Duration};

use kino_core::{episode_code, MediaKind, PreloadState, TitleStatus};

use crate::{api::AppState, userdata};

/// Checks followed series for new episodes and tells each user once.
///
/// The first check of a series only records the latest episode, so nobody gets
/// a message for episodes that were out before they started watching.
/// A watched series with a new episode goes back to "Смотрю".
pub async fn check_new_episodes<F, Fut>(state: &AppState, send: F) -> anyhow::Result<usize>
where
    F: Fn(i64, String) -> Fut,
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
                    "Вышла новая серия «{}»: {}.\n{}",
                    d.summary.title,
                    episode_code(latest.0, latest.1),
                    state.config.public_url
                );
                if let Err(e) = send(f.telegram_id, text).await {
                    tracing::warn!(error = %e, user = f.user_id, "new-episode message failed");
                    continue;
                }
                sent += 1;
                if f.status == TitleStatus::Watched {
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

/// Removes preloads past their removal time, with their files once nobody
/// else keeps the same release.
pub async fn expire_preloads(state: &AppState) -> anyhow::Result<usize> {
    let due = userdata::preloads_due(&state.pool).await?;
    for (id, infohash) in &due {
        userdata::preload_mark_deleted(&state.pool, *id).await?;
        remove_files(state, infohash).await;
    }
    Ok(due.len())
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

/// Copies download progress from qBittorrent into the preload records.
pub async fn sync_preloads(state: &AppState) -> anyhow::Result<()> {
    let Some(qbit) = &state.media.qbit else { return Ok(()) };
    let hashes = userdata::preloads_active(&state.pool).await?;
    if hashes.is_empty() {
        return Ok(());
    }
    let infos = qbit.info(&hashes).await?;
    for hash in &hashes {
        let (st, progress, size) = match infos.iter().find(|i| i.hash.eq_ignore_ascii_case(hash)) {
            None => (PreloadState::Failed, 0.0, None),
            Some(i) if i.failed() => (PreloadState::Failed, i.progress, Some(i.size)),
            Some(i) if i.done() => (PreloadState::Ready, 1.0, Some(i.size)),
            Some(i) => (PreloadState::Downloading, i.progress, Some(i.size)),
        };
        userdata::preload_progress(&state.pool, hash, st, progress, size).await?;
    }
    Ok(())
}

/// Sends a message through the Telegram Bot API.
pub async fn telegram_send(client: &reqwest::Client, token: &str, chat_id: i64, text: String) -> anyhow::Result<()> {
    let resp = client
        .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
        .json(&serde_json::json!({ "chat_id": chat_id, "text": text }))
        .send()
        .await?;
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
    let sync_state = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
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
            since_check += Duration::from_secs(3600);
            if since_check >= every {
                since_check = Duration::ZERO;
                let send = |chat, text| {
                    let (client, token) = (client.clone(), token.clone());
                    async move { telegram_send(&client, &token, chat, text).await }
                };
                match check_new_episodes(&state, send).await {
                    Ok(n) if n > 0 => tracing::info!(sent = n, "new-episode notifications"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "new-episode check failed"),
                }
            }
        }
    });
}
