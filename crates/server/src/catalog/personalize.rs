//! Per-user personalization and shelf ranking for the home screen.

use std::collections::{HashMap, HashSet};

use futures_util::future::join_all;
use kino_core::{CatalogRow, HomeCategory, MediaKind, ShelfPage, TitleDetails, TitleExtras, TitleStatus, TitleSummary};

use super::{Catalog, CatalogError};
use crate::userdata::UserTitleSignal;

/// Number of shelves loaded immediately on the first home response.
pub const INITIAL_HOME_ROWS: usize = 5;

const MAX_PERSONAL_ROWS: usize = 2;
const PROFILE_DETAILS_LIMIT: usize = 16;
const SEED_CANDIDATES_LIMIT: usize = 4;
const MIN_BECAUSE_ITEMS: usize = 4;
const BECAUSE_PREFIX: &str = "because:";

/// Ordered category chips offered on the home screen.
const CATEGORY_CATALOG: [(&str, &str); 12] = [
    ("for_you", "Для вас"),
    ("rewatch", "Пересматривай любимое"),
    ("tv", "Сериалы"),
    ("movies", "Фильмы"),
    ("fresh", "Новинки"),
    ("animation", "Аниме и мультфильмы"),
    ("scifi", "Фантастика"),
    ("thriller", "Триллеры и детективы"),
    ("comedy", "Комедии"),
    ("action", "Боевики"),
    ("drama", "Драмы"),
    ("ru", "Русское"),
];

/// Static metadata for an editorial or genre shelf.
#[derive(Debug, Clone, Copy)]
pub struct ShelfMeta {
    pub id: &'static str,
    pub title: &'static str,
    pub subtitle: Option<&'static str>,
    pub categories: &'static [&'static str],
    pub kind: Option<MediaKind>,
    pub genre_tags: &'static [&'static str],
    pub base_priority: i32,
}

/// Derived user preferences used to tailor and filter the home feed.
#[derive(Debug, Clone, Default)]
pub struct UserTaste {
    /// Titles already watched or dropped: hidden from discovery shelves.
    pub excluded: HashSet<(MediaKind, i64)>,
    /// Titles currently being watched: kept out of "because you watched" recommendations.
    pub watching: HashSet<(MediaKind, i64)>,
    /// Up to two personalized "Because you watched ..." shelves.
    pub personal_rows: Vec<CatalogRow>,
    /// Normalized lowercase genre affinity weights.
    pub genre_scores: HashMap<String, i32>,
    pub movie_bias: i32,
    pub tv_bias: i32,
}

/// Builds fast exclusion sets from DB signals without making catalog API calls.
pub fn exclusions_only(signals: &[UserTitleSignal]) -> UserTaste {
    let mut excluded = HashSet::new();
    let mut watching = HashSet::new();
    for s in signals {
        let key = (s.kind, s.tmdb_id);
        let is_excluded = s.rating.is_some()
            || matches!(
                s.status,
                Some(
                    TitleStatus::Watched
                        | TitleStatus::Dropped
                        | TitleStatus::WaitingEpisode
                        | TitleStatus::WaitingRelease
                )
            )
            || (s.kind == MediaKind::Movie && s.finished);
        if is_excluded {
            excluded.insert(key);
        } else if s.status == Some(TitleStatus::Watching) {
            watching.insert(key);
        }
    }
    UserTaste {
        excluded,
        watching,
        ..Default::default()
    }
}

/// Builds the full taste profile for `/api/catalog/home`, fetching metadata for recent/liked titles.
pub async fn build_taste(catalog: &dyn Catalog, signals: &[UserTitleSignal]) -> UserTaste {
    let mut taste = exclusions_only(signals);
    if signals.is_empty() {
        return taste;
    }

    let mut scored: Vec<(usize, i32, &UserTitleSignal)> = signals
        .iter()
        .enumerate()
        .map(|(idx, s)| (idx, signal_weight(s, idx), s))
        .collect();
    // Sort by absolute impact so both strong likes and strong dislikes shape genre weights.
    scored.sort_by(|a, b| b.1.abs().cmp(&a.1.abs()).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(PROFILE_DETAILS_LIMIT);

    let details_list = join_all(scored.iter().map(|(_, _, s)| catalog.details(s.kind, s.tmdb_id))).await;

    let mut details_map: HashMap<(MediaKind, i64), TitleDetails> = HashMap::new();
    for ((_, weight, sig), res) in scored.iter().zip(details_list) {
        let Ok(details) = res else { continue };
        match sig.kind {
            MediaKind::Movie => taste.movie_bias += *weight,
            MediaKind::Tv => taste.tv_bias += *weight,
        }
        for genre in &details.genres {
            let norm = genre.trim().to_lowercase();
            if !norm.is_empty() {
                *taste.genre_scores.entry(norm).or_insert(0) += *weight;
            }
        }
        details_map.insert((sig.kind, sig.tmdb_id), details);
    }

    let seeds = select_seed_candidates(signals, &details_map);
    if seeds.is_empty() {
        return taste;
    }

    let extras_list = join_all(seeds.iter().map(|s| catalog.extras(s.kind, s.tmdb_id))).await;
    for (sig, res) in seeds.into_iter().zip(extras_list) {
        if taste.personal_rows.len() >= MAX_PERSONAL_ROWS {
            break;
        }
        let Ok(extras) = res else { continue };
        let Some(details) = details_map.get(&(sig.kind, sig.tmdb_id)) else {
            continue;
        };
        if let Some(row) = build_because_row(sig, details, extras, &taste) {
            taste.personal_rows.push(row);
        }
    }

    taste
}

fn signal_weight(s: &UserTitleSignal, recency_idx: usize) -> i32 {
    if s.status == Some(TitleStatus::Dropped) || s.rating.is_some_and(|r| r <= 2) {
        return -35;
    }
    let mut score = 10;
    score += match s.rating {
        Some(5) => 100,
        Some(4) => 65,
        Some(3) => 15,
        _ => 0,
    };
    score += match s.status {
        Some(TitleStatus::Watched) => 45,
        Some(TitleStatus::WaitingEpisode) => 42,
        Some(TitleStatus::Watching) => 35,
        Some(TitleStatus::Want | TitleStatus::WaitingRelease) => 20,
        _ => 0,
    };
    if s.kind == MediaKind::Movie && s.finished {
        score += 20;
    }
    score + (20_i32 - recency_idx.min(20) as i32)
}

fn select_seed_candidates<'a>(
    signals: &'a [UserTitleSignal],
    details_map: &HashMap<(MediaKind, i64), TitleDetails>,
) -> Vec<&'a UserTitleSignal> {
    let mut positive: Vec<(i32, &'a UserTitleSignal)> = signals
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.status != Some(TitleStatus::Dropped)
                && s.status != Some(TitleStatus::Want)
                && !s.rating.is_some_and(|r| r <= 2)
                && details_map.contains_key(&(s.kind, s.tmdb_id))
        })
        .map(|(idx, s)| (signal_weight(s, idx), s))
        .collect();
    positive.sort_by_key(|a| std::cmp::Reverse(a.0));

    // Prefer format diversity among top seeds when the user watched both movies and series.
    let mut picked: Vec<&'a UserTitleSignal> = Vec::new();
    if let Some((_, first)) = positive.first() {
        picked.push(*first);
        if let Some((_, other_kind)) = positive.iter().find(|(_, s)| s.kind != first.kind) {
            picked.push(*other_kind);
        }
        for (_, cand) in positive {
            if picked.len() >= SEED_CANDIDATES_LIMIT {
                break;
            }
            if !picked.iter().any(|p| p.kind == cand.kind && p.tmdb_id == cand.tmdb_id) {
                picked.push(cand);
            }
        }
    }
    picked
}

fn because_subtitle(sig: &UserTitleSignal) -> String {
    if let Some(r) = sig.rating.filter(|r| *r >= 4) {
        return format!("На основе вашей оценки ★{r}");
    }
    match sig.status {
        Some(TitleStatus::Watched) => "По просмотренному".into(),
        Some(TitleStatus::Watching) => "Похоже на то, что вы смотрите".into(),
        _ => "По вашим интересам".into(),
    }
}

fn merge_because_items(extras: TitleExtras, taste: &UserTaste) -> Vec<TitleSummary> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for candidate in extras.recommendations.into_iter().chain(extras.similar) {
        let key = (candidate.kind, candidate.tmdb_id);
        if taste.excluded.contains(&key) || taste.watching.contains(&key) || !seen.insert(key) {
            continue;
        }
        items.push(candidate);
    }
    items
}

fn make_because_row(
    kind: MediaKind,
    tmdb_id: i64,
    title: &str,
    subtitle: String,
    items: Vec<TitleSummary>,
) -> CatalogRow {
    let kind_cat = match kind {
        MediaKind::Movie => "movies",
        MediaKind::Tv => "tv",
    };
    CatalogRow {
        id: format!("{BECAUSE_PREFIX}{}:{tmdb_id}", kind.as_str()),
        title: format!("Потому что вы смотрели «{title}»"),
        subtitle: Some(subtitle),
        categories: vec!["for_you".into(), kind_cat.into()],
        loaded: true,
        has_more: false,
        page: 1,
        items,
    }
}

fn build_because_row(
    sig: &UserTitleSignal,
    details: &TitleDetails,
    extras: TitleExtras,
    taste: &UserTaste,
) -> Option<CatalogRow> {
    let items = merge_because_items(extras, taste);
    if items.len() < MIN_BECAUSE_ITEMS {
        return None;
    }
    Some(make_because_row(
        sig.kind,
        sig.tmdb_id,
        &details.summary.title,
        because_subtitle(sig),
        items,
    ))
}

/// Parses `because:{movie|tv}:{tmdb_id}` shelf identifiers.
pub fn parse_because_id(id: &str) -> Option<(MediaKind, i64)> {
    let rest = id.strip_prefix(BECAUSE_PREFIX)?;
    let (kind_raw, id_raw) = rest.split_once(':')?;
    let kind = MediaKind::parse(kind_raw)?;
    let tmdb_id = id_raw.parse::<i64>().ok().filter(|n| *n > 0)?;
    Some((kind, tmdb_id))
}

/// Resolves a dynamic `because:...` shelf on demand.
pub async fn because_shelf_row(
    catalog: &dyn Catalog,
    kind: MediaKind,
    tmdb_id: i64,
    taste: &UserTaste,
) -> Result<CatalogRow, CatalogError> {
    let (details, extras) = tokio::try_join!(catalog.details(kind, tmdb_id), catalog.extras(kind, tmdb_id))?;
    let items = merge_because_items(extras, taste);
    Ok(make_because_row(
        kind,
        tmdb_id,
        &details.summary.title,
        "Персональная подборка".into(),
        items,
    ))
}

/// Resolves a dynamic `because:...` shelf for the full-grid `ShelfView`.
pub async fn because_shelf_page(
    catalog: &dyn Catalog,
    kind: MediaKind,
    tmdb_id: i64,
    taste: &UserTaste,
) -> Result<ShelfPage, CatalogError> {
    let row = because_shelf_row(catalog, kind, tmdb_id, taste).await?;
    Ok(ShelfPage {
        id: row.id,
        title: row.title,
        subtitle: row.subtitle,
        page: 1,
        has_more: false,
        items: row.items,
    })
}

/// Removes titles the user already watched or dropped.
pub fn filter_excluded(items: Vec<TitleSummary>, excluded: &HashSet<(MediaKind, i64)>) -> Vec<TitleSummary> {
    if excluded.is_empty() {
        return items;
    }
    items
        .into_iter()
        .filter(|i| !excluded.contains(&(i.kind, i.tmdb_id)))
        .collect()
}

fn genre_affinity(tags: &[&str], genre_scores: &HashMap<String, i32>) -> i32 {
    if tags.is_empty() || genre_scores.is_empty() {
        return 0;
    }
    let mut best: i32 = 0;
    for tag in tags {
        for (genre, score) in genre_scores {
            let matched = genre.contains(tag)
                || (*tag == "фантастика" && genre.contains("нф"))
                || (*tag == "аниме" && genre.contains("мультфильм"));
            if matched && score.abs() > best.abs() {
                best = *score;
            }
        }
    }
    // Scale genre weight so strong affinity outranks base priority without drowning top trends.
    (best / 3).clamp(-30, 50)
}

fn format_bonus(kind: Option<MediaKind>, taste: &UserTaste) -> i32 {
    let diff = taste.tv_bias - taste.movie_bias;
    if diff.abs() < 25 {
        return 0;
    }
    let boost = (diff.abs() / 10).clamp(4, 14);
    match (kind, diff > 0) {
        (Some(MediaKind::Tv), true) | (Some(MediaKind::Movie), false) => boost,
        (Some(MediaKind::Tv), false) | (Some(MediaKind::Movie), true) => -boost / 2,
        _ => 0,
    }
}

/// Ranks shelves for a user and lightly balances format diversity near the top.
pub fn rank_shelves<'a>(shelves: &'a [ShelfMeta], taste: &UserTaste) -> Vec<&'a ShelfMeta> {
    let mut scored: Vec<(i32, usize, &'a ShelfMeta)> = shelves
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            let score =
                s.base_priority + format_bonus(s.kind, taste) + genre_affinity(s.genre_tags, &taste.genre_scores);
            (score, idx, s)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let mut ordered: Vec<&'a ShelfMeta> = scored.into_iter().map(|(_, _, s)| s).collect();
    // Avoid stacking three identical formats consecutively at the very top when both exist.
    if ordered.len() >= 4 {
        for i in 2..ordered.len().min(8) {
            if ordered[i - 2].kind.is_some()
                && ordered[i - 2].kind == ordered[i - 1].kind
                && ordered[i - 1].kind == ordered[i].kind
            {
                if let Some(swap_idx) = ((i + 1)..ordered.len()).find(|&j| ordered[j].kind != ordered[i].kind) {
                    ordered.swap(i, swap_idx);
                }
            }
        }
    }
    ordered
}

/// Converts an editorial shelf descriptor into a deferred (`loaded: false`) row stub.
pub fn deferred_stub(meta: &ShelfMeta) -> CatalogRow {
    CatalogRow {
        id: meta.id.to_owned(),
        title: meta.title.to_owned(),
        subtitle: meta.subtitle.map(str::to_owned),
        categories: meta.categories.iter().map(|c| (*c).to_owned()).collect(),
        loaded: false,
        has_more: true,
        page: 0,
        items: Vec::new(),
    }
}

/// Builds category filter chips containing only categories present in `rows`.
pub fn categories_for(rows: &[CatalogRow]) -> Vec<HomeCategory> {
    CATEGORY_CATALOG
        .iter()
        .filter(|(id, _)| rows.iter().any(|r| r.categories.iter().any(|c| c == id)))
        .map(|(id, label)| HomeCategory {
            id: (*id).to_owned(),
            label: (*label).to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(kind: MediaKind, id: i64, title: &str) -> TitleSummary {
        TitleSummary {
            kind,
            tmdb_id: id,
            title: title.into(),
            original_title: None,
            year: Some(2020),
            poster_path: Some("/p.jpg".into()),
            backdrop_path: None,
            rating: Some(8.0),
            overview: None,
            user_rating: None,
            user_status: None,
            peers: Vec::new(),
        }
    }

    #[test]
    fn exclusions_hide_watched_and_dropped_only() {
        let signals = vec![
            UserTitleSignal {
                kind: MediaKind::Movie,
                tmdb_id: 1,
                status: None,
                rating: None,
                finished: true,
            },
            UserTitleSignal {
                kind: MediaKind::Tv,
                tmdb_id: 2,
                status: Some(TitleStatus::Dropped),
                rating: None,
                finished: false,
            },
            UserTitleSignal {
                kind: MediaKind::Tv,
                tmdb_id: 3,
                status: Some(TitleStatus::Watching),
                rating: None,
                finished: true,
            },
            UserTitleSignal {
                kind: MediaKind::Movie,
                tmdb_id: 4,
                status: Some(TitleStatus::Want),
                rating: None,
                finished: false,
            },
        ];
        let taste = exclusions_only(&signals);
        assert!(taste.excluded.contains(&(MediaKind::Movie, 1)));
        assert!(taste.excluded.contains(&(MediaKind::Tv, 2)));
        assert!(!taste.excluded.contains(&(MediaKind::Tv, 3)));
        assert!(taste.watching.contains(&(MediaKind::Tv, 3)));
        assert!(!taste.excluded.contains(&(MediaKind::Movie, 4)));

        let items = vec![
            summary(MediaKind::Movie, 1, "Watched movie"),
            summary(MediaKind::Tv, 2, "Dropped series"),
            summary(MediaKind::Tv, 3, "Watching series"),
            summary(MediaKind::Movie, 4, "Want movie"),
        ];
        let filtered = filter_excluded(items, &taste.excluded);
        assert_eq!(filtered.iter().map(|i| i.tmdb_id).collect::<Vec<_>>(), vec![3, 4]);
    }

    #[test]
    fn ranks_shelves_by_genre_and_format_affinity() {
        let shelves = [
            ShelfMeta {
                id: "comedy",
                title: "Комедии",
                subtitle: None,
                categories: &["for_you", "movies", "comedy"],
                kind: Some(MediaKind::Movie),
                genre_tags: &["комедия"],
                base_priority: 80,
            },
            ShelfMeta {
                id: "scifi_tv",
                title: "Фантастические сериалы",
                subtitle: None,
                categories: &["for_you", "tv", "scifi"],
                kind: Some(MediaKind::Tv),
                genre_tags: &["фантастика"],
                base_priority: 70,
            },
        ];
        let mut taste = UserTaste {
            tv_bias: 120,
            movie_bias: 10,
            ..Default::default()
        };
        taste.genre_scores.insert("нф и фэнтези".into(), 120);
        let ranked = rank_shelves(&shelves, &taste);
        assert_eq!(ranked[0].id, "scifi_tv");
    }

    #[test]
    fn parses_because_ids() {
        assert_eq!(parse_because_id("because:tv:1396"), Some((MediaKind::Tv, 1396)));
        assert_eq!(parse_because_id("because:movie:603"), Some((MediaKind::Movie, 603)));
        assert_eq!(parse_because_id("because:anime:1"), None);
        assert_eq!(parse_because_id("trending_tv"), None);
    }

    #[test]
    fn catalog_row_serde_defaults() {
        let legacy = serde_json::json!({ "title": "Фильмы", "items": [] });
        let row: CatalogRow = serde_json::from_value(legacy).unwrap();
        assert!(row.loaded);
        assert_eq!(row.page, 1);
        assert!(!row.has_more);
        assert!(row.id.is_empty());
    }
}
