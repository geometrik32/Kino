//! Per-user state beyond playback positions: title status and rating, player
//! tracks, manual "watched" marks, viewing history, preferences, preloads.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use kino_core::{
    meets_average_threshold, EpisodeRating, HistoryEntry, MarkWatched, MediaKind, PreloadEpisodeItem, PreloadItem,
    PreloadPhase, PreloadRequest, PreloadState, SeasonRating, TitleState, TitleStatus, UserPrefs, VideoScale,
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

const STATE_COLUMNS: &str =
    "media_kind, tmdb_id, status, rating, effective_rating, audio_track, subtitle_track, video_scale, updated_at";

fn state_from_row(row: &PgRow) -> Result<TitleState, sqlx::Error> {
    let status: Option<String> = row.try_get("status")?;
    let video_scale: Option<String> = row.try_get("video_scale")?;
    let rating: Option<i16> = row.try_get("rating")?;
    let effective_rating: Option<f32> = row.try_get("effective_rating")?;
    Ok(TitleState {
        kind: kind_of(row)?,
        tmdb_id: row.try_get("tmdb_id")?,
        status: match status {
            Some(s) => Some(TitleStatus::parse(&s).ok_or_else(|| decode_err("status", &s))?),
            None => None,
        },
        rating,
        effective_rating: effective_rating.or_else(|| rating.map(|r| r as f32)),
        season_ratings: Vec::new(),
        episode_ratings: Vec::new(),
        audio_track: row.try_get("audio_track")?,
        subtitle_track: row.try_get("subtitle_track")?,
        video_scale: match video_scale {
            Some(s) => Some(VideoScale::parse(&s).ok_or_else(|| decode_err("video_scale", &s))?),
            None => None,
        },
        updated_at: row.try_get("updated_at")?,
        title: None,
        peers: Vec::new(),
        preloaded_ready_at: None,
    })
}

async fn load_series_ratings(
    pool: &PgPool,
    user_id: i64,
    tmdb_id: i64,
) -> Result<(Vec<SeasonRating>, Vec<EpisodeRating>), sqlx::Error> {
    let s_rows = sqlx::query(
        "SELECT season, rating, effective_rating, aired_episodes
         FROM season_rating WHERE user_id = $1 AND tmdb_id = $2 ORDER BY season",
    )
    .bind(user_id)
    .bind(tmdb_id)
    .fetch_all(pool)
    .await?;
    let e_rows = sqlx::query(
        "SELECT season, episode, rating
         FROM episode_rating WHERE user_id = $1 AND tmdb_id = $2 ORDER BY season, episode",
    )
    .bind(user_id)
    .bind(tmdb_id)
    .fetch_all(pool)
    .await?;
    let episode_ratings: Vec<EpisodeRating> = e_rows
        .iter()
        .map(|r| {
            Ok(EpisodeRating {
                season: r.try_get("season")?,
                episode: r.try_get("episode")?,
                rating: r.try_get("rating")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()?;

    let season_ratings: Vec<SeasonRating> = s_rows
        .iter()
        .map(|r| {
            let season: i32 = r.try_get("season")?;
            let rated_episodes = episode_ratings.iter().filter(|e| e.season == season).count() as i32;
            Ok(SeasonRating {
                season,
                manual_rating: r.try_get("rating")?,
                effective_rating: r.try_get("effective_rating")?,
                rated_episodes,
                aired_episodes: r.try_get("aired_episodes")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()?;
    Ok((season_ratings, episode_ratings))
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
    let mut state = match row {
        Some(r) => state_from_row(&r)?,
        None => TitleState {
            kind,
            tmdb_id,
            status: None,
            rating: None,
            effective_rating: None,
            season_ratings: Vec::new(),
            episode_ratings: Vec::new(),
            audio_track: None,
            subtitle_track: None,
            video_scale: None,
            updated_at: Utc::now(),
            title: None,
            peers: Vec::new(),
            preloaded_ready_at: None,
        },
    };
    if kind == MediaKind::Tv {
        let (seasons, episodes) = load_series_ratings(pool, user_id, tmdb_id).await?;
        state.season_ratings = seasons;
        state.episode_ratings = episodes;
    }
    Ok(state)
}

/// Sets columns of a title's state on an open connection, creating the row if needed.
async fn set_column_on<T>(
    conn: &mut sqlx::PgConnection,
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
    .execute(&mut *conn)
    .await?;
    let row = sqlx::query(&format!(
        "UPDATE title_state SET {sets}, updated_at = now()
         WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3 RETURNING {STATE_COLUMNS}"
    ))
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(bind)
    .fetch_one(conn)
    .await?;
    state_from_row(&row)
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
    let mut conn = pool.acquire().await?;
    set_column_on(&mut conn, user_id, kind, tmdb_id, sets, bind).await
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

/// Recomputes `season_rating.effective_rating` and `title_state.effective_rating` using
/// the bottom-up arithmetic mean once at least 80% of aired episodes/seasons are rated.
async fn recompute_series_ratings_tx(
    conn: &mut sqlx::PgConnection,
    user_id: i64,
    tmdb_id: i64,
    season_aired: &[(i32, i32)],
) -> Result<(), sqlx::Error> {
    let ep_rows = sqlx::query("SELECT season, episode, rating FROM episode_rating WHERE user_id = $1 AND tmdb_id = $2")
        .bind(user_id)
        .bind(tmdb_id)
        .fetch_all(&mut *conn)
        .await?;
    let mut eps_by_season: std::collections::HashMap<i32, Vec<(i32, i16)>> = std::collections::HashMap::new();
    for r in ep_rows {
        let s: i32 = r.try_get("season")?;
        let e: i32 = r.try_get("episode")?;
        let rating: i16 = r.try_get("rating")?;
        if s > 0 && e >= 1 {
            eps_by_season.entry(s).or_default().push((e, rating));
        }
    }

    let s_rows =
        sqlx::query("SELECT season, rating, aired_episodes FROM season_rating WHERE user_id = $1 AND tmdb_id = $2")
            .bind(user_id)
            .bind(tmdb_id)
            .fetch_all(&mut *conn)
            .await?;
    let mut manual_seasons: std::collections::HashMap<i32, (Option<i16>, i32)> = std::collections::HashMap::new();
    for r in s_rows {
        let s: i32 = r.try_get("season")?;
        manual_seasons.insert(s, (r.try_get("rating")?, r.try_get("aired_episodes")?));
    }

    let mut aired_map: std::collections::BTreeMap<i32, i32> = std::collections::BTreeMap::new();
    for &(s, cnt) in season_aired {
        if s > 0 && cnt > 0 {
            aired_map.insert(s, cnt);
        }
    }
    for (&s, &(_, stored_aired)) in &manual_seasons {
        if s > 0 && !aired_map.contains_key(&s) && stored_aired > 0 {
            aired_map.insert(s, stored_aired);
        }
    }
    for &s in eps_by_season.keys() {
        aired_map.entry(s).or_insert(0);
    }

    let mut season_effectives: Vec<f32> = Vec::new();
    for (&s, &aired_eps) in &aired_map {
        let manual = manual_seasons.get(&s).and_then(|(m, _)| *m);
        let ep_list = eps_by_season.get(&s).map(Vec::as_slice).unwrap_or(&[]);
        let valid_eps: Vec<i16> = ep_list
            .iter()
            .filter(|(e, _)| aired_eps <= 0 || *e <= aired_eps)
            .map(|(_, r)| *r)
            .collect();
        let eff = if aired_eps > 0 && meets_average_threshold(valid_eps.len(), aired_eps as usize) {
            let sum: i32 = valid_eps.iter().map(|&r| i32::from(r)).sum();
            Some(sum as f32 / valid_eps.len() as f32)
        } else {
            manual.map(|r| r as f32)
        };
        if let Some(v) = eff {
            season_effectives.push(v);
        }
        if manual.is_some() || eff.is_some() || !valid_eps.is_empty() {
            sqlx::query(
                "INSERT INTO season_rating (user_id, tmdb_id, season, rating, effective_rating, aired_episodes, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, now())
                 ON CONFLICT (user_id, tmdb_id, season) DO UPDATE SET
                   effective_rating = EXCLUDED.effective_rating,
                   aired_episodes = GREATEST(season_rating.aired_episodes, EXCLUDED.aired_episodes),
                   updated_at = now()",
            )
            .bind(user_id)
            .bind(tmdb_id)
            .bind(s)
            .bind(manual)
            .bind(eff)
            .bind(aired_eps)
            .execute(&mut *conn)
            .await?;
        } else if manual_seasons.contains_key(&s) {
            sqlx::query("DELETE FROM season_rating WHERE user_id = $1 AND tmdb_id = $2 AND season = $3")
                .bind(user_id)
                .bind(tmdb_id)
                .bind(s)
                .execute(&mut *conn)
                .await?;
        }
    }

    let total_aired_seasons = aired_map.values().filter(|&&c| c > 0).count();
    let manual_series: Option<i16> =
        sqlx::query_scalar("SELECT rating FROM title_state WHERE user_id = $1 AND media_kind = 'tv' AND tmdb_id = $2")
            .bind(user_id)
            .bind(tmdb_id)
            .fetch_optional(&mut *conn)
            .await?
            .flatten();

    let series_eff = if total_aired_seasons > 0 && meets_average_threshold(season_effectives.len(), total_aired_seasons)
    {
        let sum: f32 = season_effectives.iter().sum();
        Some(sum / season_effectives.len() as f32)
    } else {
        manual_series.map(|r| r as f32)
    };

    sqlx::query(
        "INSERT INTO title_state (user_id, media_kind, tmdb_id, effective_rating)
         VALUES ($1, 'tv', $2, $3)
         ON CONFLICT (user_id, media_kind, tmdb_id) DO UPDATE SET
           effective_rating = EXCLUDED.effective_rating, updated_at = now()",
    )
    .bind(user_id)
    .bind(tmdb_id)
    .bind(series_eff)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

async fn derive_series_status_tx(
    conn: &mut sqlx::PgConnection,
    user_id: i64,
    tmdb_id: i64,
    all_aired_targets: &[(i32, i32)],
    ongoing: bool,
) -> Result<(), sqlx::Error> {
    let rows = sqlx::query(
        "SELECT season, episode, finished FROM watch_progress
         WHERE user_id = $1 AND media_kind = 'tv' AND tmdb_id = $2",
    )
    .bind(user_id)
    .bind(tmdb_id)
    .fetch_all(&mut *conn)
    .await?;
    let finished_set: std::collections::HashSet<(i32, i32)> = rows
        .iter()
        .filter_map(|r| {
            let fin: bool = r.try_get("finished").ok()?;
            fin.then(|| Some((r.try_get("season").ok()?, r.try_get("episode").ok()?)))
                .flatten()
        })
        .collect();
    let all_done = !all_aired_targets.is_empty() && all_aired_targets.iter().all(|ep| finished_set.contains(ep));
    let next_status = if all_done {
        Some(if ongoing {
            TitleStatus::WaitingEpisode
        } else {
            TitleStatus::Watched
        })
    } else if !rows.is_empty() {
        Some(TitleStatus::Watching)
    } else {
        None
    };
    if let Some(st) = next_status {
        set_column_on(conn, user_id, MediaKind::Tv, tmdb_id, "status = $4", Some(st.as_str())).await?;
    }
    Ok(())
}

/// Saves the user's rating; setting a rating (`Some`) marks the title finished
/// (`watched` or `waiting_episode` for ongoing series) and marks `targets` finished.
pub async fn rate_and_mark(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    rating: Option<i16>,
    targets: &[(i32, i32)],
) -> Result<TitleState, sqlx::Error> {
    rate_and_mark_with_meta(pool, user_id, kind, tmdb_id, rating, targets, &[], false).await
}

#[allow(clippy::too_many_arguments)]
pub async fn rate_and_mark_with_meta(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    rating: Option<i16>,
    targets: &[(i32, i32)],
    season_aired: &[(i32, i32)],
    ongoing_series: bool,
) -> Result<TitleState, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let target_status = if kind == MediaKind::Tv && ongoing_series {
        TitleStatus::WaitingEpisode.as_str()
    } else {
        TitleStatus::Watched.as_str()
    };
    if rating.is_some() {
        let sets = if target_status == "waiting_episode" {
            "rating = $4, status = 'waiting_episode', effective_rating = $4::real"
        } else {
            "rating = $4, status = 'watched', effective_rating = $4::real"
        };
        set_column_on(&mut tx, user_id, kind, tmdb_id, sets, rating).await?;
        if !targets.is_empty() {
            mark_many_tx(&mut tx, user_id, kind, tmdb_id, targets, true).await?;
        }
    } else {
        set_column_on(
            &mut tx,
            user_id,
            kind,
            tmdb_id,
            "rating = $4, effective_rating = NULL",
            rating,
        )
        .await?;
    }
    if kind == MediaKind::Tv {
        recompute_series_ratings_tx(&mut tx, user_id, tmdb_id, season_aired).await?;
    }
    tx.commit().await?;
    get_state(pool, user_id, kind, tmdb_id).await
}

/// Saves a manual season rating (does not overwrite episode ratings), marks the season watched,
/// recomputes bottom-up ratings with the 80% rule, and updates series status.
#[allow(clippy::too_many_arguments)]
pub async fn rate_season(
    pool: &PgPool,
    user_id: i64,
    tmdb_id: i64,
    season: i32,
    rating: Option<i16>,
    aired_in_season: i32,
    season_targets: &[(i32, i32)],
    all_aired_targets: &[(i32, i32)],
    season_aired: &[(i32, i32)],
    ongoing_series: bool,
) -> Result<TitleState, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO season_rating (user_id, tmdb_id, season, rating, aired_episodes, updated_at)
         VALUES ($1, $2, $3, $4, $5, now())
         ON CONFLICT (user_id, tmdb_id, season) DO UPDATE SET
           rating = EXCLUDED.rating,
           aired_episodes = GREATEST(season_rating.aired_episodes, EXCLUDED.aired_episodes),
           updated_at = now()",
    )
    .bind(user_id)
    .bind(tmdb_id)
    .bind(season)
    .bind(rating)
    .bind(aired_in_season)
    .execute(&mut *tx)
    .await?;

    if rating.is_some() && !season_targets.is_empty() {
        mark_many_tx(&mut tx, user_id, MediaKind::Tv, tmdb_id, season_targets, true).await?;
    }
    recompute_series_ratings_tx(&mut tx, user_id, tmdb_id, season_aired).await?;
    if rating.is_some() {
        derive_series_status_tx(&mut tx, user_id, tmdb_id, all_aired_targets, ongoing_series).await?;
    }
    tx.commit().await?;
    get_state(pool, user_id, MediaKind::Tv, tmdb_id).await
}

/// Saves a manual episode rating, marks that episode watched, recomputes season/series
/// bottom-up ratings with the 80% rule, and updates series status.
#[allow(clippy::too_many_arguments)]
pub async fn rate_episode(
    pool: &PgPool,
    user_id: i64,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    rating: Option<i16>,
    all_aired_targets: &[(i32, i32)],
    season_aired: &[(i32, i32)],
    ongoing_series: bool,
) -> Result<TitleState, sqlx::Error> {
    let mut tx = pool.begin().await?;
    match rating {
        Some(r) => {
            sqlx::query(
                "INSERT INTO episode_rating (user_id, tmdb_id, season, episode, rating, updated_at)
                 VALUES ($1, $2, $3, $4, $5, now())
                 ON CONFLICT (user_id, tmdb_id, season, episode) DO UPDATE SET
                   rating = EXCLUDED.rating, updated_at = now()",
            )
            .bind(user_id)
            .bind(tmdb_id)
            .bind(season)
            .bind(episode)
            .bind(r)
            .execute(&mut *tx)
            .await?;
            mark_many_tx(&mut tx, user_id, MediaKind::Tv, tmdb_id, &[(season, episode)], true).await?;
        }
        None => {
            sqlx::query(
                "DELETE FROM episode_rating WHERE user_id = $1 AND tmdb_id = $2 AND season = $3 AND episode = $4",
            )
            .bind(user_id)
            .bind(tmdb_id)
            .bind(season)
            .bind(episode)
            .execute(&mut *tx)
            .await?;
        }
    }
    recompute_series_ratings_tx(&mut tx, user_id, tmdb_id, season_aired).await?;
    if rating.is_some() {
        derive_series_status_tx(&mut tx, user_id, tmdb_id, all_aired_targets, ongoing_series).await?;
    }
    tx.commit().await?;
    get_state(pool, user_id, MediaKind::Tv, tmdb_id).await
}

/// Refreshes cached effective ratings when TMDB season/episode counts change.
pub async fn sync_series_aired_counts(
    pool: &PgPool,
    user_id: i64,
    tmdb_id: i64,
    season_aired: &[(i32, i32)],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    recompute_series_ratings_tx(&mut tx, user_id, tmdb_id, season_aired).await?;
    tx.commit().await
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

pub async fn set_scale(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    scale: Option<VideoScale>,
) -> Result<TitleState, sqlx::Error> {
    set_column(
        pool,
        user_id,
        kind,
        tmdb_id,
        "video_scale = $4",
        scale.map(VideoScale::as_str),
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

/// Full exclusion set of titles already watched, rated, dropped, or waiting (no 120-item cutoff).
pub async fn excluded_titles(pool: &PgPool, user_id: i64) -> Result<Vec<(MediaKind, i64)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT media_kind, tmdb_id FROM title_state
         WHERE user_id = $1
           AND (rating IS NOT NULL OR effective_rating IS NOT NULL
                OR status IN ('watched', 'dropped', 'waiting_episode', 'waiting_release'))
         UNION
         SELECT media_kind, tmdb_id FROM watch_progress
         WHERE user_id = $1 AND media_kind = 'movie' AND finished",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(|r| Ok((kind_of(r)?, r.try_get("tmdb_id")?))).collect()
}

/// Completed favorites (`watched` and rating >= 4.0), ordered oldest-updated first for "Пересматривай любимое".
pub async fn rewatch_favorites(pool: &PgPool, user_id: i64) -> Result<Vec<(MediaKind, i64, f32)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT media_kind, tmdb_id, COALESCE(effective_rating, rating::real) AS score
         FROM title_state
         WHERE user_id = $1
           AND status = 'watched'
           AND COALESCE(effective_rating, rating::real) >= 4.0
         ORDER BY updated_at ASC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|r| Ok((kind_of(r)?, r.try_get("tmdb_id")?, r.try_get("score")?)))
        .collect()
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
             SELECT media_kind, tmdb_id, status,
                    ROUND(COALESCE(effective_rating, rating::real))::smallint AS rating,
                    FALSE AS finished, updated_at
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

async fn mark_many_tx(
    conn: &mut sqlx::PgConnection,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    targets: &[(i32, i32)],
    watched: bool,
) -> Result<(), sqlx::Error> {
    let unique: std::collections::BTreeSet<(i32, i32)> = targets.iter().copied().collect();
    if unique.is_empty() {
        return Ok(());
    }
    let (seasons, episodes): (Vec<i32>, Vec<i32>) = unique.into_iter().unzip();
    if watched {
        sqlx::query(
            "INSERT INTO watch_progress
               (user_id, media_kind, tmdb_id, season, episode, position_sec, duration_sec, finished)
             SELECT $1, $2, $3, s, e, 0, 0, TRUE
             FROM UNNEST($4::int[], $5::int[]) AS t(s, e)
             ON CONFLICT (user_id, media_kind, tmdb_id, season, episode) DO UPDATE SET
               finished = TRUE, position_sec = watch_progress.duration_sec, updated_at = now()",
        )
    } else {
        sqlx::query(
            "DELETE FROM watch_progress
             WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3
               AND (season, episode) IN (SELECT s, e FROM UNNEST($4::int[], $5::int[]) AS t(s, e))",
        )
    }
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(&seasons)
    .bind(&episodes)
    .execute(conn)
    .await?;
    Ok(())
}

/// Marks episodes (or a film) watched, or clears them.
pub async fn mark(pool: &PgPool, user_id: i64, m: &MarkWatched) -> Result<(), sqlx::Error> {
    let targets: Vec<(i32, i32)> = match m.kind {
        MediaKind::Movie => vec![(0, 0)],
        MediaKind::Tv => m.episodes.iter().map(|e| (m.season, *e)).collect(),
    };
    let mut tx = pool.begin().await?;
    mark_many_tx(&mut tx, user_id, m.kind, m.tmdb_id, &targets, m.watched).await?;
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
    let (done, ongoing) = match kind {
        MediaKind::Movie => (progress.iter().any(|p| p.finished), false),
        MediaKind::Tv => match catalog.details(kind, tmdb_id).await {
            Ok(d) => {
                let ongoing = d.is_ongoing_series();
                let done = match d.last_episode {
                    Some(last) => progress.iter().any(|p| p.finished && (p.season, p.episode) >= last),
                    None => matches!(current, Some(TitleStatus::Watched | TitleStatus::WaitingEpisode)),
                };
                (done, ongoing)
            }
            // Keep finished status across transient TMDB errors.
            Err(_) => (
                matches!(current, Some(TitleStatus::Watched | TitleStatus::WaitingEpisode)),
                current == Some(TitleStatus::WaitingEpisode),
            ),
        },
    };
    let derived = if done {
        Some(if kind == MediaKind::Tv && ongoing {
            TitleStatus::WaitingEpisode
        } else {
            TitleStatus::Watched
        })
    } else if progress.is_empty() {
        None
    } else {
        Some(TitleStatus::Watching)
    };
    let next = match (current, derived) {
        (Some(TitleStatus::Dropped), _) if !played => current,
        (Some(TitleStatus::Want | TitleStatus::WaitingRelease), None) => current,
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
                min_seeders, max_size_gb, max_bitrate_mbps, sort, desktop_quality, mobile_quality
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
                max_bitrate_mbps: count("max_bitrate_mbps")?,
                sort: r.try_get("sort")?,
                desktop_quality: r.try_get("desktop_quality")?,
                mobile_quality: r.try_get("mobile_quality")?,
            }
        }
        None => UserPrefs::recommended_defaults(),
    })
}

pub async fn set_prefs(pool: &PgPool, user_id: i64, p: &UserPrefs) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_prefs (user_id, quality, voice, subtitle_lang, language, hdr, dolby_vision, subtitles,
                                 codec, direct_play, min_seeders, max_size_gb, max_bitrate_mbps, sort,
                                 desktop_quality, mobile_quality)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
         ON CONFLICT (user_id) DO UPDATE SET
           quality = EXCLUDED.quality, voice = EXCLUDED.voice, subtitle_lang = EXCLUDED.subtitle_lang,
           language = EXCLUDED.language, hdr = EXCLUDED.hdr, dolby_vision = EXCLUDED.dolby_vision,
           subtitles = EXCLUDED.subtitles, codec = EXCLUDED.codec, direct_play = EXCLUDED.direct_play,
           min_seeders = EXCLUDED.min_seeders, max_size_gb = EXCLUDED.max_size_gb,
           max_bitrate_mbps = EXCLUDED.max_bitrate_mbps, sort = EXCLUDED.sort,
           desktop_quality = EXCLUDED.desktop_quality, mobile_quality = EXCLUDED.mobile_quality",
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
    .bind(p.max_bitrate_mbps.map(|n| n.min(i32::MAX as u32) as i32))
    .bind(p.sort.as_deref())
    .bind(p.desktop_quality.as_deref())
    .bind(p.mobile_quality.as_deref())
    .execute(pool)
    .await?;
    Ok(())
}

const PRELOAD_COLUMNS: &str = "id, media_kind, tmdb_id, season, infohash, title, state, progress, size_bytes,
     requested_at, ready_at, delete_after, owner_watched_at, whole_torrent";

fn preload_from_row(row: &PgRow) -> Result<PreloadItem, sqlx::Error> {
    let state: String = row.try_get("state")?;
    let whole_torrent: bool = row.try_get("whole_torrent").unwrap_or(true);
    let watched = row.try_get::<Option<DateTime<Utc>>, _>("owner_watched_at")?.is_some();
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
        watched,
        poster_path: None,
        media_title: None,
        quality: None,
        voice: None,
        speed_bps: 0,
        dl_phase: if state == "ready" {
            PreloadPhase::Ready
        } else if state == "failed" {
            PreloadPhase::Failed
        } else {
            PreloadPhase::Waiting
        },
        episodes: Vec::new(),
        whole_torrent,
        requested_by: None,
        can_delete: true,
    })
}

#[derive(Debug, Clone)]
pub struct PreloadEpisodeRow {
    pub preload_id: i64,
    pub season: i32,
    pub episode: i32,
    pub file_index: Option<i32>,
    pub file_path: Option<String>,
    pub size_bytes: Option<i64>,
    pub progress: f32,
    pub state: PreloadState,
    pub ready_at: Option<DateTime<Utc>>,
    pub delete_after: Option<DateTime<Utc>>,
    pub owner_watched_at: Option<DateTime<Utc>>,
}

impl PreloadEpisodeRow {
    pub fn into_item(self) -> PreloadEpisodeItem {
        PreloadEpisodeItem {
            season: self.season,
            episode: self.episode,
            state: self.state,
            progress: self.progress,
            size_bytes: self.size_bytes,
            episode_name: None,
            ready_at: self.ready_at,
            delete_after: self.delete_after,
            watched: self.owner_watched_at.is_some(),
        }
    }
}

async fn load_preload_episodes(
    pool: &PgPool,
    preload_ids: &[i64],
) -> Result<HashMap<i64, Vec<PreloadEpisodeRow>>, sqlx::Error> {
    let mut map: HashMap<i64, Vec<PreloadEpisodeRow>> = HashMap::new();
    if preload_ids.is_empty() {
        return Ok(map);
    }
    let rows = sqlx::query(
        "SELECT preload_id, season, episode, file_index, file_path, size_bytes, progress, state,
                ready_at, delete_after, owner_watched_at
         FROM preload_episode
         WHERE preload_id = ANY($1) AND state <> 'deleted'
         ORDER BY season ASC, episode ASC",
    )
    .bind(preload_ids)
    .fetch_all(pool)
    .await?;
    for r in rows {
        let state_raw: String = r.try_get("state")?;
        let state = PreloadState::parse(&state_raw).ok_or_else(|| decode_err("state", &state_raw))?;
        let pid: i64 = r.try_get("preload_id")?;
        map.entry(pid).or_default().push(PreloadEpisodeRow {
            preload_id: pid,
            season: r.try_get("season")?,
            episode: r.try_get("episode")?,
            file_index: r.try_get("file_index")?,
            file_path: r.try_get("file_path")?,
            size_bytes: r.try_get("size_bytes")?,
            progress: r.try_get("progress")?,
            state,
            ready_at: r.try_get("ready_at")?,
            delete_after: r.try_get("delete_after")?,
            owner_watched_at: r.try_get("owner_watched_at")?,
        });
    }
    Ok(map)
}

fn attach_episodes(items: &mut [PreloadItem], ep_map: &mut HashMap<i64, Vec<PreloadEpisodeRow>>) {
    for p in items {
        if let Some(rows) = ep_map.remove(&p.id) {
            p.episodes = rows.into_iter().map(PreloadEpisodeRow::into_item).collect();
        }
    }
}

/// Queues a preload (full release or single episode); asking again re-queues deleted/failed items.
pub async fn preload_add(pool: &PgPool, user_id: i64, r: &PreloadRequest) -> Result<PreloadItem, sqlx::Error> {
    let whole_torrent = r.kind == MediaKind::Movie || r.episode <= 0;
    let season = if r.kind == MediaKind::Movie { 0 } else { r.season };
    let mut tx = pool.begin().await?;
    let row = sqlx::query(&format!(
        "INSERT INTO preload (user_id, media_kind, tmdb_id, season, infohash, title, whole_torrent)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (user_id, infohash) DO UPDATE SET
           title = COALESCE(EXCLUDED.title, preload.title),
           season = CASE WHEN preload.state IN ('deleted', 'failed') THEN EXCLUDED.season
                         WHEN preload.season = 0 THEN EXCLUDED.season
                         ELSE preload.season END,
           whole_torrent = CASE WHEN preload.state IN ('deleted', 'failed') THEN EXCLUDED.whole_torrent
                                ELSE preload.whole_torrent OR EXCLUDED.whole_torrent END,
           state = CASE WHEN preload.state IN ('deleted', 'failed') THEN 'queued'
                        WHEN EXCLUDED.whole_torrent AND NOT preload.whole_torrent THEN 'queued'
                        ELSE preload.state END,
           progress = CASE WHEN preload.state IN ('deleted', 'failed') THEN 0 ELSE preload.progress END,
           ready_at = CASE WHEN preload.state IN ('deleted', 'failed') THEN NULL ELSE preload.ready_at END,
           delete_after = NULL,
           owner_watched_at = CASE WHEN preload.state IN ('deleted', 'failed') THEN NULL ELSE preload.owner_watched_at END,
           requested_at = now()
         RETURNING {PRELOAD_COLUMNS}"
    ))
    .bind(user_id)
    .bind(r.kind.as_str())
    .bind(r.tmdb_id)
    .bind(season)
    .bind(&r.infohash)
    .bind(r.title.as_deref())
    .bind(whole_torrent)
    .fetch_one(&mut *tx)
    .await?;
    let mut item = preload_from_row(&row)?;
    if r.kind == MediaKind::Tv && r.episode > 0 {
        let ep_season = season.max(1);
        let ep_row = sqlx::query(
            "INSERT INTO preload_episode (preload_id, season, episode, state, progress)
             VALUES ($1, $2, $3, 'queued', 0)
             ON CONFLICT (preload_id, season, episode) DO UPDATE SET
               state = CASE WHEN preload_episode.state IN ('deleted', 'failed') THEN 'queued' ELSE preload_episode.state END,
               progress = CASE WHEN preload_episode.state IN ('deleted', 'failed') THEN 0 ELSE preload_episode.progress END,
               delete_after = NULL,
               owner_watched_at = CASE WHEN preload_episode.state IN ('deleted', 'failed') THEN NULL ELSE preload_episode.owner_watched_at END
             RETURNING state",
        )
        .bind(item.id)
        .bind(ep_season)
        .bind(r.episode)
        .fetch_one(&mut *tx)
        .await?;
        let ep_state: String = ep_row.try_get("state")?;
        if ep_state != "ready" && item.state == PreloadState::Ready {
            sqlx::query("UPDATE preload SET state = 'downloading', ready_at = NULL WHERE id = $1")
                .bind(item.id)
                .execute(&mut *tx)
                .await?;
            item.state = PreloadState::Downloading;
            item.dl_phase = PreloadPhase::Waiting;
        }
    }
    tx.commit().await?;
    let mut eps = load_preload_episodes(pool, &[item.id]).await?;
    attach_episodes(std::slice::from_mut(&mut item), &mut eps);
    Ok(item)
}

/// All non-deleted preloads for one user & title, newest request first.
pub async fn title_preloads(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
) -> Result<Vec<PreloadItem>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT {PRELOAD_COLUMNS} FROM preload
         WHERE user_id = $1 AND media_kind = $2 AND tmdb_id = $3 AND state <> 'deleted'
         ORDER BY requested_at DESC"
    ))
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .fetch_all(pool)
    .await?;
    let mut items: Vec<PreloadItem> = rows.iter().map(preload_from_row).collect::<Result<_, _>>()?;
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    let mut eps = load_preload_episodes(pool, &ids).await?;
    attach_episodes(&mut items, &mut eps);
    Ok(items)
}

pub async fn preloads(pool: &PgPool, user_id: i64) -> Result<Vec<PreloadItem>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT {PRELOAD_COLUMNS} FROM preload WHERE user_id = $1 AND state <> 'deleted'
         ORDER BY requested_at DESC"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let mut items: Vec<PreloadItem> = rows.iter().map(preload_from_row).collect::<Result<_, _>>()?;
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    let mut eps = load_preload_episodes(pool, &ids).await?;
    attach_episodes(&mut items, &mut eps);
    Ok(items)
}

fn state_rank(st: PreloadState) -> u8 {
    match st {
        PreloadState::Ready => 4,
        PreloadState::Downloading => 3,
        PreloadState::Queued => 2,
        PreloadState::Failed => 1,
        PreloadState::Deleted => 0,
    }
}

/// All non-deleted downloads on the server across all users, merged by infohash for UI display.
pub async fn all_server_preloads(
    pool: &PgPool,
    viewer_id: i64,
    is_admin: bool,
) -> Result<Vec<PreloadItem>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT p.id, p.user_id, p.media_kind, p.tmdb_id, p.season, p.infohash, p.title,
                p.state, p.progress, p.size_bytes, p.requested_at, p.ready_at, p.delete_after,
                p.owner_watched_at, p.whole_torrent,
                COALESCE(NULLIF(u.first_name, ''), NULLIF(u.username, ''), 'Участник') AS requester_name
         FROM preload p
         JOIN users u ON u.id = p.user_id
         WHERE p.state <> 'deleted'
         ORDER BY p.requested_at DESC",
    )
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = rows.iter().map(|r| r.try_get("id")).collect::<Result<Vec<i64>, _>>()?;
    let mut ep_map = load_preload_episodes(pool, &ids).await?;

    struct Group {
        item: PreloadItem,
        viewer_owns: bool,
        other_requesters: Vec<String>,
        ep_map: BTreeMap<(i32, i32), PreloadEpisodeItem>,
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut idx_by_hash: HashMap<String, usize> = HashMap::new();

    for r in rows {
        let item = preload_from_row(&r)?;
        let row_uid: i64 = r.try_get("user_id")?;
        let requester_name: String = r.try_get("requester_name")?;
        let ep_rows = ep_map.remove(&item.id).unwrap_or_default();
        let hash_key = item.infohash.to_ascii_lowercase();
        let is_mine = row_uid == viewer_id;

        if let Some(&idx) = idx_by_hash.get(&hash_key) {
            let g = &mut groups[idx];
            if is_mine && !g.viewer_owns {
                g.item.id = item.id;
                g.viewer_owns = true;
            }
            if !is_mine && !g.other_requesters.contains(&requester_name) {
                g.other_requesters.push(requester_name);
            }
            g.item.whole_torrent |= item.whole_torrent;
            if g.item.season == 0 && item.season > 0 {
                g.item.season = item.season;
            }
            if g.item.title.is_none() {
                g.item.title = item.title;
            }
            if item.requested_at > g.item.requested_at {
                g.item.requested_at = item.requested_at;
            }
            match (g.item.ready_at, item.ready_at) {
                (None, Some(ra)) => g.item.ready_at = Some(ra),
                (Some(a), Some(b)) if b > a => g.item.ready_at = Some(b),
                _ => {}
            }
            if g.item.whole_torrent && state_rank(item.state) > state_rank(g.item.state) {
                g.item.state = item.state;
                g.item.progress = g.item.progress.max(item.progress);
            }
            g.item.size_bytes = match (g.item.size_bytes, item.size_bytes) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            g.item.delete_after = match (g.item.delete_after, item.delete_after) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            g.item.watched &= item.watched;
            for er in ep_rows {
                let ei = er.into_item();
                g.ep_map
                    .entry((ei.season, ei.episode))
                    .and_modify(|cur| {
                        if state_rank(ei.state) > state_rank(cur.state) {
                            cur.state = ei.state;
                        }
                        cur.progress = cur.progress.max(ei.progress);
                        cur.size_bytes = match (cur.size_bytes, ei.size_bytes) {
                            (Some(a), Some(b)) => Some(a.max(b)),
                            (a, b) => a.or(b),
                        };
                        cur.ready_at = cur.ready_at.or(ei.ready_at);
                        cur.delete_after = match (cur.delete_after, ei.delete_after) {
                            (Some(a), Some(b)) => Some(a.max(b)),
                            (a, b) => a.or(b),
                        };
                        cur.watched &= ei.watched;
                    })
                    .or_insert(ei);
            }
        } else {
            let mut ep_btree = BTreeMap::new();
            for er in ep_rows {
                let ei = er.into_item();
                ep_btree.insert((ei.season, ei.episode), ei);
            }
            let other_requesters = if is_mine { Vec::new() } else { vec![requester_name] };
            let idx = groups.len();
            idx_by_hash.insert(hash_key, idx);
            groups.push(Group {
                item,
                viewer_owns: is_mine,
                other_requesters,
                ep_map: ep_btree,
            });
        }
    }

    let mut out = Vec::with_capacity(groups.len());
    for mut g in groups {
        g.item.episodes = g.ep_map.into_values().collect();
        if !g.item.whole_torrent && !g.item.episodes.is_empty() {
            let total = g.item.episodes.len();
            let ready = g
                .item
                .episodes
                .iter()
                .filter(|e| e.state == PreloadState::Ready)
                .count();
            let downloading = g
                .item
                .episodes
                .iter()
                .filter(|e| matches!(e.state, PreloadState::Queued | PreloadState::Downloading))
                .count();
            let failed = g
                .item
                .episodes
                .iter()
                .filter(|e| e.state == PreloadState::Failed)
                .count();
            g.item.state = if downloading > 0 {
                if g.item.episodes.iter().any(|e| e.state == PreloadState::Downloading) {
                    PreloadState::Downloading
                } else {
                    PreloadState::Queued
                }
            } else if ready == total {
                PreloadState::Ready
            } else if failed == total {
                PreloadState::Failed
            } else if ready > 0 {
                PreloadState::Ready
            } else {
                PreloadState::Failed
            };
            let ready_eps: Vec<&PreloadEpisodeItem> = g
                .item
                .episodes
                .iter()
                .filter(|e| e.state == PreloadState::Ready)
                .collect();
            if !ready_eps.is_empty() {
                g.item.watched = ready_eps.iter().all(|e| e.watched);
                if g.item.delete_after.is_none() {
                    g.item.delete_after = ready_eps.iter().filter_map(|e| e.delete_after).min();
                }
            }
            let known_bytes: i64 = g.item.episodes.iter().filter_map(|e| e.size_bytes).sum();
            if known_bytes > 0 {
                g.item.size_bytes = Some(known_bytes);
                let done_bytes: f64 = g
                    .item
                    .episodes
                    .iter()
                    .map(|e| {
                        let w = e.size_bytes.unwrap_or(1).max(1) as f64;
                        let p = if e.state == PreloadState::Ready {
                            1.0
                        } else {
                            f64::from(e.progress.clamp(0.0, 1.0))
                        };
                        w * p
                    })
                    .sum();
                let weight_sum: f64 = g
                    .item
                    .episodes
                    .iter()
                    .map(|e| e.size_bytes.unwrap_or(1).max(1) as f64)
                    .sum();
                g.item.progress = (done_bytes / weight_sum.max(1.0)) as f32;
            } else {
                let sum_p: f32 = g
                    .item
                    .episodes
                    .iter()
                    .map(|e| {
                        if e.state == PreloadState::Ready {
                            1.0
                        } else {
                            e.progress
                        }
                    })
                    .sum();
                g.item.progress = sum_p / total as f32;
            }
        }
        g.item.can_delete = is_admin || g.viewer_owns;
        g.item.requested_by = if g.other_requesters.is_empty() {
            None
        } else {
            Some(g.other_requesters.join(", "))
        };
        g.item.dl_phase = match g.item.state {
            PreloadState::Ready => PreloadPhase::Ready,
            PreloadState::Failed => PreloadPhase::Failed,
            _ => PreloadPhase::Waiting,
        };
        out.push(g.item);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletePreloadResult {
    NotFound,
    Forbidden,
    Deleted(String),
}

/// Removes a preload by hand (owner or admin). Admin deletes all user entries sharing that infohash.
pub async fn preload_delete_for_actor(
    pool: &PgPool,
    id: i64,
    actor_id: i64,
    is_admin: bool,
) -> Result<DeletePreloadResult, sqlx::Error> {
    let row = sqlx::query("SELECT user_id, infohash, state FROM preload WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else {
        return Ok(DeletePreloadResult::NotFound);
    };
    let state: String = row.try_get("state")?;
    if state == "deleted" {
        return Ok(DeletePreloadResult::NotFound);
    }
    let owner_id: i64 = row.try_get("user_id")?;
    let infohash: String = row.try_get("infohash")?;
    if !is_admin && owner_id != actor_id {
        return Ok(DeletePreloadResult::Forbidden);
    }
    let mut tx = pool.begin().await?;
    let ids: Vec<i64> = if is_admin {
        sqlx::query_scalar(
            "UPDATE preload SET state = 'deleted', delete_after = NULL, owner_watched_at = NULL
             WHERE infohash = $1 AND state <> 'deleted' RETURNING id",
        )
        .bind(&infohash)
        .fetch_all(&mut *tx)
        .await?
    } else {
        sqlx::query_scalar(
            "UPDATE preload SET state = 'deleted', delete_after = NULL, owner_watched_at = NULL
             WHERE id = $1 AND user_id = $2 AND state <> 'deleted' RETURNING id",
        )
        .bind(id)
        .bind(actor_id)
        .fetch_all(&mut *tx)
        .await?
    };
    if !ids.is_empty() {
        sqlx::query(
            "UPDATE preload_episode SET state = 'deleted', delete_after = NULL, owner_watched_at = NULL
             WHERE preload_id = ANY($1)",
        )
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(DeletePreloadResult::Deleted(infohash))
}

/// Removes a preload by hand (owner only). Returns its info-hash, or `None` if there was none.
pub async fn preload_delete(pool: &PgPool, user_id: i64, id: i64) -> Result<Option<String>, sqlx::Error> {
    match preload_delete_for_actor(pool, id, user_id, false).await? {
        DeletePreloadResult::Deleted(h) => Ok(Some(h)),
        DeletePreloadResult::NotFound | DeletePreloadResult::Forbidden => Ok(None),
    }
}

/// Removes a single episode from a preload (owner or admin), collapsing parent if no episodes remain.
pub async fn preload_delete_episode_for_actor(
    pool: &PgPool,
    preload_id: i64,
    season: i32,
    episode: i32,
    actor_id: i64,
    is_admin: bool,
) -> Result<DeletePreloadResult, sqlx::Error> {
    let row = sqlx::query("SELECT user_id, infohash, state FROM preload WHERE id = $1")
        .bind(preload_id)
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else {
        return Ok(DeletePreloadResult::NotFound);
    };
    let state: String = row.try_get("state")?;
    if state == "deleted" {
        return Ok(DeletePreloadResult::NotFound);
    }
    let owner_id: i64 = row.try_get("user_id")?;
    let infohash: String = row.try_get("infohash")?;
    if !is_admin && owner_id != actor_id {
        return Ok(DeletePreloadResult::Forbidden);
    }

    let mut tx = pool.begin().await?;
    let target_ids: Vec<i64> = if is_admin {
        sqlx::query_scalar("SELECT id FROM preload WHERE infohash = $1 AND state <> 'deleted'")
            .bind(&infohash)
            .fetch_all(&mut *tx)
            .await?
    } else {
        vec![preload_id]
    };
    if target_ids.is_empty() {
        return Ok(DeletePreloadResult::NotFound);
    }
    let updated = sqlx::query(
        "UPDATE preload_episode SET state = 'deleted', delete_after = NULL, owner_watched_at = NULL
         WHERE preload_id = ANY($1) AND season = $2 AND episode = $3 AND state <> 'deleted'",
    )
    .bind(&target_ids)
    .bind(season)
    .bind(episode)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        // Also support cancelling a whole-torrent TV preload before metadata arrived if requested on its season.
        let has_any_ep: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM preload_episode WHERE preload_id = ANY($1) AND state <> 'deleted')",
        )
        .bind(&target_ids)
        .fetch_one(&mut *tx)
        .await?;
        if has_any_ep {
            return Ok(DeletePreloadResult::NotFound);
        }
        sqlx::query(
            "UPDATE preload SET state = 'deleted', delete_after = NULL, owner_watched_at = NULL WHERE id = ANY($1)",
        )
        .bind(&target_ids)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(DeletePreloadResult::Deleted(infohash));
    }

    sqlx::query("UPDATE preload SET whole_torrent = FALSE WHERE id = ANY($1)")
        .bind(&target_ids)
        .execute(&mut *tx)
        .await?;

    for pid in target_ids {
        recompute_preload_from_episodes_tx(&mut tx, pid).await?;
    }
    tx.commit().await?;
    Ok(DeletePreloadResult::Deleted(infohash))
}

async fn recompute_preload_from_episodes_tx(conn: &mut sqlx::PgConnection, preload_id: i64) -> Result<(), sqlx::Error> {
    let eps = sqlx::query(
        "SELECT state, progress, size_bytes FROM preload_episode WHERE preload_id = $1 AND state <> 'deleted'",
    )
    .bind(preload_id)
    .fetch_all(&mut *conn)
    .await?;
    if eps.is_empty() {
        sqlx::query("UPDATE preload SET state = 'deleted', delete_after = NULL WHERE id = $1")
            .bind(preload_id)
            .execute(&mut *conn)
            .await?;
        return Ok(());
    }
    let mut ready = 0usize;
    let mut downloading = 0usize;
    let mut queued = 0usize;
    let mut failed = 0usize;
    let mut total_bytes: i64 = 0;
    let mut has_bytes = false;
    let mut weighted_done = 0.0f64;
    let mut weighted_total = 0.0f64;
    let mut sum_prog = 0.0f32;
    for r in &eps {
        let st: String = r.try_get("state")?;
        let prog: f32 = r.try_get("progress")?;
        let sz: Option<i64> = r.try_get("size_bytes")?;
        let p_eff = if st == "ready" { 1.0 } else { prog.clamp(0.0, 1.0) };
        match st.as_str() {
            "ready" => ready += 1,
            "downloading" => downloading += 1,
            "queued" => queued += 1,
            "failed" => failed += 1,
            _ => {}
        }
        if let Some(b) = sz.filter(|&b| b > 0) {
            has_bytes = true;
            total_bytes += b;
            weighted_done += (b as f64) * f64::from(p_eff);
            weighted_total += b as f64;
        }
        sum_prog += p_eff;
    }
    let n = eps.len();
    let new_state = if downloading > 0 {
        "downloading"
    } else if queued > 0 {
        if ready > 0 {
            "downloading"
        } else {
            "queued"
        }
    } else if ready == n {
        "ready"
    } else if failed == n {
        "failed"
    } else if ready > 0 {
        "ready"
    } else {
        "failed"
    };
    let new_progress = if has_bytes && weighted_total > 0.0 {
        (weighted_done / weighted_total) as f32
    } else {
        sum_prog / (n as f32)
    };
    sqlx::query(
        "UPDATE preload
         SET state = $2,
             progress = $3,
             size_bytes = COALESCE($4, size_bytes),
             ready_at = CASE WHEN $2 = 'ready' THEN COALESCE(ready_at, now()) ELSE ready_at END,
             delete_after = CASE
                 WHEN $2 = 'ready' THEN LEAST(COALESCE(delete_after, now() + interval '7 days'), now() + interval '7 days')
                 ELSE delete_after
             END
         WHERE id = $1",
    )
    .bind(preload_id)
    .bind(new_state)
    .bind(new_progress)
    .bind(has_bytes.then_some(total_bytes))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Union of wanted files across all active users' preload rows for a given torrent `infohash`.
#[derive(Debug, Clone, Default)]
pub struct WantedForHash {
    pub whole_torrent: bool,
    pub episodes: Vec<(i32, i32)>,
}

pub async fn preload_wanted_for_hash(pool: &PgPool, infohash: &str) -> Result<WantedForHash, sqlx::Error> {
    let rows = sqlx::query("SELECT id, whole_torrent FROM preload WHERE infohash = $1 AND state <> 'deleted'")
        .bind(infohash)
        .fetch_all(pool)
        .await?;
    let mut out = WantedForHash::default();
    let mut partial_ids = Vec::new();
    for r in rows {
        let id: i64 = r.try_get("id")?;
        let whole: bool = r.try_get("whole_torrent")?;
        if whole {
            out.whole_torrent = true;
        } else {
            partial_ids.push(id);
        }
    }
    if !partial_ids.is_empty() {
        let ep_rows = sqlx::query(
            "SELECT DISTINCT season, episode FROM preload_episode
             WHERE preload_id = ANY($1) AND state <> 'deleted'
             ORDER BY season, episode",
        )
        .bind(&partial_ids)
        .fetch_all(pool)
        .await?;
        for er in ep_rows {
            out.episodes.push((er.try_get("season")?, er.try_get("episode")?));
        }
    }
    Ok(out)
}

/// Once the preload owner finishes a film or season, marks it watched and shortens retention to `at`
/// (capped by the 7-day ready window on first watch). If another user watches a preload already
/// watched by its owner, extends `delete_after` to at least `at`; unwatched preloads of other users
/// keep their 7-day window untouched.
pub async fn preload_schedule_removal(
    pool: &PgPool,
    user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    at: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let owner_r = sqlx::query(
        "UPDATE preload
         SET delete_after = CASE
                 WHEN owner_watched_at IS NULL THEN LEAST(COALESCE(delete_after, $5), $5)
                 ELSE GREATEST(COALESCE(delete_after, $5), $5)
             END,
             owner_watched_at = COALESCE(owner_watched_at, now())
         WHERE user_id = $1
           AND media_kind = $2
           AND tmdb_id = $3
           AND season = $4
           AND state <> 'deleted'
           AND whole_torrent = TRUE",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(season)
    .bind(at)
    .execute(pool)
    .await?;

    let other_r = sqlx::query(
        "UPDATE preload
         SET delete_after = GREATEST(COALESCE(delete_after, $5), $5)
         WHERE user_id <> $1
           AND media_kind = $2
           AND tmdb_id = $3
           AND season = $4
           AND state <> 'deleted'
           AND whole_torrent = TRUE
           AND owner_watched_at IS NOT NULL",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(season)
    .bind(at)
    .execute(pool)
    .await?;
    Ok(owner_r.rows_affected() + other_r.rows_affected())
}

/// Applies the same owner-first retention rule to an individually-downloaded episode.
pub async fn preload_schedule_episode_removal(
    pool: &PgPool,
    user_id: i64,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    at: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let owner_r = sqlx::query(
        "UPDATE preload_episode pe
         SET delete_after = CASE
                 WHEN pe.owner_watched_at IS NULL THEN LEAST(COALESCE(pe.delete_after, $5), $5)
                 ELSE GREATEST(COALESCE(pe.delete_after, $5), $5)
             END,
             owner_watched_at = COALESCE(pe.owner_watched_at, now())
         FROM preload p
         WHERE pe.preload_id = p.id
           AND p.user_id = $1
           AND p.media_kind = 'tv'
           AND p.tmdb_id = $2
           AND p.whole_torrent = FALSE
           AND p.state <> 'deleted'
           AND pe.season = $3
           AND pe.episode = $4
           AND pe.state = 'ready'",
    )
    .bind(user_id)
    .bind(tmdb_id)
    .bind(season)
    .bind(episode)
    .bind(at)
    .execute(pool)
    .await?;

    let other_r = sqlx::query(
        "UPDATE preload_episode pe
         SET delete_after = GREATEST(COALESCE(pe.delete_after, $5), $5)
         FROM preload p
         WHERE pe.preload_id = p.id
           AND p.user_id <> $1
           AND p.media_kind = 'tv'
           AND p.tmdb_id = $2
           AND p.whole_torrent = FALSE
           AND p.state <> 'deleted'
           AND pe.season = $3
           AND pe.episode = $4
           AND pe.state = 'ready'
           AND pe.owner_watched_at IS NOT NULL",
    )
    .bind(user_id)
    .bind(tmdb_id)
    .bind(season)
    .bind(episode)
    .bind(at)
    .execute(pool)
    .await?;
    Ok(owner_r.rows_affected() + other_r.rows_affected())
}

/// Extends post-watch retention to `at` when any participant is actively watching a preload
/// (or episode) whose owner has already watched it; unwatched 7-day preloads are untouched.
pub async fn preload_extend_active_watch(
    pool: &PgPool,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    episode: i32,
    at: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let mut updated = sqlx::query(
        "UPDATE preload
         SET delete_after = GREATEST(COALESCE(delete_after, $4), $4)
         WHERE media_kind = $1
           AND tmdb_id = $2
           AND season = $3
           AND state <> 'deleted'
           AND whole_torrent = TRUE
           AND owner_watched_at IS NOT NULL",
    )
    .bind(kind.as_str())
    .bind(tmdb_id)
    .bind(if kind == MediaKind::Movie { 0 } else { season })
    .bind(at)
    .execute(pool)
    .await?
    .rows_affected();

    if kind == MediaKind::Tv && season > 0 && episode > 0 {
        updated += sqlx::query(
            "UPDATE preload_episode pe
             SET delete_after = GREATEST(COALESCE(pe.delete_after, $4), $4)
             FROM preload p
             WHERE pe.preload_id = p.id
               AND p.media_kind = 'tv'
               AND p.tmdb_id = $1
               AND p.whole_torrent = FALSE
               AND p.state <> 'deleted'
               AND pe.season = $2
               AND pe.episode = $3
               AND pe.state = 'ready'
               AND pe.owner_watched_at IS NOT NULL",
        )
        .bind(tmdb_id)
        .bind(season)
        .bind(episode)
        .bind(at)
        .execute(pool)
        .await?
        .rows_affected();
    }
    Ok(updated)
}

/// Individual downloaded episodes whose retention expired before their whole parent torrent.
pub async fn preload_episodes_due(pool: &PgPool) -> Result<Vec<(i64, String, i32, i32)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT pe.preload_id, p.infohash, pe.season, pe.episode
         FROM preload_episode pe
         JOIN preload p ON p.id = pe.preload_id
         WHERE p.state <> 'deleted' AND p.whole_torrent = FALSE
           AND pe.state <> 'deleted' AND pe.delete_after < now()",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|r| {
            Ok((
                r.try_get("preload_id")?,
                r.try_get("infohash")?,
                r.try_get("season")?,
                r.try_get("episode")?,
            ))
        })
        .collect()
}

pub async fn preload_episode_mark_deleted(
    pool: &PgPool,
    preload_id: i64,
    season: i32,
    episode: i32,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE preload_episode SET state = 'deleted', delete_after = NULL, owner_watched_at = NULL
         WHERE preload_id = $1 AND season = $2 AND episode = $3",
    )
    .bind(preload_id)
    .bind(season)
    .bind(episode)
    .execute(&mut *tx)
    .await?;
    recompute_preload_from_episodes_tx(&mut tx, preload_id).await?;
    tx.commit().await?;
    Ok(())
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

/// Active preload row needed for per-episode / partial-torrent synchronization.
#[derive(Debug, Clone)]
pub struct ActivePreloadRow {
    pub id: i64,
    pub user_id: i64,
    pub telegram_id: i64,
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub season: i32,
    pub infohash: String,
    pub title: Option<String>,
    pub whole_torrent: bool,
    pub episodes: Vec<PreloadEpisodeRow>,
}

pub async fn preloads_active_rows(pool: &PgPool) -> Result<Vec<ActivePreloadRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT p.id, p.user_id, u.telegram_id, p.media_kind, p.tmdb_id, p.season, p.infohash, p.title, p.whole_torrent
         FROM preload p
         JOIN users u ON u.id = p.user_id
         WHERE p.state IN ('queued', 'downloading')",
    )
    .fetch_all(pool)
    .await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.try_get("id")).collect::<Result<Vec<i64>, _>>()?;
    let mut ep_map = load_preload_episodes(pool, &ids).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let id: i64 = r.try_get("id")?;
        out.push(ActivePreloadRow {
            id,
            user_id: r.try_get("user_id")?,
            telegram_id: r.try_get("telegram_id")?,
            kind: kind_of(&r)?,
            tmdb_id: r.try_get("tmdb_id")?,
            season: r.try_get("season")?,
            infohash: r.try_get("infohash")?,
            title: r.try_get("title")?,
            whole_torrent: r.try_get("whole_torrent")?,
            episodes: ep_map.remove(&id).unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Ready TV preloads that do not yet have rows in `preload_episode` (for lazy backfill).
pub async fn preloads_tv_ready_needing_backfill(pool: &PgPool) -> Result<Vec<(i64, String, i32)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, infohash, season FROM preload
         WHERE media_kind = 'tv' AND state = 'ready'
           AND NOT EXISTS (SELECT 1 FROM preload_episode pe WHERE pe.preload_id = preload.id)",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|r| Ok((r.try_get("id")?, r.try_get("infohash")?, r.try_get("season")?)))
        .collect()
}

/// Upserts one episode's progress/state inside a preload; returns `true` if it just transitioned to `ready`.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_preload_episode_progress(
    pool: &PgPool,
    preload_id: i64,
    season: i32,
    episode: i32,
    file_index: Option<i32>,
    file_path: Option<&str>,
    size_bytes: Option<i64>,
    state: PreloadState,
    progress: f32,
    preserve_deleted: bool,
) -> Result<bool, sqlx::Error> {
    if preserve_deleted {
        let deleted: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM preload_episode WHERE preload_id = $1 AND season = $2 AND episode = $3 AND state = 'deleted')",
        )
        .bind(preload_id)
        .bind(season)
        .bind(episode)
        .fetch_one(pool)
        .await?;
        if deleted {
            return Ok(false);
        }
    }
    let prev_state: Option<String> =
        sqlx::query_scalar("SELECT state FROM preload_episode WHERE preload_id = $1 AND season = $2 AND episode = $3")
            .bind(preload_id)
            .bind(season)
            .bind(episode)
            .fetch_optional(pool)
            .await?;
    let became_ready = state == PreloadState::Ready && prev_state.as_deref() != Some("ready");
    sqlx::query(
        "INSERT INTO preload_episode
            (preload_id, season, episode, file_index, file_path, size_bytes, progress, state, ready_at, delete_after)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                 CASE WHEN $8 = 'ready' THEN now() ELSE NULL END,
                 CASE WHEN $8 = 'ready' THEN now() + interval '7 days' ELSE NULL END)
         ON CONFLICT (preload_id, season, episode) DO UPDATE SET
           file_index = COALESCE(EXCLUDED.file_index, preload_episode.file_index),
           file_path = COALESCE(EXCLUDED.file_path, preload_episode.file_path),
           size_bytes = COALESCE(EXCLUDED.size_bytes, preload_episode.size_bytes),
           progress = EXCLUDED.progress,
           state = EXCLUDED.state,
           ready_at = CASE WHEN EXCLUDED.state = 'ready' THEN COALESCE(preload_episode.ready_at, now()) ELSE preload_episode.ready_at END,
           delete_after = CASE
               WHEN EXCLUDED.state = 'ready' THEN LEAST(COALESCE(preload_episode.delete_after, now() + interval '7 days'), now() + interval '7 days')
               ELSE preload_episode.delete_after
           END",
    )
    .bind(preload_id)
    .bind(season)
    .bind(episode)
    .bind(file_index)
    .bind(file_path)
    .bind(size_bytes)
    .bind(progress)
    .bind(state.as_str())
    .execute(pool)
    .await?;
    Ok(became_ready)
}

/// Updates one preload row directly from its episodes or torrent state.
pub async fn update_preload_row_progress(
    pool: &PgPool,
    preload_id: i64,
    state: PreloadState,
    progress: f32,
    size_bytes: Option<i64>,
) -> Result<bool, sqlx::Error> {
    let updated: Option<String> = sqlx::query_scalar(
        "UPDATE preload
         SET state = $2,
             progress = $3,
             size_bytes = COALESCE($4, size_bytes),
             ready_at = CASE WHEN $2 = 'ready' THEN COALESCE(ready_at, now()) ELSE ready_at END,
             delete_after = CASE
                 WHEN $2 = 'ready' THEN LEAST(COALESCE(delete_after, now() + interval '7 days'), now() + interval '7 days')
                 ELSE delete_after
             END
         WHERE id = $1 AND state IN ('queued', 'downloading')
         RETURNING state",
    )
    .bind(preload_id)
    .bind(state.as_str())
    .bind(progress)
    .bind(size_bytes)
    .fetch_optional(pool)
    .await?;
    Ok(updated.as_deref() == Some("ready"))
}

/// User preload row that transitioned to a new state in [`preload_progress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreloadTransition {
    pub user_id: i64,
    pub telegram_id: i64,
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub season: i32,
    pub title: Option<String>,
    pub size_bytes: Option<i64>,
}

/// Progress from the torrent client, for every user who asked for this release.
/// Returns the rows updated from `queued`/`downloading`.
/// Finished downloads are kept on disk for at most 7 days unless watched earlier.
pub async fn preload_progress(
    pool: &PgPool,
    infohash: &str,
    state: PreloadState,
    progress: f32,
    size_bytes: Option<i64>,
) -> Result<Vec<PreloadTransition>, sqlx::Error> {
    let rows = sqlx::query(
        "UPDATE preload p
         SET state = $2,
             progress = $3,
             size_bytes = COALESCE($4, p.size_bytes),
             ready_at = CASE WHEN $2 = 'ready' THEN COALESCE(p.ready_at, now()) ELSE p.ready_at END,
             delete_after = CASE
                 WHEN $2 = 'ready' THEN LEAST(COALESCE(p.delete_after, now() + interval '7 days'), now() + interval '7 days')
                 ELSE p.delete_after
             END
         FROM users u
         WHERE p.user_id = u.id
           AND p.infohash = $1
           AND p.state IN ('queued', 'downloading')
         RETURNING p.user_id, u.telegram_id, p.media_kind, p.tmdb_id, p.season, p.title, p.size_bytes",
    )
    .bind(infohash)
    .bind(state.as_str())
    .bind(progress)
    .bind(size_bytes)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(PreloadTransition {
                user_id: row.try_get("user_id")?,
                telegram_id: row.try_get("telegram_id")?,
                kind: kind_of(row)?,
                tmdb_id: row.try_get("tmdb_id")?,
                season: row.try_get("season")?,
                title: row.try_get("title")?,
                size_bytes: row.try_get("size_bytes")?,
            })
        })
        .collect()
}

/// Ready-to-play server download metadata.
#[derive(Debug, Clone)]
pub struct ReadyPreload {
    pub infohash: String,
    pub season: i32,
    pub title: Option<String>,
    pub ready_at: DateTime<Utc>,
    pub whole_torrent: bool,
    pub ready_episodes: Vec<(i32, i32)>,
}

impl ReadyPreload {
    /// True if this ready download covers the requested `(kind, season, episode)`.
    pub fn covers(&self, kind: MediaKind, season: i32, episode: i32) -> bool {
        if kind == MediaKind::Movie {
            return true;
        }
        if episode <= 0 {
            return self.whole_torrent && (self.season == 0 || self.season == season);
        }
        if !self.ready_episodes.is_empty() {
            return self.ready_episodes.contains(&(season, episode));
        }
        if !self.whole_torrent {
            return false;
        }
        if self.season != 0 && self.season != season {
            return false;
        }
        match self
            .title
            .as_deref()
            .map(crate::torrent::title::parse_title)
            .and_then(|p| p.episode_range)
        {
            Some((a, b)) => (a..=b).contains(&episode),
            None => true,
        }
    }
}

/// All ready server preloads (or partial preloads with at least one ready episode) for a title, newest finished download first.
pub async fn ready_preloads_for_title(
    pool: &PgPool,
    kind: MediaKind,
    tmdb_id: i64,
) -> Result<Vec<ReadyPreload>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, infohash, season, title, whole_torrent,
                COALESCE(
                    ready_at,
                    (SELECT MAX(pe.ready_at) FROM preload_episode pe WHERE pe.preload_id = preload.id AND pe.state = 'ready'),
                    requested_at
                ) AS ready_ts
         FROM preload
         WHERE media_kind = $1 AND tmdb_id = $2
           AND (
               state = 'ready'
               OR (state <> 'deleted' AND EXISTS (
                   SELECT 1 FROM preload_episode pe WHERE pe.preload_id = preload.id AND pe.state = 'ready'
               ))
           )
         ORDER BY ready_ts DESC",
    )
    .bind(kind.as_str())
    .bind(tmdb_id)
    .fetch_all(pool)
    .await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.try_get("id")).collect::<Result<Vec<i64>, _>>()?;
    let mut ep_map = load_preload_episodes(pool, &ids).await?;
    rows.iter()
        .map(|r| {
            let id: i64 = r.try_get("id")?;
            let ready_episodes = ep_map
                .remove(&id)
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.state == PreloadState::Ready)
                .map(|e| (e.season, e.episode))
                .collect();
            Ok(ReadyPreload {
                infohash: r.try_get("infohash")?,
                season: r.try_get("season")?,
                title: r.try_get("title")?,
                ready_at: r.try_get("ready_ts")?,
                whole_torrent: r.try_get("whole_torrent").unwrap_or(true),
                ready_episodes,
            })
        })
        .collect()
}

/// Distinct `(kind, tmdb_id)` titles currently downloaded and ready on the server, newest first.
pub async fn ready_preloaded_keys(pool: &PgPool) -> Result<Vec<(MediaKind, i64)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT media_kind, tmdb_id
         FROM preload
         WHERE state = 'ready'
         GROUP BY media_kind, tmdb_id
         ORDER BY MAX(COALESCE(ready_at, requested_at)) DESC",
    )
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let kind = kind_of(&r)?;
        let id: i64 = r.try_get("tmdb_id")?;
        out.push((kind, id));
    }
    Ok(out)
}

/// Whether someone else still keeps this release on the server.
pub async fn preload_in_use(pool: &PgPool, infohash: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM preload WHERE infohash = $1 AND state <> 'deleted')")
        .bind(infohash)
        .fetch_one(pool)
        .await
}

/// Whether another preload row (besides `exclude_id`) still uses this release.
pub async fn preload_in_use_except(pool: &PgPool, infohash: &str, exclude_id: i64) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM preload WHERE infohash = $1 AND id <> $2 AND state <> 'deleted')")
        .bind(infohash)
        .bind(exclude_id)
        .fetch_one(pool)
        .await
}

pub async fn preload_mark_deleted(pool: &PgPool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE preload SET state = 'deleted', owner_watched_at = NULL WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE preload_episode SET state = 'deleted', owner_watched_at = NULL WHERE preload_id = $1")
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

/// Series with status "Смотрю", "Жду следующей серии" or "Просмотрено" (a new season may come out).
pub async fn followed_series(pool: &PgPool) -> Result<Vec<Followed>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT s.user_id, u.telegram_id, s.tmdb_id, s.status, s.notified_season, s.notified_episode
         FROM title_state s JOIN users u ON u.id = s.user_id
         WHERE s.media_kind = 'tv' AND s.status IN ('watching', 'waiting_episode', 'watched')",
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
