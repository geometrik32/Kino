//! Per-user state beyond playback positions: title status and rating, player
//! tracks, manual "watched" marks, viewing history, preferences, preloads.

use chrono::{DateTime, Utc};
use kino_core::{
    HistoryEntry, MarkWatched, MediaKind, PreloadItem, PreloadRequest, PreloadState, TitleState, TitleStatus, UserPrefs,
};
use sqlx::{postgres::PgRow, PgPool, Row};

use crate::{catalog::Catalog, db};

fn decode_err(what: &str, value: &str) -> sqlx::Error {
    sqlx::Error::Decode(format!("bad {what} {value}").into())
}

fn kind_of(row: &PgRow) -> Result<MediaKind, sqlx::Error> {
    let k: String = row.try_get("media_kind")?;
    MediaKind::parse(&k).ok_or_else(|| decode_err("media_kind", &k))
}

const STATE_COLUMNS: &str = "media_kind, tmdb_id, status, rating, audio_track, subtitle_track, updated_at";

fn state_from_row(row: &PgRow) -> Result<TitleState, sqlx::Error> {
    let status: Option<String> = row.try_get("status")?;
    Ok(TitleState {
        kind: kind_of(row)?,
        tmdb_id: row.try_get("tmdb_id")?,
        status: match status {
            Some(s) => Some(TitleStatus::parse(&s).ok_or_else(|| decode_err("status", &s))?),
            None => None,
        },
        rating: row.try_get("rating")?,
        audio_track: row.try_get("audio_track")?,
        subtitle_track: row.try_get("subtitle_track")?,
        updated_at: row.try_get("updated_at")?,
        title: None,
    })
}

/// The state of one title; an empty one if the user never touched it.
pub async fn get_state(pool: &PgPool, user_id: i64, kind: MediaKind, tmdb_id: i64) -> Result<TitleState, sqlx::Error> {
    let row = sqlx::query(&format!(
        "SELECT {STATE_COLUMNS} FROM title_state WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3"
    ))
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .fetch_optional(pool)
    .await?;
    match row {
        Some(r) => state_from_row(&r),
        None => Ok(TitleState {
            kind,
            tmdb_id,
            status: None,
            rating: None,
            audio_track: None,
            subtitle_track: None,
            updated_at: Utc::now(),
            title: None,
        }),
    }
}

/// Sets one column of a title's state, creating the row if needed.
async fn set_column<T>(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    sets: &str,
    bind: T,
) -> Result<TitleState, sqlx::Error>
where
    T: for<'q> sqlx::Encode<'q, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send,
{
    sqlx::query(
        "INSERT INTO title_state (user_id, media_kind, tmdb_id) VALUES ($1, $2, $3)
         ON CONFLICT (user_id, media_kind, tmdb_id) DO NOTHING",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .execute(pool)
    .await?;
    let row = sqlx::query(&format!(
        "UPDATE title_state SET {sets}, updated_at = now()
         WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3 RETURNING {STATE_COLUMNS}"
    ))
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(bind)
    .fetch_one(pool)
    .await?;
    state_from_row(&row)
}

pub async fn set_status(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    status: Option<TitleStatus>,
) -> Result<TitleState, sqlx::Error> {
    set_column(
        pool,
        user_id,
        kind,
        tmdb_id,
        "status = $4",
        status.map(TitleStatus::as_str),
    )
    .await
}

pub async fn set_rating(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    rating: Option<i16>,
) -> Result<TitleState, sqlx::Error> {
    set_column(pool, user_id, kind, tmdb_id, "rating = $4", rating).await
}

pub async fn set_tracks(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    audio: Option<String>,
    subtitles: Option<String>,
) -> Result<TitleState, sqlx::Error> {
    // Both values in one array bind, so `set_column` stays generic.
    let packed = vec![audio, subtitles];
    set_column(
        pool,
        user_id,
        kind,
        tmdb_id,
        "audio_track = $4[1], subtitle_track = $4[2]",
        packed,
    )
    .await
}

/// Titles with a status (all of them, or one status), newest first.
pub async fn list_states(
    pool: &PgPool,
    user_id: i64,
    status: Option<TitleStatus>,
) -> Result<Vec<TitleState>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT {STATE_COLUMNS} FROM title_state
         WHERE user_id = $1 AND status IS NOT NULL AND ($2::text IS NULL OR status = $2)
         ORDER BY updated_at DESC LIMIT 500"
    ))
    .bind(user_id)
    .bind(status.map(TitleStatus::as_str))
    .fetch_all(pool)
    .await?;
    rows.iter().map(state_from_row).collect()
}

/// Aggregated per-title interaction signal used for home feed personalization.
#[derive(Debug, Clone)]
pub struct UserTitleSignal {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub status: Option<TitleStatus>,
    pub rating: Option<i16>,
    pub finished: bool,
}

/// Recent titles the user watched, rated or saved, newest interaction first.
pub async fn taste_signals(pool: &PgPool, user_id: i64) -> Result<Vec<UserTitleSignal>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT media_kind, tmdb_id,
                MAX(status) AS status,
                MAX(rating) AS rating,
                BOOL_OR(finished) AS finished,
                MAX(updated_at) AS last_at
         FROM (
             SELECT media_kind, tmdb_id, status, rating, FALSE AS finished, updated_at
             FROM title_state
             WHERE user_id = $1
             UNION ALL
             SELECT media_kind, tmdb_id, NULL::text AS status, NULL::smallint AS rating, finished, updated_at
             FROM watch_progress
             WHERE user_id = $1
         ) s
         GROUP BY media_kind, tmdb_id
         ORDER BY last_at DESC
         LIMIT 120",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let status: Option<String> = row.try_get("status")?;
            Ok(UserTitleSignal {
                kind: kind_of(row)?,
                tmdb_id: row.try_get("tmdb_id")?,
                status: match status {
                    Some(s) => Some(TitleStatus::parse(&s).ok_or_else(|| decode_err("status", &s))?),
                    None => None,
                },
                rating: row.try_get("rating")?,
                finished: row.try_get::<Option<bool>, _>("finished")?.unwrap_or(false),
            })
        })
        .collect()
}

/// Marks episodes (or a film) watched, or clears them.
pub async fn mark(pool: &PgPool, user_id: i64, m: &MarkWatched) -> Result<(), sqlx::Error> {
    let targets: Vec<(i32, i32)> = match m.kind {
        MediaKind::Movie => vec![(0, 0)],
        MediaKind::Tv => m.episodes.iter().map(|e| (m.season, *e)).collect(),
    };
    let mut tx = pool.begin().await?;
    for (season, episode) in targets {
        if m.watched {
            sqlx::query(
                "INSERT INTO watch_progress
                   (user_id, media_kind, tmdb_id, season, episode, position_sec, duration_sec, finished)
                 VALUES ($1, $2, $3, $4, $5, 0, 0, TRUE)
                 ON CONFLICT (user_id, media_kind, tmdb_id, season, episode) DO UPDATE SET
                   finished = TRUE, position_sec = watch_progress.duration_sec, updated_at = now()",
            )
        } else {
            sqlx::query(
                "DELETE FROM watch_progress
                 WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3 AND season = $4 AND episode = $5",
            )
        }
        .bind(user_id)
        .bind(m.kind.as_str())
        .bind(m.tmdb_id)
        .bind(season)
        .bind(episode)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Recomputes the automatic status after playback (`played`) or manual marks.
///
/// - anything watched or started → "Смотрю" ("Хочу посмотреть" is replaced);
/// - the film, or the latest aired episode, finished → "Просмотрено";
/// - "Брошено" stays unless the user starts playing again;
/// - nothing watched any more → an automatic status is cleared.
pub async fn refresh_status(
    pool: &PgPool,
    catalog: &dyn Catalog,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    played: bool,
) -> Result<Option<TitleStatus>, sqlx::Error> {
    let current = get_state(pool, user_id, kind, tmdb_id).await?.status;
    let progress = db::title_progress(pool, user_id, kind, tmdb_id).await?;
    let done = match kind {
        MediaKind::Movie => progress.iter().any(|p| p.finished),
        MediaKind::Tv => match catalog.details(kind, tmdb_id).await.ok().and_then(|d| d.last_episode) {
            Some(last) => progress.iter().any(|p| p.finished && (p.season, p.episode) >= last),
            None => false,
        },
    };
    let derived = if done {
        Some(TitleStatus::Watched)
    } else if progress.is_empty() {
        None
    } else {
        Some(TitleStatus::Watching)
    };
    let next = match (current, derived) {
        (Some(TitleStatus::Dropped), _) if !played => current,
        (Some(TitleStatus::Want), None) => current,
        (Some(TitleStatus::Dropped), None) => current,
        _ => derived,
    };
    if next != current {
        set_status(pool, user_id, kind, tmdb_id, next).await?;
    }
    Ok(next)
}

pub async fn history(pool: &PgPool, user_id: i64, limit: i64) -> Result<Vec<HistoryEntry>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT media_kind, tmdb_id, season, episode, started_at, last_at, position_from, position_to, device
         FROM watch_history WHERE user_id = $1 ORDER BY last_at DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(HistoryEntry {
                kind: kind_of(row)?,
                tmdb_id: row.try_get("tmdb_id")?,
                season: row.try_get("season")?,
                episode: row.try_get("episode")?,
                started_at: row.try_get("started_at")?,
                last_at: row.try_get("last_at")?,
                position_from: row.try_get("position_from")?,
                position_to: row.try_get("position_to")?,
                device: row.try_get("device")?,
                title: None,
            })
        })
        .collect()
}

pub async fn get_prefs(pool: &PgPool, user_id: i64) -> Result<UserPrefs, sqlx::Error> {
    let row = sqlx::query(
        "SELECT quality, voice, subtitle_lang, language, hdr, dolby_vision, subtitles, codec, direct_play,
                min_seeders, max_size_gb, sort
         FROM user_prefs WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some(r) => {
            let count = |col: &str| -> Result<Option<u32>, sqlx::Error> {
                Ok(r.try_get::<Option<i32>, _>(col)?.and_then(|n| u32::try_from(n).ok()))
            };
            UserPrefs {
                quality: r.try_get("quality")?,
                voice: r.try_get("voice")?,
                subtitle_lang: r.try_get("subtitle_lang")?,
                language: r.try_get("language")?,
                hdr: r.try_get("hdr")?,
                dolby_vision: r.try_get("dolby_vision")?,
                subtitles: r.try_get("subtitles")?,
                codec: r.try_get("codec")?,
                direct_play: r.try_get("direct_play")?,
                min_seeders: count("min_seeders")?,
                max_size_gb: count("max_size_gb")?,
                sort: r.try_get("sort")?,
            }
        }
        None => UserPrefs::default(),
    })
}

pub async fn set_prefs(pool: &PgPool, user_id: i64, p: &UserPrefs) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_prefs (user_id, quality, voice, subtitle_lang, language, hdr, dolby_vision, subtitles,
                                 codec, direct_play, min_seeders, max_size_gb, sort)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         ON CONFLICT (user_id) DO UPDATE SET
           quality = EXCLUDED.quality, voice = EXCLUDED.voice, subtitle_lang = EXCLUDED.subtitle_lang,
           language = EXCLUDED.language, hdr = EXCLUDED.hdr, dolby_vision = EXCLUDED.dolby_vision,
           subtitles = EXCLUDED.subtitles, codec = EXCLUDED.codec, direct_play = EXCLUDED.direct_play,
           min_seeders = EXCLUDED.min_seeders, max_size_gb = EXCLUDED.max_size_gb, sort = EXCLUDED.sort",
    )
    .bind(user_id)
    .bind(p.quality.as_deref())
    .bind(p.voice.as_deref())
    .bind(p.subtitle_lang.as_deref())
    .bind(p.language.as_deref())
    .bind(p.hdr)
    .bind(p.dolby_vision)
    .bind(p.subtitles)
    .bind(p.codec.as_deref())
    .bind(p.direct_play)
    .bind(p.min_seeders.map(|n| n.min(i32::MAX as u32) as i32))
    .bind(p.max_size_gb.map(|n| n.min(i32::MAX as u32) as i32))
    .bind(p.sort.as_deref())
    .execute(pool)
    .await?;
    Ok(())
}

const PRELOAD_COLUMNS: &str = "id, media_kind, tmdb_id, season, infohash, title, state, progress, size_bytes,
     requested_at, ready_at, delete_after";

fn preload_from_row(row: &PgRow) -> Result<PreloadItem, sqlx::Error> {
    let state: String = row.try_get("state")?;
    Ok(PreloadItem {
        id: row.try_get("id")?,
        kind: kind_of(row)?,
        tmdb_id: row.try_get("tmdb_id")?,
        season: row.try_get("season")?,
        infohash: row.try_get("infohash")?,
        title: row.try_get("title")?,
        state: PreloadState::parse(&state).ok_or_else(|| decode_err("state", &state))?,
        progress: row.try_get("progress")?,
        size_bytes: row.try_get("size_bytes")?,
        requested_at: row.try_get("requested_at")?,
        ready_at: row.try_get("ready_at")?,
        delete_after: row.try_get("delete_after")?,
    })
}

/// Queues a preload; asking again for a deleted or failed one re-queues it.
pub async fn preload_add(pool: &PgPool, user_id: i64, r: &PreloadRequest) -> Result<PreloadItem, sqlx::Error> {
    let row = sqlx::query(&format!(
        "INSERT INTO preload (user_id, media_kind, tmdb_id, season, infohash, title) VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (user_id, infohash) DO UPDATE SET
           state = CASE WHEN preload.state IN ('deleted', 'failed') THEN 'queued' ELSE preload.state END,
           progress = CASE WHEN preload.state IN ('deleted', 'failed') THEN 0 ELSE preload.progress END,
           delete_after = NULL, requested_at = now()
         RETURNING {PRELOAD_COLUMNS}"
    ))
    .bind(user_id)
    .bind(r.kind.as_str())
    .bind(r.tmdb_id)
    .bind(r.season)
    .bind(&r.infohash)
    .bind(r.title.as_deref())
    .fetch_one(pool)
    .await?;
    preload_from_row(&row)
}

pub async fn preloads(pool: &PgPool, user_id: i64) -> Result<Vec<PreloadItem>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT {PRELOAD_COLUMNS} FROM preload WHERE user_id = $1 AND state <> 'deleted'
         ORDER BY requested_at DESC"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(preload_from_row).collect()
}

/// Removes a preload by hand. Returns its info-hash, or `None` if there was none.
pub async fn preload_delete(pool: &PgPool, user_id: i64, id: i64) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE preload SET state = 'deleted', delete_after = NULL
         WHERE user_id = $1 AND id = $2 AND state <> 'deleted' RETURNING infohash",
    )
    .bind(user_id)
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Once a film or a season is watched, its preloads are scheduled for removal.
pub async fn preload_schedule_removal(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    at: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE preload SET delete_after = $5
         WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3 AND season = $4
           AND state <> 'deleted' AND delete_after IS NULL",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(season)
    .bind(at)
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// Preloads whose time is up. The caller removes the files, then calls
/// [`preload_mark_deleted`].
pub async fn preloads_due(pool: &PgPool) -> Result<Vec<(i64, String)>, sqlx::Error> {
    let rows = sqlx::query("SELECT id, infohash FROM preload WHERE state <> 'deleted' AND delete_after < now()")
        .fetch_all(pool)
        .await?;
    rows.iter()
        .map(|r| Ok((r.try_get("id")?, r.try_get("infohash")?)))
        .collect()
}

/// Info-hashes of preloads still downloading.
pub async fn preloads_active(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT DISTINCT infohash FROM preload WHERE state IN ('queued', 'downloading')")
        .fetch_all(pool)
        .await
}

/// Progress from the torrent client, for every user who asked for this release.
pub async fn preload_progress(
    pool: &PgPool,
    infohash: &str,
    state: PreloadState,
    progress: f32,
    size_bytes: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE preload SET state = $2, progress = $3, size_bytes = COALESCE($4, size_bytes),
             ready_at = CASE WHEN $2 = 'ready' THEN COALESCE(ready_at, now()) ELSE ready_at END
         WHERE infohash = $1 AND state IN ('queued', 'downloading')",
    )
    .bind(infohash)
    .bind(state.as_str())
    .bind(progress)
    .bind(size_bytes)
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether someone else still keeps this release on the server.
pub async fn preload_in_use(pool: &PgPool, infohash: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM preload WHERE infohash = $1 AND state <> 'deleted')")
        .bind(infohash)
        .fetch_one(pool)
        .await
}

pub async fn preload_mark_deleted(pool: &PgPool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE preload SET state = 'deleted' WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// A series someone is following, for the new-episode check.
pub struct Followed {
    pub user_id: i64,
    pub telegram_id: i64,
    pub tmdb_id: i64,
    pub status: TitleStatus,
    pub notified: Option<(i32, i32)>,
}

/// Series with status "Смотрю" or "Просмотрено" (a new season may come out).
pub async fn followed_series(pool: &PgPool) -> Result<Vec<Followed>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT s.user_id, u.telegram_id, s.tmdb_id, s.status, s.notified_season, s.notified_episode
         FROM title_state s JOIN users u ON u.id = s.user_id
         WHERE s.media_kind = 'tv' AND s.status IN ('watching', 'watched')",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|r| {
            let status: String = r.try_get("status")?;
            let s: Option<i32> = r.try_get("notified_season")?;
            let e: Option<i32> = r.try_get("notified_episode")?;
            Ok(Followed {
                user_id: r.try_get("user_id")?,
                telegram_id: r.try_get("telegram_id")?,
                tmdb_id: r.try_get("tmdb_id")?,
                status: TitleStatus::parse(&status).ok_or_else(|| decode_err("status", &status))?,
                notified: s.zip(e),
            })
        })
        .collect()
}

pub async fn set_notified(pool: &PgPool, user_id: i64, tmdb_id: i64, ep: (i32, i32)) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE title_state SET notified_season = $3, notified_episode = $4
         WHERE user_id = $1 AND media_kind = 'tv' AND tmdb_id = $2",
    )
    .bind(user_id)
    .bind(tmdb_id)
    .bind(ep.0)
    .bind(ep.1)
    .execute(pool)
    .await?;
    Ok(())
}
