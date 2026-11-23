//! Database access. Queries are checked at runtime (no live DB needed to build).

use chrono::{DateTime, Utc};
use kino_core::{
    is_finished, normalize_quality_key, ContinueItem, Me, MediaKind, Progress, ProgressUpdate, ReleaseChoice,
    ReleaseChoiceUpdate, ReleaseRef, Role, WatchlistItem,
};
use rand::{distr::Alphanumeric, Rng};
use sqlx::{PgPool, Row};

use crate::telegram_auth::TelegramUser;

pub type Pool = PgPool;

pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

fn me_from_row(row: &sqlx::postgres::PgRow) -> Result<Me, sqlx::Error> {
    let role: String = row.try_get("role")?;
    Ok(Me {
        id: row.try_get("id")?,
        telegram_id: row.try_get("telegram_id")?,
        first_name: row.try_get("first_name")?,
        username: row.try_get("username")?,
        role: if role == "admin" { Role::Admin } else { Role::User },
        avatar_url: None,
    })
}

const ME_COLUMNS: &str = "id, telegram_id, first_name, username, role";

pub async fn get_user(pool: &PgPool, id: i64) -> Result<Option<Me>, sqlx::Error> {
    let row = sqlx::query(&format!("SELECT {ME_COLUMNS} FROM users WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(me_from_row).transpose()
}

/// The account of a Telegram user, if they have logged in before.
pub async fn user_by_telegram(pool: &PgPool, telegram_id: i64) -> Result<Option<Me>, sqlx::Error> {
    let row = sqlx::query(&format!("SELECT {ME_COLUMNS} FROM users WHERE telegram_id = $1"))
        .bind(telegram_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(me_from_row).transpose()
}

pub struct AvatarMeta {
    pub telegram_id: i64,
    pub photo_url: Option<String>,
    pub avatar_hash: Option<String>,
    pub avatar_checked_at: Option<DateTime<Utc>>,
}

pub async fn avatar_meta(pool: &PgPool, user_id: i64) -> Result<Option<AvatarMeta>, sqlx::Error> {
    let row = sqlx::query("SELECT telegram_id, photo_url, avatar_hash, avatar_checked_at FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    row.map(|r| {
        Ok(AvatarMeta {
            telegram_id: r.try_get("telegram_id")?,
            photo_url: r.try_get("photo_url")?,
            avatar_hash: r.try_get("avatar_hash")?,
            avatar_checked_at: r.try_get("avatar_checked_at")?,
        })
    })
    .transpose()
}

pub async fn save_avatar(pool: &PgPool, user_id: i64, data: &[u8], mime: &str, hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE users SET avatar_data = $2, avatar_mime = $3, avatar_hash = $4, avatar_checked_at = now()
         WHERE id = $1",
    )
    .bind(user_id)
    .bind(data)
    .bind(mime)
    .bind(hash)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn clear_avatar(pool: &PgPool, user_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE users SET avatar_data = NULL, avatar_mime = NULL, avatar_hash = NULL, avatar_checked_at = now()
         WHERE id = $1",
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn avatar_bytes(
    pool: &PgPool,
    user_id: i64,
) -> Result<Option<(Vec<u8>, String, Option<String>)>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT avatar_data, avatar_mime, avatar_hash
         FROM users
         WHERE id = $1 AND avatar_data IS NOT NULL AND avatar_mime IS NOT NULL",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    row.map(|r| {
        Ok((
            r.try_get("avatar_data")?,
            r.try_get("avatar_mime")?,
            r.try_get("avatar_hash")?,
        ))
    })
    .transpose()
}

#[derive(Debug, PartialEq)]
pub enum LoginOutcome {
    LoggedIn(Me),
    /// Unknown user without a valid invite.
    NotInvited,
}

/// Logs a Telegram user in. Existing users get their profile refreshed; new
/// users need to be in `admins` or bring an unused invite code.
pub async fn login(
    pool: &PgPool,
    tg: &TelegramUser,
    invite: Option<&str>,
    admins: &[i64],
) -> Result<LoginOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let existing = sqlx::query(&format!(
        "UPDATE users SET username = $2, first_name = $3, last_name = $4,
                photo_url = COALESCE(NULLIF($5, ''), photo_url), last_seen_at = now()
         WHERE telegram_id = $1 RETURNING {ME_COLUMNS}"
    ))
    .bind(tg.id)
    .bind(&tg.username)
    .bind(&tg.first_name)
    .bind(&tg.last_name)
    .bind(&tg.photo_url)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = existing {
        let me = me_from_row(&row)?;
        tx.commit().await?;
        return Ok(LoginOutcome::LoggedIn(me));
    }

    let is_admin = admins.contains(&tg.id);
    let mut claimed_invite = None;
    if !is_admin {
        let Some(code) = invite else {
            return Ok(LoginOutcome::NotInvited);
        };
        // Claim the invite atomically so it can't be used twice.
        let claimed =
            sqlx::query("UPDATE invites SET used_at = now() WHERE code = $1 AND used_at IS NULL RETURNING code")
                .bind(code)
                .fetch_optional(&mut *tx)
                .await?;
        if claimed.is_none() {
            return Ok(LoginOutcome::NotInvited);
        }
        claimed_invite = Some(code);
    }

    let row = sqlx::query(&format!(
        "INSERT INTO users (telegram_id, username, first_name, last_name, photo_url, role)
         VALUES ($1, $2, $3, $4, NULLIF($5, ''), $6) RETURNING {ME_COLUMNS}"
    ))
    .bind(tg.id)
    .bind(&tg.username)
    .bind(&tg.first_name)
    .bind(&tg.last_name)
    .bind(&tg.photo_url)
    .bind(if is_admin { "admin" } else { "user" })
    .fetch_one(&mut *tx)
    .await?;
    let me = me_from_row(&row)?;
    if let Some(code) = claimed_invite {
        sqlx::query("UPDATE invites SET used_by = $2 WHERE code = $1")
            .bind(code)
            .bind(me.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(LoginOutcome::LoggedIn(me))
}

/// Creates a one-time invite code.
pub async fn create_invite(pool: &PgPool, created_by: Option<i64>) -> Result<String, sqlx::Error> {
    let code: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(12)
        .map(char::from)
        .collect();
    sqlx::query("INSERT INTO invites (code, created_by) VALUES ($1, $2)")
        .bind(&code)
        .bind(created_by)
        .execute(pool)
        .await?;
    Ok(code)
}

/// How long a "log in through Telegram" link stays valid.
pub const LOGIN_LINK_MINUTES: i32 = 10;

/// `/start` payload prefix for website logins: `t.me/<bot>?start=login_<token>`.
pub const LOGIN_PREFIX: &str = "login_";

/// Creates a login link token (43 URL-safe characters, fits a `/start` payload).
pub async fn create_login_link(pool: &PgPool, invite: Option<&str>) -> Result<String, sqlx::Error> {
    sqlx::query("DELETE FROM login_link WHERE created_at < now() - make_interval(mins => $1)")
        .bind(LOGIN_LINK_MINUTES * 6)
        .execute(pool)
        .await?;
    let token: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(43)
        .map(char::from)
        .collect();
    sqlx::query("INSERT INTO login_link (token, invite) VALUES ($1, $2)")
        .bind(&token)
        .bind(invite)
        .execute(pool)
        .await?;
    Ok(token)
}

/// Marks a login link as confirmed by this Telegram user. False when the link
/// is unknown, already used or too old.
pub async fn confirm_login_link(pool: &PgPool, token: &str, tg: &TelegramUser) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE login_link SET telegram_id = $2, first_name = $3, last_name = $4, username = $5, confirmed_at = now()
         WHERE token = $1 AND confirmed_at IS NULL AND created_at > now() - make_interval(mins => $6)",
    )
    .bind(token)
    .bind(tg.id)
    .bind(&tg.first_name)
    .bind(&tg.last_name)
    .bind(&tg.username)
    .bind(LOGIN_LINK_MINUTES)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub enum LoginLinkState {
    Pending,
    Expired,
    /// Confirmed: who it was and the invite the site passed. The link is deleted.
    Confirmed(TelegramUser, Option<String>),
}

/// Looks at a login link; a confirmed one is used up by this call.
pub async fn take_login_link(pool: &PgPool, token: &str) -> Result<LoginLinkState, sqlx::Error> {
    let row = sqlx::query(
        "SELECT telegram_id, first_name, last_name, username, invite,
                created_at > now() - make_interval(mins => $2) AS fresh
         FROM login_link WHERE token = $1",
    )
    .bind(token)
    .bind(LOGIN_LINK_MINUTES)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(LoginLinkState::Expired);
    };
    let telegram_id: Option<i64> = row.try_get("telegram_id")?;
    let fresh: bool = row.try_get("fresh")?;
    match telegram_id {
        Some(id) if fresh => {
            let deleted = sqlx::query("DELETE FROM login_link WHERE token = $1")
                .bind(token)
                .execute(pool)
                .await?;
            if deleted.rows_affected() != 1 {
                // Someone else picked it up at the same moment.
                return Ok(LoginLinkState::Expired);
            }
            let tg = TelegramUser {
                id,
                first_name: row.try_get("first_name")?,
                last_name: row.try_get("last_name")?,
                username: row.try_get("username")?,
                photo_url: None,
            };
            Ok(LoginLinkState::Confirmed(tg, row.try_get("invite")?))
        }
        Some(_) => {
            let _ = sqlx::query("DELETE FROM login_link WHERE token = $1")
                .bind(token)
                .execute(pool)
                .await;
            Ok(LoginLinkState::Expired)
        }
        None if fresh => Ok(LoginLinkState::Pending),
        None => Ok(LoginLinkState::Expired),
    }
}

const PROGRESS_COLUMNS: &str = "media_kind, tmdb_id, season, episode, position_sec, duration_sec, finished,
     release_infohash, release_file, release_title, updated_at";

fn progress_from_row(row: &sqlx::postgres::PgRow) -> Result<Progress, sqlx::Error> {
    let kind: String = row.try_get("media_kind")?;
    let infohash: Option<String> = row.try_get("release_infohash")?;
    let file: Option<i32> = row.try_get("release_file")?;
    Ok(Progress {
        kind: MediaKind::parse(&kind).ok_or_else(|| sqlx::Error::Decode(format!("bad media_kind {kind}").into()))?,
        tmdb_id: row.try_get("tmdb_id")?,
        season: row.try_get("season")?,
        episode: row.try_get("episode")?,
        position_sec: row.try_get("position_sec")?,
        duration_sec: row.try_get("duration_sec")?,
        finished: row.try_get("finished")?,
        release: match (infohash, file) {
            (Some(infohash), Some(file_index)) => Some(ReleaseRef {
                infohash,
                file_index,
                title: row.try_get("release_title")?,
            }),
            _ => None,
        },
        updated_at: row.try_get::<DateTime<Utc>, _>("updated_at")?,
    })
}

/// Saves the player position. A release given here replaces the stored one;
/// `None` keeps whatever was stored.
pub async fn save_progress(pool: &PgPool, user_id: i64, u: &ProgressUpdate) -> Result<Progress, sqlx::Error> {
    let (season, episode) = match u.kind {
        MediaKind::Movie => (0, 0),
        MediaKind::Tv => (u.season, u.episode),
    };
    let row = sqlx::query(&format!(
        "INSERT INTO watch_progress
           (user_id, media_kind, tmdb_id, season, episode, position_sec, duration_sec, finished,
            release_infohash, release_file, release_title, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now())
         ON CONFLICT (user_id, media_kind, tmdb_id, season, episode) DO UPDATE SET
           position_sec = EXCLUDED.position_sec,
           duration_sec = CASE WHEN EXCLUDED.duration_sec > 0 THEN EXCLUDED.duration_sec ELSE watch_progress.duration_sec END,
           finished = watch_progress.finished OR EXCLUDED.finished,
           release_infohash = COALESCE(EXCLUDED.release_infohash, watch_progress.release_infohash),
           release_file = COALESCE(EXCLUDED.release_file, watch_progress.release_file),
           release_title = CASE WHEN EXCLUDED.release_infohash IS NULL
                                THEN watch_progress.release_title ELSE EXCLUDED.release_title END,
           updated_at = now()
         RETURNING {PROGRESS_COLUMNS}"
    ))
    .bind(user_id)
    .bind(u.kind.as_str())
    .bind(u.tmdb_id)
    .bind(season)
    .bind(episode)
    .bind(u.position_sec)
    .bind(u.duration_sec)
    .bind(is_finished(u.position_sec, u.duration_sec))
    .bind(u.release.as_ref().map(|r| r.infohash.as_str()))
    .bind(u.release.as_ref().map(|r| r.file_index))
    .bind(u.release.as_ref().and_then(|r| r.title.as_deref()))
    .fetch_one(pool)
    .await?;
    record_history(pool, user_id, u, season, episode).await?;
    progress_from_row(&row)
}

/// Extends the current viewing session, or starts a new one after a 30-minute break.
async fn record_history(
    pool: &PgPool,
    user_id: i64,
    u: &ProgressUpdate,
    season: i32,
    episode: i32,
) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(
        "UPDATE watch_history SET last_at = now(), position_to = $6, device = COALESCE($7, device)
         WHERE id = (SELECT id FROM watch_history
                     WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3 AND season = $4 AND episode = $5
                       AND last_at > now() - interval '30 minutes'
                     ORDER BY last_at DESC LIMIT 1)",
    )
    .bind(user_id)
    .bind(u.kind.as_str())
    .bind(u.tmdb_id)
    .bind(season)
    .bind(episode)
    .bind(u.position_sec)
    .bind(u.device.as_deref())
    .execute(pool)
    .await?;
    if updated.rows_affected() == 0 {
        sqlx::query(
            "INSERT INTO watch_history
               (user_id, media_kind, tmdb_id, season, episode, position_from, position_to, device)
             VALUES ($1, $2, $3, $4, $5, $6, $6, $7)",
        )
        .bind(user_id)
        .bind(u.kind.as_str())
        .bind(u.tmdb_id)
        .bind(season)
        .bind(episode)
        .bind(u.position_sec)
        .bind(u.device.as_deref())
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// All saved positions for one title (every episode of a series).
pub async fn title_progress(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
) -> Result<Vec<Progress>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT {PROGRESS_COLUMNS} FROM watch_progress
         WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3 ORDER BY season, episode"
    ))
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(progress_from_row).collect()
}

/// A title not touched for this long is offered to be marked "Брошено".
pub const STALE_AFTER: chrono::Duration = chrono::Duration::days(60);

/// "Continue watching": the latest position per title, newest first.
/// Finished films drop out; a finished episode points at the next one.
pub async fn continue_watching(pool: &PgPool, user_id: i64, limit: i64) -> Result<Vec<ContinueItem>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT * FROM (
           SELECT DISTINCT ON (media_kind, tmdb_id) {PROGRESS_COLUMNS}
           FROM watch_progress WHERE user_id = $1
           ORDER BY media_kind, tmdb_id, updated_at DESC
         ) latest
         WHERE NOT (media_kind = 'movie' AND finished)
           AND NOT EXISTS (
             SELECT 1 FROM title_state ts
             WHERE ts.user_id = $1 AND ts.media_kind = latest.media_kind AND ts.tmdb_id = latest.tmdb_id
               AND ts.status IN ('dropped', 'watched', 'waiting_episode', 'waiting_release'))
         ORDER BY updated_at DESC LIMIT $2"
    ))
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let last = progress_from_row(row)?;
            let stale = last.updated_at < Utc::now() - STALE_AFTER;
            let next_episode = (last.kind == MediaKind::Tv && last.finished).then_some((last.season, last.episode + 1));
            Ok(ContinueItem {
                last,
                next_episode,
                title: None,
                stale,
            })
        })
        .collect()
}

/// "Хочу посмотреть": titles with status `want`.
pub async fn watchlist(pool: &PgPool, user_id: i64) -> Result<Vec<WatchlistItem>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT media_kind, tmdb_id, updated_at FROM title_state
         WHERE user_id = $1 AND status = 'want' ORDER BY updated_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let kind: String = row.try_get("media_kind")?;
            Ok(WatchlistItem {
                kind: MediaKind::parse(&kind)
                    .ok_or_else(|| sqlx::Error::Decode(format!("bad media_kind {kind}").into()))?,
                tmdb_id: row.try_get("tmdb_id")?,
                added_at: row.try_get("updated_at")?,
            })
        })
        .collect()
}

pub async fn watchlist_add(pool: &PgPool, user_id: i64, kind: MediaKind, tmdb_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO title_state (user_id, media_kind, tmdb_id, status) VALUES ($1, $2, $3, 'want')
         ON CONFLICT (user_id, media_kind, tmdb_id) DO UPDATE SET status = 'want', updated_at = now()
         WHERE title_state.status IS DISTINCT FROM 'want'",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn watchlist_remove(pool: &PgPool, user_id: i64, kind: MediaKind, tmdb_id: i64) -> Result<bool, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE title_state SET status = NULL, updated_at = now()
         WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3 AND status = 'want'",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() > 0)
}

fn quality_key_of(c: &ReleaseChoiceUpdate) -> String {
    c.quality_key
        .as_deref()
        .or(c.quality.as_deref())
        .map(normalize_quality_key)
        .unwrap_or("")
        .to_owned()
}

/// Remembers the release picked for a film, series, season or episode under its quality bucket.
pub async fn save_release_choice(
    pool: &PgPool,
    user_id: i64,
    c: &ReleaseChoiceUpdate,
) -> Result<ReleaseChoice, sqlx::Error> {
    let qkey = quality_key_of(c);
    let mut stored = c.clone();
    if stored.quality_key.is_none() && !qkey.is_empty() {
        stored.quality_key = Some(qkey.clone());
    }
    let chosen_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO release_choice
            (user_id, media_kind, tmdb_id, season, episode, quality_key, infohash, release_title, quality, voice, chosen_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())
         ON CONFLICT (user_id, media_kind, tmdb_id, season, episode, quality_key) DO UPDATE SET
           infohash = EXCLUDED.infohash, release_title = EXCLUDED.release_title,
           quality = EXCLUDED.quality, voice = EXCLUDED.voice, chosen_at = EXCLUDED.chosen_at
         RETURNING chosen_at",
    )
    .bind(user_id)
    .bind(c.kind.as_str())
    .bind(c.tmdb_id)
    .bind(c.season)
    .bind(c.episode)
    .bind(&qkey)
    .bind(&c.infohash)
    .bind(c.title.as_deref())
    .bind(c.quality.as_deref())
    .bind(c.voice.as_deref())
    .fetch_one(pool)
    .await?;
    Ok(ReleaseChoice {
        release: stored,
        chosen_at,
        picked_by: None,
    })
}

fn choice_from_row(
    row: &sqlx::postgres::PgRow,
    kind: MediaKind,
    tmdb_id: i64,
    picked_by: Option<String>,
) -> Result<ReleaseChoice, sqlx::Error> {
    let qkey: String = row.try_get("quality_key")?;
    Ok(ReleaseChoice {
        release: ReleaseChoiceUpdate {
            kind,
            tmdb_id,
            season: row.try_get("season")?,
            episode: row.try_get("episode")?,
            infohash: row.try_get("infohash")?,
            title: row.try_get("release_title")?,
            quality: row.try_get("quality")?,
            quality_key: (!qkey.is_empty()).then_some(qkey),
            voice: row.try_get("voice")?,
        },
        chosen_at: row.try_get("chosen_at")?,
        picked_by,
    })
}

/// All remembered releases for a title for one user (by season, episode and quality), newest first.
pub async fn release_choices(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
) -> Result<Vec<ReleaseChoice>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT season, episode, quality_key, infohash, release_title, quality, voice, chosen_at
         FROM release_choice
         WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3
         ORDER BY chosen_at DESC",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(|r| choice_from_row(r, kind, tmdb_id, None)).collect()
}

/// Release choices made by OTHER users for this title, newest first.
pub async fn shared_release_choices(
    pool: &PgPool,
    exclude_user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
) -> Result<Vec<ReleaseChoice>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT rc.season, rc.episode, rc.quality_key, rc.infohash, rc.release_title,
                rc.quality, rc.voice, rc.chosen_at,
                COALESCE(NULLIF(u.first_name, ''), NULLIF(u.username, ''), 'Участник') AS picker_name
         FROM release_choice rc
         JOIN users u ON u.id = rc.user_id
         WHERE rc.user_id <> $1 AND rc.media_kind = $2 AND rc.tmdb_id = $3
         ORDER BY rc.chosen_at DESC",
    )
    .bind(exclude_user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|r| {
            let who: String = r.try_get("picker_name")?;
            choice_from_row(r, kind, tmdb_id, Some(who))
        })
        .collect()
}

pub async fn save_device_quality(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    device_kind: &str,
    quality_key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO release_device_quality
            (user_id, media_kind, tmdb_id, season, episode, device_kind, quality_key, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, now())
         ON CONFLICT (user_id, media_kind, tmdb_id, season, episode, device_kind) DO UPDATE SET
           quality_key = EXCLUDED.quality_key, updated_at = EXCLUDED.updated_at",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(season)
    .bind(episode)
    .bind(device_kind)
    .bind(quality_key)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn device_quality_for(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    device_kind: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT quality_key FROM release_device_quality
         WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3 AND device_kind = $6
           AND ((season = $4 AND episode = $5) OR (season = $4 AND episode = 0) OR (season = 0 AND episode = 0))
         ORDER BY (episode = $5) DESC, (season = $4) DESC, updated_at DESC
         LIMIT 1",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(season)
    .bind(episode)
    .bind(device_kind)
    .fetch_optional(pool)
    .await
}
