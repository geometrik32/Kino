//! Social features across users, poster badge enrichment, and release calendar.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use futures_util::stream::{self, StreamExt};
use kino_core::{
    CalendarEntry, MediaKind, PeerBadge, TitleState, TitleStatus, TitleSummary, UserPublicProfile, UserSocialLibrary,
    WatchingNowItem,
};
use sqlx::{PgPool, Row};

use crate::{avatar, catalog::Catalog, sync::SyncHub, userdata};

pub fn display_name(first_name: Option<&str>, username: Option<&str>) -> String {
    if let Some(f) = first_name.map(str::trim).filter(|s| !s.is_empty()) {
        return f.to_owned();
    }
    if let Some(u) = username.map(str::trim).filter(|s| !s.is_empty()) {
        return format!("@{u}");
    }
    "Участник".to_owned()
}

pub fn avatar_url_for(secret: &[u8], user_id: i64, avatar_hash: Option<&str>) -> Option<String> {
    avatar_hash
        .filter(|h| !h.is_empty())
        .map(|h| avatar::path(secret, user_id, h))
}

#[derive(Clone)]
pub struct ActiveWatch {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub season: i32,
    pub episode: i32,
    pub updated_at: DateTime<Utc>,
}

/// Combines active WebSocket sync rooms with very recent watch progress updates.
pub async fn active_watchers_map(pool: &PgPool, sync: &SyncHub) -> HashMap<i64, ActiveWatch> {
    let mut map: HashMap<i64, ActiveWatch> = HashMap::new();
    let now = Utc::now();
    for (uid, (kind, tmdb_id, season, episode, paused)) in sync.active_watchers() {
        if !paused {
            map.insert(
                uid,
                ActiveWatch {
                    kind,
                    tmdb_id,
                    season,
                    episode,
                    updated_at: now,
                },
            );
        }
    }
    if let Ok(rows) = sqlx::query(
        "SELECT DISTINCT ON (user_id) user_id, media_kind, tmdb_id, season, episode, updated_at
         FROM watch_progress
         WHERE updated_at > now() - interval '3 minutes' AND NOT finished
         ORDER BY user_id, updated_at DESC",
    )
    .fetch_all(pool)
    .await
    {
        for r in rows {
            let Ok(uid) = r.try_get::<i64, _>("user_id") else {
                continue;
            };
            if map.contains_key(&uid) {
                continue;
            }
            let Ok(k_raw) = r.try_get::<String, _>("media_kind") else {
                continue;
            };
            let Some(kind) = MediaKind::parse(&k_raw) else { continue };
            if let (Ok(tmdb_id), Ok(season), Ok(episode), Ok(updated_at)) = (
                r.try_get("tmdb_id"),
                r.try_get("season"),
                r.try_get("episode"),
                r.try_get("updated_at"),
            ) {
                map.insert(
                    uid,
                    ActiveWatch {
                        kind,
                        tmdb_id,
                        season,
                        episode,
                        updated_at,
                    },
                );
            }
        }
    }
    map
}

/// Populates `user_rating`, `user_status`, and `peers` across any list of `TitleSummary`.
pub async fn enrich_summaries(
    pool: &PgPool,
    secret: &[u8],
    sync: &SyncHub,
    viewer_id: i64,
    items: &mut [TitleSummary],
) {
    if items.is_empty() {
        return;
    }
    let mut kinds: Vec<String> = Vec::new();
    let mut ids: Vec<i64> = Vec::new();
    let mut seen: HashSet<(MediaKind, i64)> = HashSet::new();
    for item in items.iter() {
        if seen.insert((item.kind, item.tmdb_id)) {
            kinds.push(item.kind.as_str().to_owned());
            ids.push(item.tmdb_id);
        }
    }

    let active = active_watchers_map(pool, sync).await;
    let Ok(rows) = sqlx::query(
        "SELECT ts.user_id, ts.media_kind, ts.tmdb_id, ts.status,
                COALESCE(ts.effective_rating, ts.rating::real) AS score,
                u.first_name, u.username, u.avatar_hash
         FROM title_state ts
         JOIN users u ON u.id = ts.user_id
         JOIN UNNEST($1::text[], $2::bigint[]) AS q(k, id)
           ON ts.media_kind = q.k AND ts.tmdb_id = q.id
         ORDER BY ts.updated_at DESC",
    )
    .bind(&kinds)
    .bind(&ids)
    .fetch_all(pool)
    .await
    else {
        return;
    };

    let mut mine: HashMap<(MediaKind, i64), (Option<TitleStatus>, Option<f32>)> = HashMap::new();
    let mut peers_map: HashMap<(MediaKind, i64), Vec<PeerBadge>> = HashMap::new();

    for r in rows {
        let Ok(uid) = r.try_get::<i64, _>("user_id") else {
            continue;
        };
        let Ok(k_raw) = r.try_get::<String, _>("media_kind") else {
            continue;
        };
        let Some(kind) = MediaKind::parse(&k_raw) else { continue };
        let Ok(tmdb_id) = r.try_get::<i64, _>("tmdb_id") else {
            continue;
        };
        let status = r
            .try_get::<Option<String>, _>("status")
            .ok()
            .flatten()
            .and_then(|s| TitleStatus::parse(&s));
        let rating = r.try_get::<Option<f32>, _>("score").ok().flatten();

        if uid == viewer_id {
            mine.insert((kind, tmdb_id), (status, rating));
            continue;
        }
        let watching_now = active.get(&uid).is_some_and(|w| w.kind == kind && w.tmdb_id == tmdb_id);
        // Only surface peers who actually watched/watching/rated/want (not merely dropped silently).
        if status.is_none() && rating.is_none() && !watching_now {
            continue;
        }
        let first_name: Option<String> = r.try_get("first_name").ok().flatten();
        let username: Option<String> = r.try_get("username").ok().flatten();
        let avatar_hash: Option<String> = r.try_get("avatar_hash").ok().flatten();
        peers_map.entry((kind, tmdb_id)).or_default().push(PeerBadge {
            user_id: uid,
            name: display_name(first_name.as_deref(), username.as_deref()),
            avatar_url: avatar_url_for(secret, uid, avatar_hash.as_deref()),
            status,
            rating,
            watching_now,
        });
    }

    // Ensure peers who are actively watching right now appear even before title_state was touched.
    let active_missing: Vec<i64> = active
        .iter()
        .filter(|(&uid, w)| {
            uid != viewer_id
                && seen.contains(&(w.kind, w.tmdb_id))
                && !peers_map
                    .get(&(w.kind, w.tmdb_id))
                    .is_some_and(|list| list.iter().any(|p| p.user_id == uid))
        })
        .map(|(&uid, _)| uid)
        .collect();
    if !active_missing.is_empty() {
        if let Ok(urows) = sqlx::query("SELECT id, first_name, username, avatar_hash FROM users WHERE id = ANY($1)")
            .bind(&active_missing)
            .fetch_all(pool)
            .await
        {
            for r in urows {
                let Ok(uid) = r.try_get::<i64, _>("id") else { continue };
                let Some(w) = active.get(&uid) else { continue };
                let first_name: Option<String> = r.try_get("first_name").ok().flatten();
                let username: Option<String> = r.try_get("username").ok().flatten();
                let avatar_hash: Option<String> = r.try_get("avatar_hash").ok().flatten();
                peers_map.entry((w.kind, w.tmdb_id)).or_default().push(PeerBadge {
                    user_id: uid,
                    name: display_name(first_name.as_deref(), username.as_deref()),
                    avatar_url: avatar_url_for(secret, uid, avatar_hash.as_deref()),
                    status: Some(TitleStatus::Watching),
                    rating: None,
                    watching_now: true,
                });
            }
        }
    }

    for item in items.iter_mut() {
        if let Some(&(st, rt)) = mine.get(&(item.kind, item.tmdb_id)) {
            item.user_status = st;
            item.user_rating = rt;
        }
        if let Some(peers) = peers_map.get(&(item.kind, item.tmdb_id)) {
            item.peers = peers.clone();
        }
    }
}

/// Peers' activity for a single title card.
pub async fn title_peers(
    pool: &PgPool,
    secret: &[u8],
    sync: &SyncHub,
    viewer_id: i64,
    kind: MediaKind,
    tmdb_id: i64,
) -> Vec<PeerBadge> {
    let mut dummy = [TitleSummary {
        kind,
        tmdb_id,
        title: String::new(),
        original_title: None,
        year: None,
        poster_path: None,
        backdrop_path: None,
        rating: None,
        overview: None,
        user_rating: None,
        user_status: None,
        peers: Vec::new(),
    }];
    enrich_summaries(pool, secret, sync, viewer_id, &mut dummy).await;
    dummy.into_iter().next().map(|s| s.peers).unwrap_or_default()
}

/// Keys of titles other users are watching or rated highly, excluding what `viewer_id` has already finished/excluded.
pub async fn peers_recommendation_keys(
    pool: &PgPool,
    sync: &SyncHub,
    viewer_id: i64,
    excluded: &HashSet<(MediaKind, i64)>,
    limit: usize,
) -> Result<Vec<(MediaKind, i64)>, sqlx::Error> {
    let active = active_watchers_map(pool, sync).await;
    let mut keys: Vec<(MediaKind, i64)> = Vec::new();
    let mut seen: HashSet<(MediaKind, i64)> = HashSet::new();

    for (&uid, w) in &active {
        if uid != viewer_id && !excluded.contains(&(w.kind, w.tmdb_id)) && seen.insert((w.kind, w.tmdb_id)) {
            keys.push((w.kind, w.tmdb_id));
        }
    }

    let rows = sqlx::query(
        "SELECT media_kind, tmdb_id,
                MAX(COALESCE(effective_rating, rating::real)) AS max_rating,
                MAX(updated_at) AS last_at
         FROM title_state
         WHERE user_id <> $1
           AND (COALESCE(effective_rating, rating::real) >= 4.0 OR status IN ('watching', 'watched', 'waiting_episode'))
         GROUP BY media_kind, tmdb_id
         ORDER BY MAX(CASE WHEN COALESCE(effective_rating, rating::real) >= 4.0 THEN 1 ELSE 0 END) DESC,
                  MAX(updated_at) DESC
         LIMIT 60",
    )
    .bind(viewer_id)
    .fetch_all(pool)
    .await?;

    for r in rows {
        if keys.len() >= limit {
            break;
        }
        let k_raw: String = r.try_get("media_kind")?;
        let Some(kind) = MediaKind::parse(&k_raw) else {
            continue;
        };
        let tmdb_id: i64 = r.try_get("tmdb_id")?;
        if !excluded.contains(&(kind, tmdb_id)) && seen.insert((kind, tmdb_id)) {
            keys.push((kind, tmdb_id));
        }
    }
    keys.truncate(limit);
    Ok(keys)
}

pub async fn list_users_public(
    pool: &PgPool,
    secret: &[u8],
    sync: &SyncHub,
    catalog: &dyn Catalog,
    viewer_id: i64,
) -> Result<Vec<UserPublicProfile>, sqlx::Error> {
    let active = active_watchers_map(pool, sync).await;
    let rows = sqlx::query(
        "SELECT u.id, u.first_name, u.username, u.avatar_hash,
                COUNT(*) FILTER (WHERE ts.status = 'watched') AS watched_count,
                COUNT(*) FILTER (WHERE ts.status = 'watching') AS watching_count,
                COUNT(*) FILTER (WHERE ts.status = 'want') AS want_count,
                COUNT(*) FILTER (WHERE ts.status IN ('waiting_episode', 'waiting_release')) AS waiting_count
         FROM users u
         LEFT JOIN title_state ts ON ts.user_id = u.id
         WHERE u.id <> $1
         GROUP BY u.id
         ORDER BY u.last_seen_at DESC",
    )
    .bind(viewer_id)
    .fetch_all(pool)
    .await?;

    let mut profiles: Vec<UserPublicProfile> = Vec::with_capacity(rows.len());
    let mut active_keys: Vec<(usize, MediaKind, i64)> = Vec::new();
    for r in rows {
        let uid: i64 = r.try_get("id")?;
        let first_name: Option<String> = r.try_get("first_name")?;
        let username: Option<String> = r.try_get("username")?;
        let avatar_hash: Option<String> = r.try_get("avatar_hash")?;
        let watching_now = active.get(&uid).map(|w| WatchingNowItem {
            kind: w.kind,
            tmdb_id: w.tmdb_id,
            season: w.season,
            episode: w.episode,
            title: None,
            updated_at: w.updated_at,
        });
        if let Some(w) = &watching_now {
            active_keys.push((profiles.len(), w.kind, w.tmdb_id));
        }
        profiles.push(UserPublicProfile {
            id: uid,
            name: display_name(first_name.as_deref(), username.as_deref()),
            username,
            avatar_url: avatar_url_for(secret, uid, avatar_hash.as_deref()),
            watching_now,
            watched_count: r.try_get("watched_count")?,
            watching_count: r.try_get("watching_count")?,
            want_count: r.try_get("want_count")?,
            waiting_count: r.try_get("waiting_count")?,
        });
    }

    let fetched = stream::iter(active_keys.into_iter().map(|(idx, kind, id)| async move {
        let title = catalog.details(kind, id).await.ok().map(|d| d.summary);
        (idx, title)
    }))
    .buffer_unordered(6)
    .collect::<Vec<_>>()
    .await;

    for (idx, title) in fetched {
        if let Some(w) = &mut profiles[idx].watching_now {
            w.title = title;
        }
    }
    Ok(profiles)
}

pub async fn user_public_library(
    pool: &PgPool,
    secret: &[u8],
    sync: &SyncHub,
    catalog: &dyn Catalog,
    viewer_id: i64,
    target_user_id: i64,
) -> Result<Option<UserSocialLibrary>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT u.id, u.first_name, u.username, u.avatar_hash,
                COUNT(*) FILTER (WHERE ts.status = 'watched') AS watched_count,
                COUNT(*) FILTER (WHERE ts.status = 'watching') AS watching_count,
                COUNT(*) FILTER (WHERE ts.status = 'want') AS want_count,
                COUNT(*) FILTER (WHERE ts.status IN ('waiting_episode', 'waiting_release')) AS waiting_count
         FROM users u
         LEFT JOIN title_state ts ON ts.user_id = u.id
         WHERE u.id = $1
         GROUP BY u.id",
    )
    .bind(target_user_id)
    .fetch_optional(pool)
    .await?;
    let Some(r) = row else {
        return Ok(None);
    };
    let active = active_watchers_map(pool, sync).await;
    let first_name: Option<String> = r.try_get("first_name")?;
    let username: Option<String> = r.try_get("username")?;
    let avatar_hash: Option<String> = r.try_get("avatar_hash")?;
    let mut watching_now = active.get(&target_user_id).map(|w| WatchingNowItem {
        kind: w.kind,
        tmdb_id: w.tmdb_id,
        season: w.season,
        episode: w.episode,
        title: None,
        updated_at: w.updated_at,
    });
    if let Some(w) = &mut watching_now {
        w.title = catalog.details(w.kind, w.tmdb_id).await.ok().map(|d| d.summary);
    }
    let profile = UserPublicProfile {
        id: target_user_id,
        name: display_name(first_name.as_deref(), username.as_deref()),
        username,
        avatar_url: avatar_url_for(secret, target_user_id, avatar_hash.as_deref()),
        watching_now,
        watched_count: r.try_get("watched_count")?,
        watching_count: r.try_get("watching_count")?,
        want_count: r.try_get("want_count")?,
        waiting_count: r.try_get("waiting_count")?,
    };

    let mut items: Vec<TitleState> = userdata::list_states(pool, target_user_id, None).await?;
    let keys: Vec<(usize, MediaKind, i64)> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (i, it.kind, it.tmdb_id))
        .collect();
    let summaries = stream::iter(keys.into_iter().map(|(i, kind, id)| async move {
        let title = catalog.details(kind, id).await.ok().map(|d| d.summary);
        (i, title)
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    for (i, title) in summaries {
        items[i].title = title;
    }
    let mut sum_refs: Vec<TitleSummary> = items.iter().filter_map(|it| it.title.clone()).collect();
    enrich_summaries(pool, secret, sync, viewer_id, &mut sum_refs).await;
    let map: HashMap<(MediaKind, i64), TitleSummary> = sum_refs.into_iter().map(|s| ((s.kind, s.tmdb_id), s)).collect();
    for it in &mut items {
        if let Some(s) = map.get(&(it.kind, it.tmdb_id)) {
            it.title = Some(s.clone());
        }
    }
    Ok(Some(UserSocialLibrary { profile, items }))
}

/// Builds upcoming release dates for the user's watched/waiting series and expected movies.
pub async fn calendar_entries(
    pool: &PgPool,
    secret: &[u8],
    sync: &SyncHub,
    catalog: &dyn Catalog,
    user_id: i64,
    today_ymd: &str,
) -> Result<Vec<CalendarEntry>, sqlx::Error> {
    let states = userdata::list_states(pool, user_id, None).await?;
    let targets: Vec<(MediaKind, i64, TitleStatus)> = states
        .into_iter()
        .filter_map(|st| {
            let status = st.status?;
            let keep = matches!(
                (st.kind, status),
                (
                    MediaKind::Tv,
                    TitleStatus::Watching | TitleStatus::WaitingEpisode | TitleStatus::Want
                ) | (MediaKind::Movie, TitleStatus::WaitingRelease | TitleStatus::Want)
            );
            keep.then_some((st.kind, st.tmdb_id, status))
        })
        .collect();

    let fetched = stream::iter(targets.into_iter().map(|(kind, id, status)| async move {
        let details = catalog.details(kind, id).await.ok();
        (kind, id, status, details)
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;

    let mut entries: Vec<CalendarEntry> = Vec::new();
    let mut summaries: Vec<TitleSummary> = Vec::new();

    for (kind, id, status, details) in fetched {
        let Some(d) = details else { continue };
        match kind {
            MediaKind::Tv => {
                if let Some(next) = &d.next_episode {
                    if let Some(date) = &next.air_date {
                        if date.as_str() >= today_ymd {
                            summaries.push(d.summary.clone());
                            entries.push(CalendarEntry {
                                date: date.clone(),
                                kind,
                                tmdb_id: id,
                                season: Some(next.season),
                                episode: Some(next.episode),
                                episode_name: next.name.clone(),
                                title: Some(d.summary),
                                status,
                            });
                        }
                    }
                }
            }
            MediaKind::Movie => {
                if let Some(date) = &d.release_date {
                    if date.as_str() >= today_ymd
                        && (status == TitleStatus::WaitingRelease || status == TitleStatus::Want)
                    {
                        summaries.push(d.summary.clone());
                        entries.push(CalendarEntry {
                            date: date.clone(),
                            kind,
                            tmdb_id: id,
                            season: None,
                            episode: None,
                            episode_name: None,
                            title: Some(d.summary),
                            status,
                        });
                    }
                }
            }
        }
    }

    enrich_summaries(pool, secret, sync, user_id, &mut summaries).await;
    let map: HashMap<(MediaKind, i64), TitleSummary> =
        summaries.into_iter().map(|s| ((s.kind, s.tmdb_id), s)).collect();
    for e in &mut entries {
        if let Some(s) = map.get(&(e.kind, e.tmdb_id)) {
            e.title = Some(s.clone());
        }
    }
    entries.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.tmdb_id.cmp(&b.tmdb_id)));
    Ok(entries)
}
