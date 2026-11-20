//! `kino-preview <out-dir>`: renders every screen on demo data to static HTML
//! files, so the interface can be looked at (and screenshotted) without a
//! database, Telegram or a WebAssembly build.

#![allow(non_snake_case)]
// A one-off dev tool: plain data over tidy types.
#![allow(clippy::large_enum_variant, clippy::too_many_arguments)]

use std::path::PathBuf;

use chrono::{Duration, Utc};
use dioxus::prelude::*;
use kino_core::{
    ContinueItem, HomeFeed, MediaKind, Progress, ReleaseRef, SearchResults, Season, TitleDetails, TitleSummary,
};
use kino_server::catalog::{Catalog, DemoCatalog};
use kino_ui::{
    mock_releases, HomeView, LoginView, PickerPanel, ReleaseFilter, ReleaseOption, ReleasePicker, ReleaseSort,
    SearchView, Shell, Tab, TitleView, WatchlistView, STYLE,
};

#[derive(Clone, PartialEq)]
struct Picker {
    heading: String,
    releases: Vec<ReleaseOption>,
    chosen: Option<String>,
    watched: Vec<String>,
    filter: ReleaseFilter,
    sort: ReleaseSort,
    panel: Option<PickerPanel>,
}

#[derive(Clone, PartialEq)]
enum Screen {
    Home(HomeFeed, Vec<ContinueItem>),
    Search(String, SearchResults),
    Title {
        details: TitleDetails,
        season: Option<Season>,
        progress: Vec<Progress>,
        in_watchlist: bool,
        release_label: Option<String>,
        picker: Option<Picker>,
    },
    Watchlist(Vec<TitleSummary>),
    Login,
}

#[derive(Props, Clone, PartialEq)]
struct PageProps {
    screen: Screen,
}

fn Page(props: PageProps) -> Element {
    let tab = match &props.screen {
        Screen::Home(..) => Some(Tab::Home),
        Screen::Search(..) => Some(Tab::Search),
        Screen::Watchlist(..) => Some(Tab::Watchlist),
        _ => None,
    };
    let query = match &props.screen {
        Screen::Search(q, _) => q.clone(),
        _ => String::new(),
    };
    let body = match props.screen {
        Screen::Login => {
            return rsx! {
                LoginView { invite: Some("demo".to_string()),
                    button { class: "tg-btn", "✈ Войти через Telegram" }
                }
            }
        }
        Screen::Home(feed, cont) => rsx! { HomeView { feed, continue_items: cont, on_open: |_| {}, on_play: |_| {} } },
        Screen::Search(q, r) => rsx! { SearchView { query: q, results: Some(r), on_open: |_| {}, on_query: |_| {} } },
        Screen::Watchlist(items) => rsx! { WatchlistView { items, on_open: |_| {} } },
        Screen::Title {
            details,
            season,
            progress,
            in_watchlist,
            release_label,
            picker,
        } => rsx! {
            TitleView {
                details,
                season,
                progress,
                in_watchlist,
                release_label,
                on_play: |_| {},
                on_change_release: |_| {},
                on_toggle_watchlist: |_| {},
                on_season: |_| {},
            }
            if let Some(p) = picker {
                ReleasePicker {
                    heading: p.heading,
                    releases: p.releases,
                    chosen: p.chosen,
                    watched: p.watched,
                    filter: p.filter,
                    sort: p.sort,
                    panel: p.panel,
                    mock: true,
                    on_filter: |_| {},
                    on_sort: |_| {},
                    on_panel: |_| {},
                    on_pick: |_| {},
                    on_preload: |_| {},
                    on_close: |_| {},
                }
            }
        },
    };
    rsx! {
        Shell { tab, query, user_initial: "Д", on_tab: |_| {}, on_query: |_| {}, {body} }
    }
}

fn progress(
    kind: MediaKind,
    id: i64,
    season: i32,
    episode: i32,
    pos: f64,
    dur: f64,
    ago_min: i64,
    rel: Option<&ReleaseRef>,
) -> Progress {
    Progress {
        kind,
        tmdb_id: id,
        season,
        episode,
        position_sec: pos,
        duration_sec: dur,
        finished: kino_core::is_finished(pos, dur),
        release: rel.cloned(),
        updated_at: Utc::now() - Duration::minutes(ago_min),
    }
}

fn release(hash: char, title: &str) -> ReleaseRef {
    ReleaseRef {
        infohash: hash.to_string().repeat(40),
        file_index: 0,
        title: Some(title.into()),
    }
}

fn page(screen: Screen, title: &str) -> String {
    let mut dom = VirtualDom::new_with_props(Page, PageProps { screen });
    dom.rebuild_in_place();
    let body = dioxus_ssr::render(&dom);
    format!(
        "<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title} — Кино</title><style>{STYLE}</style></head><body>{body}</body></html>"
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "preview".into()));
    std::fs::create_dir_all(&out)?;
    let cat = DemoCatalog;

    // Demo viewing history.
    let bb_rel = release(
        'a',
        "Во все тяжкие / Breaking Bad (S02 1-13 из 13) WEB-DL 1080p | LostFilm",
    );
    let inc_rel = release('c', "Начало / Inception (2010) BDRip 1080p | x264");
    let mut bb_progress = Vec::new();
    for e in 1..=7 {
        bb_progress.push(progress(
            MediaKind::Tv,
            1396,
            1,
            e,
            2800.0,
            2820.0,
            9000 - i64::from(e),
            Some(&bb_rel),
        ));
    }
    for e in 1..=5 {
        bb_progress.push(progress(
            MediaKind::Tv,
            1396,
            2,
            e,
            2810.0,
            2820.0,
            500 - i64::from(e) * 10,
            Some(&bb_rel),
        ));
    }
    bb_progress.push(progress(MediaKind::Tv, 1396, 2, 6, 2472.0, 2820.0, 30, Some(&bb_rel)));
    let inc_progress = vec![progress(
        MediaKind::Movie,
        27205,
        0,
        0,
        4360.0,
        8880.0,
        60 * 26,
        Some(&inc_rel),
    )];
    let dark_last = progress(MediaKind::Tv, 70523, 1, 4, 3290.0, 3300.0, 60 * 50, None);

    let title_of = |kind, id| {
        let cat = &cat;
        async move { cat.details(kind, id).await.ok().map(|d| d.summary) }
    };
    let cont = vec![
        ContinueItem {
            last: bb_progress.last().unwrap().clone(),
            next_episode: None,
            title: title_of(MediaKind::Tv, 1396).await,
        },
        ContinueItem {
            last: inc_progress[0].clone(),
            next_episode: None,
            title: title_of(MediaKind::Movie, 27205).await,
        },
        ContinueItem {
            last: dark_last,
            next_episode: Some((1, 5)),
            title: title_of(MediaKind::Tv, 70523).await,
        },
    ];

    let bb = cat.details(MediaKind::Tv, 1396).await?;
    let mut bb_s2 = cat.season(1396, 2).await?;
    for (ep, date) in bb_s2.episodes.iter_mut().zip(1..) {
        ep.air_date = Some(format!("2009-{:02}-{:02}", 3 + date / 4, 1 + (date * 7) % 28));
    }
    let inception = cat.details(MediaKind::Movie, 27205).await?;
    let mut watchlist = Vec::new();
    for (k, id) in [
        (MediaKind::Movie, 438631),
        (MediaKind::Tv, 100088),
        (MediaKind::Movie, 157336),
        (MediaKind::Tv, 76479),
    ] {
        watchlist.extend(title_of(k, id).await);
    }

    let bb_releases = mock_releases(&bb.summary, Some(2));
    let chosen = bb_releases[0].clone();
    let bb_picker = |filter, sort, panel| Picker {
        heading: "Во все тяжкие · S02E06 · продолжить с 41:12".to_string(),
        releases: bb_releases.clone(),
        chosen: Some(chosen.infohash.clone()),
        watched: vec![chosen.infohash.clone()],
        filter,
        sort,
        panel,
    };
    let bb_screen = |picker| Screen::Title {
        details: bb.clone(),
        season: Some(bb_s2.clone()),
        progress: bb_progress.clone(),
        in_watchlist: false,
        release_label: Some(chosen.label()),
        picker,
    };
    let filtered = ReleaseFilter {
        quality: Some("2160p".into()),
        language: Some("Русский".into()),
        ..Default::default()
    };
    let pages = [
        ("home", "Главная", Screen::Home(cat.home().await?, cont)),
        (
            "search",
            "Поиск",
            Screen::Search("the".into(), cat.search("the").await?),
        ),
        ("series", "Во все тяжкие", bb_screen(None)),
        (
            "releases",
            "Выбор раздачи",
            bb_screen(Some(bb_picker(ReleaseFilter::default(), ReleaseSort::Seeders, None))),
        ),
        (
            "releases-filter",
            "Фильтр раздач",
            bb_screen(Some(bb_picker(
                filtered,
                ReleaseSort::Seeders,
                Some(PickerPanel::Filter),
            ))),
        ),
        (
            "releases-sort",
            "Сортировка раздач",
            bb_screen(Some(bb_picker(
                ReleaseFilter::default(),
                ReleaseSort::Size,
                Some(PickerPanel::Sort),
            ))),
        ),
        (
            "movie",
            "Начало",
            Screen::Title {
                details: inception.clone(),
                season: None,
                progress: inc_progress,
                in_watchlist: true,
                release_label: Some("NewStudio · 1080p · RuTor".into()),
                picker: None,
            },
        ),
        ("watchlist", "Хочу посмотреть", Screen::Watchlist(watchlist)),
        ("login", "Вход", Screen::Login),
    ];
    for (file, title, screen) in pages {
        let path = out.join(format!("{file}.html"));
        std::fs::write(&path, page(screen, title))?;
        println!("{}", path.display());
    }
    Ok(())
}
