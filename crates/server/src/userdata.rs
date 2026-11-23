//! Per-user state beyond playback positions: title status and rating, player
//! tracks, manual "watched" marks, viewing history, preferences, preloads.

use chrono::{DateTime, Utc};
use kino_core::{
    meets_average_threshold, EpisodeRating, HistoryEntry, MarkWatched, MediaKind, PreloadItem, PreloadRequest,
    PreloadState, SeasonRating, TitleState, TitleStatus, UserPrefs, VideoScale,
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
                min_seeders, max_size_gb, sort, desktop_quality, mobile_quality
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
                desktop_quality: r.try_get("desktop_quality")?,
                mobile_quality: r.try_get("mobile_quality")?,
            }
        }
        None => UserPrefs::default(),
    })
}

pub async fn set_prefs(pool: &PgPool, user_id: i64, p: &UserPrefs) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_prefs (user_id, quality, voice, subtitle_lang, language, hdr, dolby_vision, subtitles,
                                 codec, direct_play, min_seeders, max_size_gb, sort, desktop_quality, mobile_quality)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
         ON CONFLICT (user_id) DO UPDATE SET
           quality = EXCLUDED.quality, voice = EXCLUDED.voice, subtitle_lang = EXCLUDED.subtitle_lang,
           language = EXCLUDED.language, hdr = EXCLUDED.hdr, dolby_vision = EXCLUDED.dolby_vision,
           subtitles = EXCLUDED.subtitles, codec = EXCLUDED.codec, direct_play = EXCLUDED.direct_play,
           min_seeders = EXCLUDED.min_seeders, max_size_gb = EXCLUDED.max_size_gb, sort = EXCLUDED.sort,
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
    .bind(p.sort.as_deref())
    .bind(p.desktop_quality.as_deref())
    .bind(p.mobile_quality.as_deref())
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
        poster_path: None,
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

/// Once a film or a season is watched, its server preload is shortened to `at` (never exceeding the 7-day ready cap).
pub async fn preload_schedule_removal(
    pool: &PgPool,
    _user_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    at: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE preload
         SET delete_after = LEAST(COALESCE(delete_after, $4), $4)
         WHERE media_kind = $1 AND tmdb_id = $2 AND season = $3 AND state <> 'deleted'",
    )
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
/// Finished downloads are kept on disk for at most 7 days unless watched earlier.
pub async fn preload_progress(
    pool: &PgPool,
    infohash: &str,
    state: PreloadState,
    progress: f32,
    size_bytes: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE preload SET state = $2, progress = $3, size_bytes = COALESCE($4, size_bytes),
             ready_at = CASE WHEN $2 = 'ready' THEN COALESCE(ready_at, now()) ELSE ready_at END,
             delete_after = CASE
                 WHEN $2 = 'ready' THEN LEAST(COALESCE(delete_after, now() + interval '7 days'), now() + interval '7 days')
                 ELSE delete_after
             END
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

/// Ready-to-play server download metadata.
#[derive(Debug, Clone)]
pub struct ReadyPreload {
    pub infohash: String,
    pub season: i32,
    pub title: Option<String>,
    pub ready_at: DateTime<Utc>,
}

/// All ready server preloads for a title, newest finished download first.
pub async fn ready_preloads_for_title(
    pool: &PgPool,
    kind: MediaKind,
    tmdb_id: i64,
) -> Result<Vec<ReadyPreload>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT infohash, season, title, COALESCE(ready_at, requested_at) AS ready_ts
         FROM preload
         WHERE media_kind = $1 AND tmdb_id = $2 AND state = 'ready'
         ORDER BY ready_ts DESC",
    )
    .bind(kind.as_str())
    .bind(tmdb_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|r| {
            Ok(ReadyPreload {
                infohash: r.try_get("infohash")?,
                season: r.try_get("season")?,
                title: r.try_get("title")?,
                ready_at: r.try_get("ready_ts")?,
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
