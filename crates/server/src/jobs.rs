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
                tg_users: Vec<(i64, i64)>,
                raw_title: Option<String>,
                size_bytes: Option<i64>,
            }
            let mut by_title: HashMap<(MediaKind, i64, i32), ReadyGroup> = HashMap::new();
            for t in transitioned {
                let entry = by_title.entry((t.kind, t.tmdb_id, t.season)).or_default();
                if !entry.users.contains(&t.user_id) {
                    entry.users.push(t.user_id);
                }
                if !entry.tg_users.iter().any(|&(uid, _)| uid == t.user_id) {
                    entry.tg_users.push((t.user_id, t.telegram_id));
                }
                if entry.raw_title.is_none() {
                    entry.raw_title = t.title;
                }
                if entry.size_bytes.is_none() {
                    entry.size_bytes = t.size_bytes;
                }
            }
            for ((kind, tmdb_id, season), grp) in by_title {
                let parsed = grp.raw_title.as_deref().map(crate::torrent::title::parse_title);
                let quality = parsed.as_ref().map(|p| p.quality.clone());
                let voice = parsed.as_ref().map(|p| p.voice.clone()).filter(|v| !v.is_empty());
                state.sync.notify_preload_ready(
                    &grp.users,
                    kind,
                    tmdb_id,
                    season,
                    hash,
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
                        infohash: hash.to_ascii_lowercase(),
                        raw_title: grp.raw_title,
                        quality,
                        voice,
                        size_bytes: grp.size_bytes,
                    },
                );
            }
        }
    }
    *state.media.preload_telemetry.write().await = telemetry_updates;
    Ok(())
}

#[derive(Debug, Clone)]
struct PreloadReadyTelegramJob {
    recipients: Vec<(i64, i64)>,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
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
    if bytes <= 0 {
        return None;
    }
    let gb = bytes as f64 / 1e9;
    Some(if gb >= 100.0 {
        format!("{gb:.0} ГБ")
    } else if gb >= 1.0 {
        format!("{gb:.1} ГБ")
    } else {
        format!("{:.0} МБ", (bytes as f64 / 1e6).max(1.0))
    })
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
    let clean_title = catalog_title.map(str::trim).filter(|s| !s.is_empty());
    let mut text = match (kind, season, clean_title) {
        (MediaKind::Movie, _, Some(name)) => format!("✅ <b>Фильм «{}» скачался</b>", escape_html(name)),
        (MediaKind::Movie, _, None) => "✅ <b>Фильм скачался</b>".to_owned(),
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
        text.push_str("\nГотов к просмотру с сервера.");
    } else {
        text.push_str(&format!(
            "\n{} — готов к просмотру с сервера.",
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

fn preload_ready_open_url(public_url: &str, kind: MediaKind, tmdb_id: i64, season: i32, infohash: &str) -> String {
    let base_url = public_url.trim_end_matches('/');
    let hash = infohash.to_ascii_lowercase();
    if kind == MediaKind::Tv && season > 0 {
        format!("{base_url}/?open=tv_{tmdb_id}&play=1&season={season}&release={hash}")
    } else {
        format!("{base_url}/?open={}_{tmdb_id}&play=1&release={hash}", kind.as_str())
    }
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
    let text = format_preload_ready_message(
        job.kind,
        job.season,
        catalog_title.as_deref(),
        job.raw_title.as_deref(),
        job.quality.as_deref(),
        job.voice.as_deref(),
        job.size_bytes,
    );
    let open_url = preload_ready_open_url(
        &state.config.public_url,
        job.kind,
        job.tmdb_id,
        job.season,
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
            "✅ <b>Фильм «Интерстеллар» скачался</b>\n4K · Дубляж · 28.4 ГБ — готов к просмотру с сервера."
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
            "✅ <b>Сезон 2 сериала «Во все тяжкие &amp; Co &lt;test&gt;» скачался</b>\n1080p · LostFilm · 18.7 ГБ — готов к просмотру с сервера."
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
            "✅ <b>Фильм скачался</b>\n<i>Raw &lt;Release&gt; &amp; Name</i>\n720p · 500 МБ — готов к просмотру с сервера."
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
    }
}
