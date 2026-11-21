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
    ContinueItem, HistoryEntry, HomeFeed, MediaKind, MediaTrack, PersonDetails, PreloadItem, PreloadState, Progress,
    ReleaseRef, SearchResults, Season, StreamPhase, StreamProgress, TitleDetails, TitleExtras, TitleState, TitleStatus,
    UserPrefs,
};
use kino_server::catalog::{Catalog, DemoCatalog};
use kino_ui::{
    icons, mock_releases, HomeView, Icon, LoginView, MyFilters, MyView, PersonView, PickerPanel, PlayerEpisode,
    PlayerMenu, PlayerState, PlayerView, ProfileView, ReleaseFilter, ReleaseOption, ReleasePicker, ReleaseSort,
    SearchView, Shell, Tab, Theme, TitleExtrasView, TitleView, FAVICON_URI, STYLE,
};

const FONTS_URL: &str = "https://fonts.googleapis.com/css2?family=Golos+Text:wght@400;500;600;700&family=Playfair+Display:ital,wght@0,500;0,600;0,700;1,500&display=swap";

#[derive(Clone, PartialEq)]
struct Picker {
    heading: String,
    releases: Vec<ReleaseOption>,
    chosen: Option<String>,
    watched: Vec<String>,
    filter: ReleaseFilter,
    sort: ReleaseSort,
    panel: Option<PickerPanel>,
    /// Demo data (shows the "примеры" badge) or parsed from a tracker feed.
    mock: bool,
}

#[derive(Clone, PartialEq)]
enum Screen {
    Home(HomeFeed, Vec<ContinueItem>),
    Search(String, SearchResults),
    Title {
        details: TitleDetails,
        season: Option<Season>,
        progress: Vec<Progress>,
        status: Option<TitleStatus>,
        my_rating: Option<i16>,
        release_label: Option<String>,
        picker: Option<Picker>,
        extras: Option<TitleExtras>,
    },
    Mine(Vec<TitleState>, Option<TitleStatus>),
    Person(PersonDetails),
    Profile(Vec<HistoryEntry>, Vec<PreloadItem>, UserPrefs),
    Player(PlayerState),
    Login,
}

#[derive(Props, Clone, PartialEq)]
struct PageProps {
    screen: Screen,
    light: bool,
}

fn Page(props: PageProps) -> Element {
    let tab = match &props.screen {
        Screen::Home(..) => Some(Tab::Home),
        Screen::Search(..) => Some(Tab::Search),
        Screen::Mine(..) => Some(Tab::Mine),
        Screen::Profile(..) => Some(Tab::Profile),
        _ => None,
    };
    let query = match &props.screen {
        Screen::Search(q, _) => q.clone(),
        _ => String::new(),
    };
    let body = match props.screen {
        Screen::Player(state) => {
            return rsx! {
                PlayerView {
                    state,
                    on_toggle: |_| {},
                    on_seek: |_| {},
                    on_menu: |_| {},
                    on_audio: |_| {},
                    on_subtitle: |_| {},
                    on_next: |_| {},
                    on_scale: |_| {},
                    on_fullscreen: |_| {},
                    on_close: |_| {},
                    on_tap: |_| {},
                    on_double_tap: |_| {},
                    on_speed: |_| {},
                    on_episode: |_| {},
                    on_pick_release: |_| {},
                    on_video: |_| {},
                }
            }
        }
        Screen::Login => {
            return rsx! {
                LoginView { invite: Some("demo".to_string()),
                    input { class: "input-field", placeholder: "Код приглашения (если есть)" }
                    button { class: "btn primary",
                        Icon { svg: icons::TG, class: "ic sm" }
                        "Войти через Telegram"
                    }
                }
            }
        }
        Screen::Home(feed, cont) => {
            rsx! {
                HomeView {
                    feed: Some(feed),
                    continue_items: cont,
                    on_open: |_| {},
                    on_open_title: |_| {},
                    on_play: |_| {},
                    on_drop: |_| {},
                    on_category: |_| {},
                    on_open_shelf: |_| {},
                    on_load_more_rows: |_| {},
                }
            }
        }
        Screen::Search(_, r) => rsx! { SearchView { results: Some(r), on_open: |_| {} } },
        Screen::Person(person) => rsx! { PersonView { person, on_open: |_| {} } },
        Screen::Mine(items, filter) => rsx! {
            MyView {
                items,
                filters: MyFilters { status: filter, ..MyFilters::default() },
                on_filters: |_| {},
                on_open: |_| {},
            }
        },
        Screen::Profile(history, preloads, prefs) => rsx! {
            ProfileView {
                name: "Даниил",
                history,
                preloads,
                prefs,
                theme: if props.light { Theme::Light } else { Theme::Dark },
                in_telegram: false,
                now: Utc::now(),
                utc_offset_min: 180,
                on_theme: |_| {},
                on_prefs: |_| {},
                on_delete_preload: |_| {},
                on_refresh: |_| {},
                on_open: |_| {},
                on_logout: |_| {},
            }
        },
        Screen::Title {
            details,
            season,
            progress,
            status,
            my_rating,
            release_label,
            picker,
            extras,
        } => rsx! {
            TitleView {
                details,
                season,
                progress,
                status,
                my_rating,
                release_label,
                on_play: |_| {},
                on_change_release: |_| {},
                on_status: |_| {},
                on_rate: |_| {},
                on_mark: |_| {},
                on_season: |_| {},
            }
            if let Some(extras) = extras {
                TitleExtrasView { extras, on_open: |_| {}, on_person: |_| {} }
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
                    mock: p.mock,
                    note: None,
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
        Shell { tab, query, user_initial: "Д", dl_percent: Some(42), on_tab: |_| {}, on_query: |_| {}, {body} }
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

fn page(screen: Screen, title: &str, light: bool) -> String {
    let mut dom = VirtualDom::new_with_props(Page, PageProps { screen, light });
    dom.rebuild_in_place();
    let body = dioxus_ssr::render(&dom);
    let theme_attr = if light { " data-theme=\"light\"" } else { "" };
    format!(
        "<!doctype html><html lang=\"ru\"{theme_attr}><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <link rel=\"icon\" type=\"image/svg+xml\" href=\"{FAVICON_URI}\">\
         <link rel=\"preconnect\" href=\"https://fonts.googleapis.com\">\
         <link rel=\"preconnect\" href=\"https://fonts.gstatic.com\" crossorigin>\
         <link rel=\"stylesheet\" href=\"{FONTS_URL}\" media=\"print\" onload=\"this.media='all'\">\
         <noscript><link rel=\"stylesheet\" href=\"{FONTS_URL}\"></noscript>\
         <title>{title} — Орловский кинопоказ</title><style>{STYLE}</style></head><body>{body}</body></html>"
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
            stale: false,
        },
        ContinueItem {
            last: inc_progress[0].clone(),
            next_episode: None,
            title: title_of(MediaKind::Movie, 27205).await,
            stale: false,
        },
        ContinueItem {
            last: dark_last,
            next_episode: Some((1, 5)),
            title: title_of(MediaKind::Tv, 70523).await,
            stale: true,
        },
    ];

    let bb = cat.details(MediaKind::Tv, 1396).await?;
    let mut bb_s2 = cat.season(1396, 2).await?;
    for (ep, date) in bb_s2.episodes.iter_mut().zip(1..) {
        ep.air_date = Some(format!("2009-{:02}-{:02}", 3 + date / 4, 1 + (date * 7) % 28));
    }
    let bb_extras = cat.extras(MediaKind::Tv, 1396).await?;
    let inception = cat.details(MediaKind::Movie, 27205).await?;
    let inc_extras = cat.extras(MediaKind::Movie, 27205).await?;
    // "Мои": a mix of statuses and ratings.
    let mut mine = Vec::new();
    for (k, id, st, rating) in [
        (MediaKind::Tv, 1396, TitleStatus::Watching, None),
        (MediaKind::Movie, 27205, TitleStatus::Watching, None),
        (MediaKind::Tv, 70523, TitleStatus::Watching, None),
        (MediaKind::Movie, 438631, TitleStatus::Want, None),
        (MediaKind::Tv, 100088, TitleStatus::Want, None),
        (MediaKind::Movie, 157336, TitleStatus::Want, None),
        (MediaKind::Movie, 603, TitleStatus::Watched, Some(5)),
        (MediaKind::Movie, 155, TitleStatus::Watched, Some(5)),
        (MediaKind::Movie, 550, TitleStatus::Watched, Some(4)),
        (MediaKind::Tv, 1399, TitleStatus::Watched, Some(3)),
        (MediaKind::Tv, 76479, TitleStatus::Dropped, Some(2)),
    ] {
        mine.push(TitleState {
            kind: k,
            tmdb_id: id,
            status: Some(st),
            rating,
            audio_track: None,
            subtitle_track: None,
            video_scale: None,
            updated_at: Utc::now(),
            title: title_of(k, id).await,
        });
    }
    let hist = |k, id, s, e, from: f64, to: f64, ago_h: i64, dev: &str| HistoryEntry {
        kind: k,
        tmdb_id: id,
        season: s,
        episode: e,
        started_at: Utc::now() - Duration::hours(ago_h) - Duration::minutes(40),
        last_at: Utc::now() - Duration::hours(ago_h),
        position_from: from,
        position_to: to,
        device: Some(dev.into()),
        title: None,
    };
    let mut history = vec![
        hist(MediaKind::Tv, 1396, 2, 6, 0.0, 2472.0, 1, "Телефон"),
        hist(MediaKind::Tv, 1396, 2, 5, 0.0, 2810.0, 22, "Компьютер"),
        hist(MediaKind::Movie, 27205, 0, 0, 1820.0, 4360.0, 26, "Телевизор"),
        hist(MediaKind::Movie, 27205, 0, 0, 0.0, 1820.0, 50, "Телефон"),
        hist(MediaKind::Tv, 70523, 1, 4, 0.0, 3290.0, 74, "Компьютер"),
    ];
    for h in &mut history {
        h.title = title_of(h.kind, h.tmdb_id).await;
    }
    let preload = |id, k, tmdb, season, title: &str, state, progress, gb: f64, delete_days: Option<i64>| PreloadItem {
        id,
        kind: k,
        tmdb_id: tmdb,
        season,
        infohash: "e".repeat(40),
        title: Some(title.into()),
        state,
        progress,
        size_bytes: Some((gb * 1e9) as i64),
        requested_at: Utc::now() - Duration::hours(3),
        ready_at: None,
        delete_after: delete_days.map(|d| Utc::now() + Duration::days(d) + Duration::hours(1)),
    };
    let preloads = vec![
        preload(
            1,
            MediaKind::Tv,
            100088,
            2,
            "Одни из нас / The Last of Us (S02) 2160p BDRemux | Оригинал",
            PreloadState::Downloading,
            0.42,
            61.0,
            None,
        ),
        preload(
            2,
            MediaKind::Movie,
            438631,
            0,
            "Дюна / Dune (2021) 2160p WEB-DL HDR | Дубляж",
            PreloadState::Ready,
            1.0,
            23.5,
            None,
        ),
        preload(
            3,
            MediaKind::Movie,
            157336,
            0,
            "Интерстеллар / Interstellar (2014) 1080p BDRip | Дубляж",
            PreloadState::Ready,
            1.0,
            12.1,
            Some(5),
        ),
    ];
    let prefs = UserPrefs {
        quality: Some("1080p".into()),
        voice: None,
        subtitle_lang: None,
        language: Some("Русский".into()),
        codec: Some("H.264".into()),
        min_seeders: Some(20),
        sort: Some("bitrate".into()),
        ..UserPrefs::default()
    };

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
        mock: true,
    };
    let bb_screen = |picker| Screen::Title {
        details: bb.clone(),
        season: Some(bb_s2.clone()),
        progress: bb_progress.clone(),
        status: Some(TitleStatus::Watching),
        my_rating: None,
        release_label: Some(chosen.label()),
        picker,
        extras: Some(bb_extras.clone()),
    };
    // Releases as the indexer delivers them: names from a tracker feed, parsed.
    let feed = kino_server::torrent::torznab::parse_feed(include_str!("feed.xml"))?;
    let mut live: Vec<ReleaseOption> = feed
        .iter()
        .map(|it| kino_server::torrent::to_release(it, &bb))
        .collect();
    live.sort_by_key(|r| std::cmp::Reverse(r.seeders));
    let live_picker = Picker {
        heading: "Во все тяжкие · S02E06 · продолжить с 41:12".to_string(),
        releases: live,
        chosen: None,
        watched: vec![],
        filter: ReleaseFilter::default(),
        sort: ReleaseSort::Seeders,
        panel: None,
        mock: false,
    };
    let track = |index, lang: &str, title: Option<&str>, codec: &str, channels, default| MediaTrack {
        index,
        language: Some(lang.into()),
        title: title.map(String::from),
        codec: codec.into(),
        channels: Some(channels),
        default,
    };
    let player = PlayerState {
        title: "Во все тяжкие".into(),
        subtitle: Some("S02E06 · Ку-ку".into()),
        release: "LostFilm · 1080p · RuTracker".into(),
        src: String::new(),
        subtitles_src: None,
        poster: Some("frame.jpg".into()),
        position: 2472.0,
        duration: Some(2820.0),
        paused: true,
        waiting: false,
        status: None,
        progress: None,
        error: None,
        audio: vec![
            track(0, "rus", Some("LostFilm"), "eac3", 6, true),
            track(1, "rus", Some("NewStudio"), "ac3", 2, false),
            track(2, "eng", Some("Original"), "dts", 6, false),
        ],
        subtitle_tracks: vec![
            track(0, "rus", Some("Полные"), "subrip", 0, false),
            track(1, "eng", None, "subrip", 0, false),
        ],
        audio_track: Some(0),
        subtitle_track: None,
        menu: None,
        next: Some("S02E07".into()),
        transcoded: true,
        native_video: false,
        preloaded: false,
        controls: true,
        rate: 1.0,
        scale: kino_ui::VideoScale::FitWidth,
        volume: 0.78,
        muted: false,
        show_volume: true,
        season: 2,
        episode: 6,
        episodes: [
            "Семь тридцать семь",
            "Копчёный лосось",
            "Бесконечное лето",
            "Хорошо в ванной",
            "Ку-ку",
            "Кошмар на улице Вязов",
            "Окружная свадьба",
        ]
        .iter()
        .enumerate()
        .map(|(i, t)| PlayerEpisode {
            number: i as i32 + 1,
            title: (*t).into(),
            watched: i < 5,
        })
        .collect(),
        flash: None,
        autoplay: true,
        auto_muted: false,
        sync_members: Vec::new(),
        sync_waiting_for: Vec::new(),
        sync_client_id: String::new(),
        sync_sound_client_id: None,
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
                status: Some(TitleStatus::Watching),
                my_rating: Some(4),
                release_label: Some("NewStudio · 1080p · RuTor".into()),
                picker: None,
                extras: Some(inc_extras),
            },
        ),
        ("releases-live", "Раздачи с трекеров", bb_screen(Some(live_picker))),
        ("player", "Плеер", Screen::Player(player.clone())),
        (
            "player-audio",
            "Плеер · звук",
            Screen::Player(PlayerState {
                menu: Some(PlayerMenu::Audio),
                ..player.clone()
            }),
        ),
        (
            "player-speed",
            "Плеер · скорость",
            Screen::Player(PlayerState {
                menu: Some(PlayerMenu::Speed),
                rate: 1.25,
                paused: false,
                ..player.clone()
            }),
        ),
        (
            "player-sync",
            "Плеер · синхронизация",
            Screen::Player(PlayerState {
                menu: Some(PlayerMenu::Sync),
                paused: false,
                sync_client_id: "pc-1".into(),
                sync_sound_client_id: Some("pc-1".into()),
                sync_members: vec![
                    kino_core::SyncMemberInfo {
                        client_id: "pc-1".into(),
                        device: "ПК · Chrome".into(),
                        ready: true,
                        buffering: false,
                    },
                    kino_core::SyncMemberInfo {
                        client_id: "phone-2".into(),
                        device: "Телефон · Android".into(),
                        ready: true,
                        buffering: false,
                    },
                ],
                ..player.clone()
            }),
        ),
        (
            "player-episodes",
            "Плеер · серии",
            Screen::Player(PlayerState {
                menu: Some(PlayerMenu::Episodes),
                paused: false,
                ..player.clone()
            }),
        ),
        (
            "player-one-track",
            "Плеер · одна дорожка",
            Screen::Player(PlayerState {
                menu: Some(PlayerMenu::Audio),
                audio: vec![track(0, "rus", Some("LostFilm"), "eac3", 6, true)],
                ..player.clone()
            }),
        ),
        (
            "player-doubletap",
            "Плеер · двойной тап",
            Screen::Player(PlayerState {
                paused: false,
                controls: false,
                flash: Some((true, 20)),
                ..player.clone()
            }),
        ),
        (
            "player-next",
            "Плеер · следующая серия",
            Screen::Player(PlayerState {
                position: 2795.0,
                paused: false,
                subtitle_track: Some(0),
                ..player.clone()
            }),
        ),
        (
            "player-loading",
            "Плеер · запуск",
            Screen::Player(PlayerState {
                position: 0.0,
                duration: None,
                paused: false,
                status: Some("Подключаемся к раздаче…".into()),
                // 38% of the first part is loaded.
                progress: Some(StreamProgress {
                    phase: StreamPhase::Buffering,
                    loaded_bytes: 12_400_000,
                    needed_bytes: 33_554_432,
                    peers: 14,
                    seeders: 6,
                    speed_bps: 2_600_000,
                }),
                audio: vec![],
                subtitle_tracks: vec![],
                ..player.clone()
            }),
        ),
        (
            "player-connecting",
            "Плеер · поиск раздающих",
            Screen::Player(PlayerState {
                position: 0.0,
                duration: None,
                paused: false,
                status: Some("Подключаемся к раздаче…".into()),
                // Nothing to measure yet: peers are still being found.
                progress: Some(StreamProgress {
                    phase: StreamPhase::Connecting,
                    peers: 3,
                    ..StreamProgress::default()
                }),
                audio: vec![],
                subtitle_tracks: vec![],
                ..player.clone()
            }),
        ),
        (
            "person",
            "Актёр",
            Screen::Person(cat.person(bb_extras.cast[0].id).await?),
        ),
        ("mine", "Мои", Screen::Mine(mine, None)),
        ("profile", "Профиль", Screen::Profile(history, preloads, prefs)),
        ("login", "Вход", Screen::Login),
    ];
    for (file, title, screen) in pages {
        let light_pair = match file {
            "home" | "movie" | "person" | "profile" | "login" | "player-loading" => Some(screen.clone()),
            _ => None,
        };
        let path = out.join(format!("{file}.html"));
        std::fs::write(&path, page(screen, title, false))?;
        println!("{}", path.display());
        if let Some(s) = light_pair {
            let lpath = out.join(format!("{file}-light.html"));
            std::fs::write(&lpath, page(s, title, true))?;
            println!("{}", lpath.display());
        }
    }
    Ok(())
}
