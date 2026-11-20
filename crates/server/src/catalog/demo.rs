//! Built-in demo catalog: a handful of well-known titles, no posters.
//! Used when `TMDB_API_KEY` is not set, so the interface can be tried anywhere.

use async_trait::async_trait;
use kino_core::{
    CatalogRow, Episode, HomeFeed, MediaKind, SearchResults, Season, SeasonSummary, TitleDetails, TitleSummary,
};

use super::{Catalog, CatalogError};

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
    }
}

fn find(kind: MediaKind, id: i64) -> Result<&'static DemoTitle, CatalogError> {
    TITLES
        .iter()
        .find(|t| t.kind == kind && t.id == id)
        .ok_or(CatalogError::NotFound)
}

#[derive(Default)]
pub struct DemoCatalog;

#[async_trait]
impl Catalog for DemoCatalog {
    async fn home(&self) -> Result<HomeFeed, CatalogError> {
        let of = |kind| TITLES.iter().filter(|t| t.kind == kind).map(summary).collect();
        let mut best: Vec<_> = TITLES.iter().map(summary).collect();
        best.sort_by(|a, b| b.rating.partial_cmp(&a.rating).unwrap_or(std::cmp::Ordering::Equal));
        best.truncate(8);
        Ok(HomeFeed {
            rows: vec![
                CatalogRow {
                    title: "Популярные сериалы".into(),
                    items: of(MediaKind::Tv),
                },
                CatalogRow {
                    title: "Популярные фильмы".into(),
                    items: of(MediaKind::Movie),
                },
                CatalogRow {
                    title: "С высоким рейтингом".into(),
                    items: best,
                },
            ],
            demo: true,
        })
    }

    async fn search(&self, query: &str) -> Result<SearchResults, CatalogError> {
        let q = query.trim().to_lowercase();
        let items = TITLES
            .iter()
            .filter(|t| {
                !q.is_empty() && (t.title.to_lowercase().contains(&q) || t.original.to_lowercase().contains(&q))
            })
            .map(summary)
            .collect();
        Ok(SearchResults {
            query: query.to_owned(),
            items,
        })
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

    async fn image(&self, _size: &str, _file: &str) -> Result<(Vec<u8>, String), CatalogError> {
        Err(CatalogError::NotFound)
    }
}
