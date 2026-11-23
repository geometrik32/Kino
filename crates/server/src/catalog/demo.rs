//! Built-in demo catalog: a handful of well-known titles, no posters.
//! Used when `TMDB_API_KEY` is not set, so the interface can be tried anywhere.

use async_trait::async_trait;
use kino_core::{
    CastMember, CatalogRow, Episode, HomeFeed, MediaKind, PersonDetails, SearchResults, Season, SeasonSummary,
    ShelfPage, TitleDetails, TitleExtras, TitleSummary,
};
use rand::Rng;

use super::{
    matches_normalized_query, normalize_search_text,
    personalize::{
        because_shelf_page, because_shelf_row, categories_for, deferred_stub, filter_excluded, parse_because_id,
        rank_shelves, ShelfMeta, UserTaste, INITIAL_HOME_ROWS,
    },
    split_query_year, Catalog, CatalogError, RandomMovieFilter, RandomMoviePick,
};

struct DemoTitle {
    kind: MediaKind,
    id: i64,
    title: &'static str,
    original: &'static str,
    year: i32,
    rating: f32,
    genres: &'static [&'static str],
    runtime: i32,
    overview: &'static str,
    /// Episode count per season (series only).
    seasons: &'static [i32],
}

const M: MediaKind = MediaKind::Movie;
const T: MediaKind = MediaKind::Tv;

#[rustfmt::skip]
const TITLES: &[DemoTitle] = &[
    DemoTitle { kind: M, id: 603, title: "Матрица", original: "The Matrix", year: 1999, rating: 8.2, genres: &["фантастика", "боевик"], runtime: 136,
        overview: "Хакер Нео узнаёт, что привычный мир — симуляция, и присоединяется к восстанию против машин.", seasons: &[] },
    DemoTitle { kind: M, id: 27205, title: "Начало", original: "Inception", year: 2010, rating: 8.4, genres: &["фантастика", "триллер"], runtime: 148,
        overview: "Вор, крадущий идеи из снов, получает задание наоборот: внедрить мысль в чужое подсознание.", seasons: &[] },
    DemoTitle { kind: M, id: 157336, title: "Интерстеллар", original: "Interstellar", year: 2014, rating: 8.4, genres: &["фантастика", "драма"], runtime: 169,
        overview: "Экспедиция отправляется через червоточину в поисках нового дома для человечества.", seasons: &[] },
    DemoTitle { kind: M, id: 155, title: "Тёмный рыцарь", original: "The Dark Knight", year: 2008, rating: 8.5, genres: &["боевик", "криминал"], runtime: 152,
        overview: "Бэтмен сталкивается с Джокером, который хочет погрузить Готэм в хаос.", seasons: &[] },
    DemoTitle { kind: M, id: 438631, title: "Дюна", original: "Dune", year: 2021, rating: 7.8, genres: &["фантастика", "приключения"], runtime: 155,
        overview: "Наследник дома Атрейдес оказывается на самой опасной планете вселенной.", seasons: &[] },
    DemoTitle { kind: M, id: 680, title: "Криминальное чтиво", original: "Pulp Fiction", year: 1994, rating: 8.5, genres: &["криминал", "триллер"], runtime: 154,
        overview: "Несколько историй из жизни лос-анджелесских бандитов переплетаются в одну.", seasons: &[] },
    DemoTitle { kind: M, id: 13, title: "Форрест Гамп", original: "Forrest Gump", year: 1994, rating: 8.5, genres: &["драма", "мелодрама"], runtime: 142,
        overview: "Простодушный Форрест проходит через главные события американской истории.", seasons: &[] },
    DemoTitle { kind: M, id: 550, title: "Бойцовский клуб", original: "Fight Club", year: 1999, rating: 8.4, genres: &["драма", "триллер"], runtime: 139,
        overview: "Офисный работник и торговец мылом создают подпольный клуб.", seasons: &[] },
    DemoTitle { kind: T, id: 1396, title: "Во все тяжкие", original: "Breaking Bad", year: 2008, rating: 8.9, genres: &["драма", "криминал"], runtime: 47,
        overview: "Учитель химии с тяжёлым диагнозом начинает варить метамфетамин.", seasons: &[7, 13, 13, 13, 16] },
    DemoTitle { kind: T, id: 70523, title: "Тьма", original: "Dark", year: 2017, rating: 8.4, genres: &["фантастика", "детектив"], runtime: 55,
        overview: "Исчезновение детей в немецком городке связывает четыре семьи через время.", seasons: &[10, 8, 8] },
    DemoTitle { kind: T, id: 66732, title: "Очень странные дела", original: "Stranger Things", year: 2016, rating: 8.6, genres: &["фантастика", "ужасы"], runtime: 51,
        overview: "Друзья ищут пропавшего мальчика и сталкиваются с тайной правительственной лаборатории.", seasons: &[8, 9, 8, 9, 8] },
    DemoTitle { kind: T, id: 1399, title: "Игра престолов", original: "Game of Thrones", year: 2011, rating: 8.5, genres: &["фэнтези", "драма"], runtime: 57,
        overview: "Знатные семьи Вестероса борются за Железный трон, пока с севера надвигается зима.", seasons: &[10, 10, 10, 10, 10, 10, 7, 6] },
    DemoTitle { kind: T, id: 100088, title: "Одни из нас", original: "The Last of Us", year: 2023, rating: 8.6, genres: &["драма", "фантастика"], runtime: 55,
        overview: "Контрабандист сопровождает девочку через разрушенную пандемией Америку.", seasons: &[9, 7] },
    DemoTitle { kind: T, id: 76479, title: "Пацаны", original: "The Boys", year: 2019, rating: 8.5, genres: &["боевик", "комедия"], runtime: 60,
        overview: "Команда линчевателей выступает против коррумпированных супергероев.", seasons: &[8, 8, 8, 8] },
];

fn summary(t: &DemoTitle) -> TitleSummary {
    TitleSummary {
        kind: t.kind,
        tmdb_id: t.id,
        title: t.title.to_owned(),
        original_title: Some(t.original.to_owned()),
        year: Some(t.year),
        poster_path: None,
        backdrop_path: None,
        rating: Some(t.rating),
        overview: Some(t.overview.to_owned()),
        user_rating: None,
        user_status: None,
        peers: Vec::new(),
        preloaded: false,
    }
}

/// The made-up cast every demo title has.
const DEMO_PEOPLE: [(&str, &str); 6] = [
    ("Анна Лебедева", "Главная роль"),
    ("Игорь Соколов", "Партнёр"),
    ("Мария Верховская", "Антагонист"),
    ("Пётр Громов", "Наставник"),
    ("Елена Мартынова", "Свидетель"),
    ("Дмитрий Орлов", "Друг семьи"),
];

fn find_any(id: i64) -> Result<&'static DemoTitle, CatalogError> {
    TITLES.iter().find(|t| t.id == id).ok_or(CatalogError::NotFound)
}

fn find(kind: MediaKind, id: i64) -> Result<&'static DemoTitle, CatalogError> {
    TITLES
        .iter()
        .find(|t| t.kind == kind && t.id == id)
        .ok_or(CatalogError::NotFound)
}

const DEMO_PAGE_SIZE: usize = 6;

const DEMO_SHELVES: &[ShelfMeta] = &[
    ShelfMeta {
        id: "trending_tv",
        title: "Популярные сериалы",
        subtitle: None,
        categories: &["for_you", "tv"],
        kind: Some(MediaKind::Tv),
        genre_tags: &[],
        base_priority: 95,
    },
    ShelfMeta {
        id: "trending_movie",
        title: "Популярные фильмы",
        subtitle: None,
        categories: &["for_you", "movies"],
        kind: Some(MediaKind::Movie),
        genre_tags: &[],
        base_priority: 95,
    },
    ShelfMeta {
        id: "top_rated",
        title: "С высоким рейтингом",
        subtitle: None,
        categories: &["for_you", "movies", "tv"],
        kind: None,
        genre_tags: &[],
        base_priority: 85,
    },
    ShelfMeta {
        id: "scifi",
        title: "Фантастика и фэнтези",
        subtitle: None,
        categories: &["for_you", "movies", "tv", "scifi"],
        kind: None,
        genre_tags: &["фантастика", "фэнтези"],
        base_priority: 75,
    },
    ShelfMeta {
        id: "thriller",
        title: "Триллеры и криминал",
        subtitle: None,
        categories: &["for_you", "movies", "tv", "thriller"],
        kind: None,
        genre_tags: &["триллер", "криминал", "детектив"],
        base_priority: 72,
    },
    ShelfMeta {
        id: "drama",
        title: "Сильные драмы",
        subtitle: None,
        categories: &["for_you", "movies", "tv", "drama"],
        kind: None,
        genre_tags: &["драма"],
        base_priority: 68,
    },
    ShelfMeta {
        id: "action",
        title: "Боевики и приключения",
        subtitle: None,
        categories: &["for_you", "movies", "tv", "action"],
        kind: None,
        genre_tags: &["боевик", "приключения"],
        base_priority: 64,
    },
    ShelfMeta {
        id: "fresh",
        title: "Свежие новинки",
        subtitle: None,
        categories: &["for_you", "movies", "tv", "fresh"],
        kind: None,
        genre_tags: &[],
        base_priority: 60,
    },
];

fn demo_shelf_all(id: &str) -> Option<Vec<TitleSummary>> {
    let items: Vec<TitleSummary> = match id {
        "trending_tv" => TITLES.iter().filter(|t| t.kind == MediaKind::Tv).map(summary).collect(),
        "trending_movie" => TITLES
            .iter()
            .filter(|t| t.kind == MediaKind::Movie)
            .map(summary)
            .collect(),
        "top_rated" => {
            let mut list: Vec<_> = TITLES.iter().map(summary).collect();
            list.sort_by(|a, b| b.rating.partial_cmp(&a.rating).unwrap_or(std::cmp::Ordering::Equal));
            list
        }
        "scifi" => TITLES
            .iter()
            .filter(|t| t.genres.iter().any(|g| matches!(*g, "фантастика" | "фэнтези")))
            .map(summary)
            .collect(),
        "thriller" => TITLES
            .iter()
            .filter(|t| {
                t.genres
                    .iter()
                    .any(|g| matches!(*g, "триллер" | "криминал" | "детектив"))
            })
            .map(summary)
            .collect(),
        "drama" => TITLES
            .iter()
            .filter(|t| t.genres.contains(&"драма"))
            .map(summary)
            .collect(),
        "action" => TITLES
            .iter()
            .filter(|t| t.genres.iter().any(|g| matches!(*g, "боевик" | "приключения")))
            .map(summary)
            .collect(),
        "fresh" => {
            let mut list: Vec<_> = TITLES.iter().filter(|t| t.year >= 2016).map(summary).collect();
            list.sort_by_key(|a| std::cmp::Reverse(a.year));
            list
        }
        _ => return None,
    };
    Some(items)
}

fn demo_shelf_page(meta: &ShelfMeta, page: u32, taste: &UserTaste) -> Result<(Vec<TitleSummary>, bool), CatalogError> {
    let all = demo_shelf_all(meta.id).ok_or(CatalogError::NotFound)?;
    let filtered = filter_excluded(all, &taste.excluded);
    let page = page.max(1) as usize;
    let start = (page - 1) * DEMO_PAGE_SIZE;
    if start >= filtered.len() {
        return Ok((Vec::new(), false));
    }
    let end = (start + DEMO_PAGE_SIZE).min(filtered.len());
    let has_more = end < filtered.len();
    Ok((filtered[start..end].to_vec(), has_more))
}

#[derive(Default)]
pub struct DemoCatalog;

#[async_trait]
impl Catalog for DemoCatalog {
    async fn home_for(&self, taste: &UserTaste) -> Result<HomeFeed, CatalogError> {
        let mut rows: Vec<CatalogRow> = taste.personal_rows.iter().take(INITIAL_HOME_ROWS).cloned().collect();
        let ranked = rank_shelves(DEMO_SHELVES, taste);

        for meta in ranked {
            if rows.len() < INITIAL_HOME_ROWS {
                let (items, has_more) = demo_shelf_page(meta, 1, taste)?;
                if !items.is_empty() {
                    rows.push(CatalogRow {
                        id: meta.id.to_owned(),
                        title: meta.title.to_owned(),
                        subtitle: meta.subtitle.map(str::to_owned),
                        categories: meta.categories.iter().map(|c| (*c).to_owned()).collect(),
                        loaded: true,
                        has_more,
                        page: 1,
                        items,
                    });
                }
            } else {
                rows.push(deferred_stub(meta));
            }
        }

        let categories = categories_for(&rows);
        Ok(HomeFeed {
            rows,
            categories,
            demo: true,
        })
    }

    async fn home_shelves(&self, ids: &[String], taste: &UserTaste) -> Result<Vec<CatalogRow>, CatalogError> {
        let mut out = Vec::new();
        for id in ids {
            if let Some((kind, tmdb_id)) = parse_because_id(id) {
                if let Ok(row) = because_shelf_row(self, kind, tmdb_id, taste).await {
                    out.push(row);
                }
                continue;
            }
            let Some(meta) = DEMO_SHELVES.iter().find(|s| s.id == id) else {
                continue;
            };
            let (items, has_more) = demo_shelf_page(meta, 1, taste)?;
            out.push(CatalogRow {
                id: meta.id.to_owned(),
                title: meta.title.to_owned(),
                subtitle: meta.subtitle.map(str::to_owned),
                categories: meta.categories.iter().map(|c| (*c).to_owned()).collect(),
                loaded: true,
                has_more,
                page: 1,
                items,
            });
        }
        Ok(out)
    }

    async fn shelf_page(&self, id: &str, page: u32, taste: &UserTaste) -> Result<ShelfPage, CatalogError> {
        if let Some((kind, tmdb_id)) = parse_because_id(id) {
            return because_shelf_page(self, kind, tmdb_id, taste).await;
        }
        let meta = DEMO_SHELVES.iter().find(|s| s.id == id).ok_or(CatalogError::NotFound)?;
        let (items, has_more) = demo_shelf_page(meta, page, taste)?;
        Ok(ShelfPage {
            id: meta.id.to_owned(),
            title: meta.title.to_owned(),
            subtitle: meta.subtitle.map(str::to_owned),
            page: page.max(1),
            has_more,
            items,
        })
    }

    async fn search(&self, query: &str) -> Result<SearchResults, CatalogError> {
        let norm_q = normalize_search_text(query);
        let mut items: Vec<_> = TITLES
            .iter()
            .filter(|t| matches_normalized_query(t.title, Some(t.original), &norm_q))
            .map(summary)
            .collect();
        if items.is_empty() {
            if let Some((title_q, year)) = split_query_year(query) {
                let title_norm = normalize_search_text(title_q);
                let matched: Vec<_> = TITLES
                    .iter()
                    .filter(|t| matches_normalized_query(t.title, Some(t.original), &title_norm))
                    .collect();
                items = matched
                    .iter()
                    .copied()
                    .filter(|t| t.year == year)
                    .map(summary)
                    .collect();
                if items.is_empty() {
                    items = matched
                        .into_iter()
                        .filter(|t| (t.year - year).abs() <= 1)
                        .map(summary)
                        .collect();
                }
            }
        }
        Ok(SearchResults {
            query: query.to_owned(),
            items,
        })
    }

    async fn random_movie(&self, filter: &RandomMovieFilter) -> Result<RandomMoviePick, CatalogError> {
        let bounds = filter.effective_bounds();
        let genre_norm = filter
            .genre_name
            .as_deref()
            .map(normalize_search_text)
            .filter(|g| !g.is_empty());
        let has_genre = |t: &DemoTitle| {
            genre_norm
                .as_deref()
                .is_none_or(|g| t.genres.iter().any(|tg| normalize_search_text(tg) == g))
        };
        let movies: Vec<&DemoTitle> = TITLES.iter().filter(|t| t.kind == MediaKind::Movie).collect();
        let exact: Vec<&DemoTitle> = movies
            .iter()
            .copied()
            .filter(|t| {
                has_genre(t)
                    && bounds
                        .runtime_range
                        .is_none_or(|(lo, hi)| (lo..=hi).contains(&t.runtime))
                    && bounds.min_rating.is_none_or(|r| t.rating >= r)
            })
            .collect();

        let (pool, relaxed) = if !exact.is_empty() {
            (exact, false)
        } else {
            let by_genre: Vec<&DemoTitle> = movies.iter().copied().filter(|t| has_genre(t)).collect();
            if !by_genre.is_empty() {
                (by_genre, true)
            } else {
                (movies, true)
            }
        };
        if pool.is_empty() {
            return Err(CatalogError::NotFound);
        }
        let idx = rand::rng().random_range(0..pool.len());
        let details = self.details(MediaKind::Movie, pool[idx].id).await?;
        Ok(RandomMoviePick { details, relaxed })
    }

    async fn details(&self, kind: MediaKind, tmdb_id: i64) -> Result<TitleDetails, CatalogError> {
        let t = find(kind, tmdb_id)?;
        let seasons = t
            .seasons
            .iter()
            .enumerate()
            .map(|(i, &n)| SeasonSummary {
                number: i as i32 + 1,
                name: format!("Сезон {}", i + 1),
                episode_count: n,
                year: Some(t.year + i as i32),
            })
            .collect();
        Ok(TitleDetails {
            summary: summary(t),
            genres: t.genres.iter().map(|g| (*g).to_owned()).collect(),
            runtime_min: Some(t.runtime),
            tagline: None,
            seasons,
            imdb_id: None,
            last_episode: t.seasons.last().map(|&n| (t.seasons.len() as i32, n)),
            series_status: (t.kind == MediaKind::Tv).then(|| "Ended".to_owned()),
            release_date: Some(format!("{}-01-01", t.year)),
            next_episode: None,
        })
    }

    async fn extras(&self, kind: MediaKind, tmdb_id: i64) -> Result<TitleExtras, CatalogError> {
        let t = find(kind, tmdb_id)?;
        // Made-up cast without photos; the other titles of the demo catalog stand in
        // for recommendations and similar ones.
        let cast = DEMO_PEOPLE
            .iter()
            .enumerate()
            .map(|(i, (name, role))| CastMember {
                id: t.id * 10 + i as i64,
                name: (*name).to_owned(),
                character: Some((*role).to_owned()),
                profile_path: None,
            })
            .collect();
        let others = |same_genre: bool| -> Vec<TitleSummary> {
            TITLES
                .iter()
                .filter(|o| o.id != t.id && (o.genres.iter().any(|g| t.genres.contains(g)) == same_genre))
                .map(summary)
                .take(8)
                .collect()
        };
        Ok(TitleExtras {
            cast,
            recommendations: others(true),
            similar: others(false),
        })
    }

    async fn person(&self, id: i64) -> Result<PersonDetails, CatalogError> {
        // The demo cast of title `t` has ids `t * 10 + 0..5` (see `extras`).
        let title = find_any(id / 10)?;
        let (name, role) = DEMO_PEOPLE
            .get(usize::try_from(id % 10).map_err(|_| CatalogError::NotFound)?)
            .ok_or(CatalogError::NotFound)?;
        // Their own title first, then a few more of the catalog.
        let mut credits = vec![summary(title)];
        credits.extend(TITLES.iter().filter(|o| o.id != title.id).map(summary).take(7));
        Ok(PersonDetails {
            id,
            name: (*name).to_owned(),
            biography: Some(format!(
                "Вымышленный актёр демонстрационного каталога. Сыграл роль: {role}. Настоящая биография появится вместе с данными TMDB."
            )),
            birthday: Some("1985-04-12".into()),
            deathday: None,
            place_of_birth: Some("Орёл, Россия".into()),
            profile_path: None,
            credits,
        })
    }

    async fn season(&self, tmdb_id: i64, number: i32) -> Result<Season, CatalogError> {
        let t = find(MediaKind::Tv, tmdb_id)?;
        let count = *t
            .seasons
            .get(usize::try_from(number - 1).map_err(|_| CatalogError::NotFound)?)
            .ok_or(CatalogError::NotFound)?;
        Ok(Season {
            number,
            name: format!("Сезон {number}"),
            episodes: (1..=count)
                .map(|e| Episode {
                    number: e,
                    name: format!("Серия {e}"),
                    overview: None,
                    still_path: None,
                    runtime_min: Some(t.runtime),
                    air_date: None,
                })
                .collect(),
        })
    }

    async fn trending_today(&self, kind: MediaKind, limit: usize) -> Result<Vec<TitleSummary>, CatalogError> {
        Ok(TITLES
            .iter()
            .filter(|t| t.kind == kind)
            .map(summary)
            .take(limit)
            .collect())
    }

    async fn image(&self, _size: &str, _file: &str) -> Result<(Vec<u8>, String), CatalogError> {
        Err(CatalogError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_matches_e_and_yo_equivalently() {
        let res = DemoCatalog.search("темный рыцарь").await.unwrap();
        assert_eq!(res.items.len(), 1);
        assert_eq!(res.items[0].title, "Тёмный рыцарь");

        let res_yo = DemoCatalog.search("Тёмный").await.unwrap();
        assert_eq!(res_yo.items.len(), 1);
        assert_eq!(res_yo.items[0].tmdb_id, 155);

        let res_year = DemoCatalog.search("Матрица 1999").await.unwrap();
        assert_eq!(res_year.items.len(), 1);
        assert_eq!(res_year.items[0].tmdb_id, 603);
        assert!(DemoCatalog.search("Матрица 2020").await.unwrap().items.is_empty());
    }

    #[tokio::test]
    async fn random_movie_respects_filter_and_relaxes_when_empty() {
        let pick = DemoCatalog
            .random_movie(&RandomMovieFilter {
                genre_name: Some("фантастика".into()),
                target_runtime_min: Some(140),
                min_rating: Some(8.0),
                deviation_pct: 20,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!pick.relaxed);
        assert_eq!(pick.details.summary.kind, MediaKind::Movie);

        let relaxed_pick = DemoCatalog
            .random_movie(&RandomMovieFilter {
                genre_name: Some("фантастика".into()),
                target_runtime_min: Some(40),
                min_rating: Some(9.9),
                deviation_pct: 0,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(relaxed_pick.relaxed);
    }

    #[tokio::test]
    async fn home_personalizes_excludes_and_paginates() {
        use super::super::personalize::build_taste;
        use crate::userdata::UserTitleSignal;
        use kino_core::TitleStatus;

        let signals = vec![
            UserTitleSignal {
                kind: MediaKind::Movie,
                tmdb_id: 603, // Матрица (фантастика, боевик)
                status: Some(TitleStatus::Watched),
                rating: Some(5),
                finished: true,
            },
            UserTitleSignal {
                kind: MediaKind::Movie,
                tmdb_id: 155, // Тёмный рыцарь (dropped)
                status: Some(TitleStatus::Dropped),
                rating: Some(2),
                finished: false,
            },
        ];

        let taste = build_taste(&DemoCatalog, &signals).await;
        assert!(taste.excluded.contains(&(MediaKind::Movie, 603)));
        assert!(taste.excluded.contains(&(MediaKind::Movie, 155)));
        assert!(!taste.personal_rows.is_empty(), "should build a 'because' shelf");
        assert_eq!(taste.personal_rows[0].id, "because:movie:603");

        let feed = DemoCatalog.home_for(&taste).await.unwrap();
        assert!(!feed.categories.is_empty());
        assert_eq!(
            feed.rows.iter().filter(|r| r.loaded).count(),
            INITIAL_HOME_ROWS,
            "initial response should cap loaded rows"
        );
        assert!(feed.rows.iter().any(|r| !r.loaded), "trailing rows must be deferred");
        for row in feed.rows.iter().filter(|r| r.loaded) {
            assert!(row
                .items
                .iter()
                .all(|i| !(i.kind == MediaKind::Movie && (i.tmdb_id == 603 || i.tmdb_id == 155))));
        }

        let deferred_ids: Vec<String> = feed.rows.iter().filter(|r| !r.loaded).map(|r| r.id.clone()).collect();
        let batch = DemoCatalog.home_shelves(&deferred_ids, &taste).await.unwrap();
        assert_eq!(batch.len(), deferred_ids.len());
        assert!(batch.iter().all(|r| r.loaded));

        let p1 = DemoCatalog.shelf_page("top_rated", 1, &taste).await.unwrap();
        assert!(p1.has_more);
        let p2 = DemoCatalog.shelf_page("top_rated", 2, &taste).await.unwrap();
        assert!(!p2.items.is_empty());
        assert!(p2
            .items
            .iter()
            .all(|i| !p1.items.iter().any(|x| x.kind == i.kind && x.tmdb_id == i.tmdb_id)));
    }
}
