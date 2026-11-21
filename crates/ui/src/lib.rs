//! Screens and widgets of the Kino interface, as plain Dioxus components.
//!
//! Components take data and report user actions through `EventHandler`s; they
//! do no networking and no routing. The web app wires them to the API, and the
//! `kino-preview` tool renders them to static HTML for screenshots.

#![allow(non_snake_case)]

use std::rc::Rc;

use dioxus::html::{geometry::PixelsVector2D, MountedData, ScrollBehavior};
use dioxus::prelude::*;
use kino_core::{
    clock, episode_code, image_url, CastMember, ContinueItem, HomeFeed, ImageSize, MediaKind, PersonDetails, Progress,
    SearchResults, Season, TitleDetails, TitleExtras, TitleStatus, TitleSummary,
};

/// The stylesheet all components rely on.
pub const STYLE: &str = include_str!("style.css");

/// Browser tab favicon: pure turquoise ring adapting to light/dark chrome.
pub const FAVICON_URI: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E%3Cstyle%3Ecircle%7Bstroke:%230B6B65%7D@media(prefers-color-scheme:dark)%7Bcircle%7Bstroke:%2338C6BD%7D%7D%3C/style%3E%3Ccircle cx='16' cy='16' r='10.5' fill='none' stroke-width='5'/%3E%3C/svg%3E";

mod account;
pub mod icons;
mod player;
mod releases;
pub use account::*;
pub use icons::{seeder_signal, Icon};
pub use player::*;
pub use releases::*;

/// Large brand mark (smooth eagle profile inside a turquoise film-reel ring).
#[component]
pub fn BrandMark() -> Element {
    rsx! { div { class: "brand-mark", dangerous_inner_html: include_str!("brand.svg") } }
}

/// Compact turquoise ring mark without the eagle.
#[component]
pub fn RingMark() -> Element {
    rsx! { span { class: "ring-mark", dangerous_inner_html: include_str!("ring.svg") } }
}

/// Editorial brand plaque for the top bar: full eagle-in-ring emblem (44px desktop / 38px mobile)
/// paired with a balanced 2-line stacked title.
#[component]
pub fn BrandPlaque(on_click: EventHandler<()>) -> Element {
    rsx! {
        button { class: "brand-plaque", onclick: move |_| on_click.call(()),
            span { class: "brand-svg", "aria-hidden": "true", dangerous_inner_html: include_str!("brand.svg") }
            span { class: "brand-stack",
                span { class: "brand-top", "Орловский" }
                span { class: "brand-sub", "кинопоказ" }
            }
        }
    }
}

/// Top-level sections of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Search,
    Mine,
    Profile,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Home => "Главная",
            Tab::Search => "Поиск",
            Tab::Mine => "Мои",
            Tab::Profile => "Профиль",
        }
    }

    fn icon_svg(self) -> &'static str {
        match self {
            Tab::Home => icons::HOME,
            Tab::Search => icons::SEARCH,
            Tab::Mine => icons::BOOKMARK,
            Tab::Profile => icons::HOME,
        }
    }
}

/// Desktop topbar tabs (Search lives in the search box, Profile behind the user avatar).
const DESKTOP_TABS: [Tab; 2] = [Tab::Home, Tab::Mine];
/// First three slots of the 4-tab mobile bottom bar; the 4th slot is the user's Telegram avatar.
const MOBILE_TABS: [Tab; 3] = [Tab::Home, Tab::Search, Tab::Mine];

fn hue(id: i64) -> i64 {
    (id.wrapping_mul(47)).rem_euclid(360)
}

/// Gradient used when a title has no image (demo data, or TMDB has none).
pub(crate) fn fallback_bg(id: i64) -> String {
    let h = hue(id);
    let h2 = (h + 40) % 360;
    format!("linear-gradient(160deg, hsl({h} 55% 38%), hsl({h2} 60% 16%))")
}

fn backdrop_style(t: &TitleSummary) -> String {
    match &t.backdrop_path {
        Some(p) => format!("background-image: url('{}')", image_url(p, ImageSize::Backdrop)),
        None => format!("background-image: {}", fallback_bg(t.tmdb_id)),
    }
}

fn rating(t: &TitleSummary) -> Option<String> {
    t.rating.filter(|r| *r > 0.0).map(|r| format!("{r:.1}"))
}

fn kind_label(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "Фильм",
        MediaKind::Tv => "Сериал",
    }
}

fn minutes_left(p: &Progress) -> i64 {
    ((p.duration_sec - p.position_sec).max(0.0) / 60.0).round() as i64
}

fn format_left_short(mins: i64) -> String {
    if mins >= 60 {
        let h = mins / 60;
        let m = mins % 60;
        if m == 0 {
            format!("ост. {h} ч")
        } else {
            format!("ост. {h} ч {m} мин")
        }
    } else {
        format!("ост. {mins} мин")
    }
}

fn percent(p: &Progress) -> f64 {
    if p.duration_sec > 0.0 {
        (p.position_sec / p.duration_sec * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Shows the user's Telegram photo when available and falls back to their initial on missing or failed loads.
#[component]
pub(crate) fn AvatarContent(initial: String, url: Option<String>, alt: String) -> Element {
    let mut failed_for = use_signal(|| None::<String>);
    let active_url = url.filter(|u| !u.is_empty() && failed_for().as_deref() != Some(u.as_str()));
    match active_url {
        Some(raw) => {
            let src = kino_core::asset_url(&raw);
            rsx! {
                img {
                    src: "{src}",
                    alt: "{alt}",
                    onerror: move |_| failed_for.set(Some(raw.clone())),
                }
            }
        }
        None => rsx! { "{initial}" },
    }
}

/// Top bar on wide screens, bottom 4-tab bar (with circular Telegram avatar) on phones.
/// No custom UI back button: browser, Android swipe, and Telegram's native BackButton handle back navigation.
#[component]
pub fn Shell(
    tab: Option<Tab>,
    query: String,
    user_initial: String,
    #[props(default)] avatar_url: Option<String>,
    #[props(default)] dl_percent: Option<u8>,
    on_tab: EventHandler<Tab>,
    on_query: EventHandler<String>,
    children: Element,
) -> Element {
    let is_search_tab = tab == Some(Tab::Search);
    let has_query = !query.is_empty();
    let header_cls = if is_search_tab { "topbar is-search" } else { "topbar" };
    rsx! {
        header { class: "{header_cls}",
            div { class: "top-brand-slot",
                BrandPlaque { on_click: move |_| on_tab.call(Tab::Home) }
            }
            nav { class: "nav-pills",
                for t in DESKTOP_TABS {
                    button {
                        class: if tab == Some(t) { "active" } else { "" },
                        onclick: move |_| on_tab.call(t),
                        Icon { svg: t.icon_svg(), class: "ic sm" }
                        "{t.label()}"
                    }
                }
            }
            div { class: "top-right",
                label { class: "search-box",
                    span { class: "muted", Icon { svg: icons::SEARCH, class: "ic sm" } }
                    input {
                        id: "kino-search-input",
                        r#type: "search",
                        placeholder: "Поиск фильмов и сериалов…",
                        value: "{query}",
                        oninput: move |e| on_query.call(e.value()),
                    }
                    if has_query {
                        button {
                            class: "clear-x",
                            r#type: "button",
                            title: "Очистить поиск",
                            "aria-label": "Очистить поиск",
                            onclick: move |e| {
                                e.prevent_default();
                                e.stop_propagation();
                                on_query.call(String::new());
                            },
                            Icon { svg: icons::CLOSE, class: "ic xs" }
                        }
                    } else {
                        span { class: "kbd", "Ctrl K" }
                    }
                }
                if let Some(pct) = dl_percent {
                    button {
                        class: "dl-pill",
                        title: "Активная загрузка на сервер",
                        onclick: move |_| on_tab.call(Tab::Profile),
                        Icon { svg: icons::DOWNLOAD, class: "ic sm" }
                        "{pct}%"
                    }
                }
                button {
                    class: if tab == Some(Tab::Profile) { "tg-avatar desktop-only active" } else { "tg-avatar desktop-only" },
                    title: "Профиль",
                    "aria-label": "Профиль",
                    onclick: move |_| on_tab.call(Tab::Profile),
                    AvatarContent { initial: user_initial.clone(), url: avatar_url.clone(), alt: "Профиль".to_owned() }
                }
            }
        }
        main { class: if tab.is_none() { "content no-tabbar" } else { "content" }, {children} }
        if tab.is_some() {
            nav { class: "tabbar",
                for t in MOBILE_TABS {
                    button {
                        class: if tab == Some(t) { "active" } else { "" },
                        onclick: move |_| on_tab.call(t),
                        Icon { svg: t.icon_svg(), class: "ic lg" }
                        span { "{t.label()}" }
                    }
                }
                button {
                    class: if tab == Some(Tab::Profile) { "tab-profile active" } else { "tab-profile" },
                    title: "Профиль",
                    "aria-label": "Профиль",
                    onclick: move |_| on_tab.call(Tab::Profile),
                    if dl_percent.is_some() {
                        span { class: "tab-dot" }
                    }
                    span { class: "tab-avatar",
                        AvatarContent { initial: user_initial, url: avatar_url, alt: "Профиль".to_owned() }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cards
// ---------------------------------------------------------------------------

#[component]
pub fn PosterCard(
    item: TitleSummary,
    #[props(default)] status_badge: Option<TitleStatus>,
    on_open: EventHandler<TitleSummary>,
) -> Element {
    let sub = match item.year {
        Some(y) => format!("{} · {y}", kind_label(item.kind)),
        None => kind_label(item.kind).to_owned(),
    };
    let rate = rating(&item);
    let it = item.clone();
    rsx! {
        button { class: "poster", onclick: move |_| on_open.call(it.clone()),
            div { class: "img",
                if let Some(p) = &item.poster_path {
                    img { src: image_url(p, ImageSize::Poster), alt: "{item.title}", loading: "lazy" }
                } else {
                    div { class: "placeholder", style: "background: {fallback_bg(item.tmdb_id)}", "{item.title}" }
                }
                match status_badge {
                    Some(TitleStatus::Watched) => rsx! {
                        span { class: "p-badge status seen", title: "Просмотрено",
                            Icon { svg: icons::CHECK, class: "ic sm" }
                        }
                    },
                    Some(TitleStatus::Want) => rsx! {
                        span { class: "p-badge status want", title: "Хочу посмотреть",
                            Icon { svg: icons::BOOKMARK, class: "ic sm" }
                        }
                    },
                    _ => rsx! {},
                }
                if let Some(r) = rate {
                    span { class: "p-badge rate", "{r}" }
                }
            }
            div { class: "name", "{item.title}" }
            div { class: "sub", "{sub}" }
        }
    }
}

/// "Continue watching" card: clean 16:9 frame (no giant center Play button),
/// top-right `ⓘ` and `⋯` actions, bottom-left time-left pill, and detached inset rounded progress bar.
#[component]
pub fn ContinueCard(
    item: ContinueItem,
    on_play: EventHandler<ContinueItem>,
    on_drop: EventHandler<ContinueItem>,
    #[props(default)] on_open: Option<EventHandler<(MediaKind, i64)>>,
) -> Element {
    let mut menu_open = use_signal(|| false);
    let p = &item.last;
    let name = item
        .title
        .as_ref()
        .map(|t| t.title.clone())
        .unwrap_or_else(|| "Без названия".into());
    let (pill_text, pct) = match (p.kind, item.next_episode) {
        (MediaKind::Tv, Some((s, e))) => (format!("Далее · {}", episode_code(s, e)), 0.0),
        (MediaKind::Tv, None) => (
            format!(
                "{} · {}",
                episode_code(p.season, p.episode),
                format_left_short(minutes_left(p))
            ),
            percent(p),
        ),
        (MediaKind::Movie, _) => (format_left_short(minutes_left(p)), percent(p)),
    };
    let bg = match item.title.as_ref() {
        Some(t) => backdrop_style(t),
        None => format!("background-image: {}", fallback_bg(p.tmdb_id)),
    };
    let release = p.release.as_ref().and_then(|r| r.title.clone());
    let (kind, tmdb_id) = (p.kind, p.tmdb_id);
    let it_play = item.clone();
    let it_drop = item.clone();
    let it_drop_menu = item.clone();
    rsx! {
        div { class: "cw-card",
            div { class: "cw-thumb",
                onclick: move |_| {
                    if menu_open() {
                        menu_open.set(false);
                    } else {
                        on_play.call(it_play.clone());
                    }
                },
                div { class: "cw-bg", style: "{bg}" }
                div { class: "cw-scrim" }
                div { class: "cw-top-acts",
                    if let Some(open_cb) = on_open {
                        button {
                            class: "glass-circle",
                            title: "Открыть страницу",
                            "aria-label": "Открыть страницу",
                            onclick: move |e| {
                                e.stop_propagation();
                                menu_open.set(false);
                                open_cb.call((kind, tmdb_id));
                            },
                            Icon { svg: icons::INFO, class: "ic sm" }
                        }
                    }
                    button {
                        class: "glass-circle",
                        title: "Действия",
                        "aria-label": "Действия",
                        onclick: move |e| {
                            e.stop_propagation();
                            menu_open.toggle();
                        },
                        Icon { svg: icons::MORE, class: "ic sm" }
                    }
                }
                if menu_open() {
                    div {
                        class: "cw-pop",
                        onclick: move |e| e.stop_propagation(),
                        if let Some(open_cb) = on_open {
                            button {
                                class: "cw-pop-item",
                                onclick: move |_| {
                                    menu_open.set(false);
                                    open_cb.call((kind, tmdb_id));
                                },
                                Icon { svg: icons::INFO, class: "ic sm" }
                                "Страница тайтла"
                            }
                        }
                        button {
                            class: "cw-pop-item warn",
                            onclick: move |_| {
                                menu_open.set(false);
                                on_drop.call(it_drop_menu.clone());
                            },
                            Icon { svg: icons::CLOSE, class: "ic sm" }
                            "Убрать из списка"
                        }
                    }
                }
                div { class: if pct > 0.0 { "cw-overlay-meta has-bar" } else { "cw-overlay-meta" },
                    span { class: "cw-time-pill", "{pill_text}" }
                    if item.stale {
                        span { class: "cw-stale-pill", "Давно не смотрели" }
                    }
                }
                if pct > 0.0 {
                    div { class: "cw-inset-bar",
                        div { style: "width: {pct:.1}%" }
                    }
                }
            }
            div { class: "cw-foot",
                button {
                    class: "cw-title",
                    onclick: move |_| {
                        if let Some(open_cb) = on_open {
                            open_cb.call((kind, tmdb_id));
                        } else {
                            on_play.call(item.clone());
                        }
                    },
                    "{name}"
                }
                if item.stale {
                    button {
                        class: "cw-drop",
                        onclick: move |_| on_drop.call(it_drop.clone()),
                        "Убрать"
                    }
                }
            }
            if let Some(r) = release {
                div { class: "cw-rel", "{r}" }
            }
        }
    }
}

/// Horizontal scrollable row with desktop ‹ / › navigation buttons and mobile swipe support.
#[component]
pub fn ScrollRow(
    #[props(default = "row".to_owned())] row_class: String,
    items_len: usize,
    #[props(default)] has_more: bool,
    #[props(default)] on_near_end: Option<EventHandler<()>>,
    children: Element,
) -> Element {
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    let mut can_prev = use_signal(|| false);
    let mut can_next = use_signal(|| false);

    // Re-measure overflow whenever items change (e.g. after appending page 2).
    use_effect(use_reactive((&items_len,), move |_| {
        let el = mounted();
        if let Some(el) = el {
            spawn(async move {
                if let (Ok(offset), Ok(size), Ok(rect)) = (
                    el.get_scroll_offset().await,
                    el.get_scroll_size().await,
                    el.get_client_rect().await,
                ) {
                    let w = rect.width();
                    can_prev.set(offset.x > 8.0);
                    can_next.set(offset.x + w < size.width - 8.0);
                }
            });
        }
    }));

    let scroll_by = move |dir: f64| {
        let Some(el) = mounted() else { return };
        let on_near_end = on_near_end;
        spawn(async move {
            let (Ok(offset), Ok(size), Ok(rect)) = (
                el.get_scroll_offset().await,
                el.get_scroll_size().await,
                el.get_client_rect().await,
            ) else {
                return;
            };
            let w = rect.width();
            let max_left = (size.width - w).max(0.0);
            let step = (w * 0.82).max(240.0);
            let target = (offset.x + dir * step).clamp(0.0, max_left);
            can_prev.set(target > 8.0);
            can_next.set(target + w < size.width - 8.0);
            let _ = el
                .scroll(PixelsVector2D::new(target, offset.y), ScrollBehavior::Smooth)
                .await;
            if dir > 0.0 && has_more && target + w >= size.width - 360.0 {
                if let Some(cb) = on_near_end {
                    cb.call(());
                }
            }
        });
    };

    rsx! {
        div { class: "row-wrap",
            if can_prev() {
                button {
                    class: "row-nav prev",
                    title: "Прокрутить назад",
                    "aria-label": "Прокрутить назад",
                    onclick: move |_| scroll_by(-1.0),
                    Icon { svg: icons::CHEV_L, class: "ic" }
                }
            }
            div {
                class: "{row_class}",
                onmounted: move |e| mounted.set(Some(e.data())),
                onscroll: move |e| {
                    let left = e.scroll_left();
                    let width = f64::from(e.scroll_width());
                    let client = f64::from(e.client_width());
                    can_prev.set(left > 8.0);
                    can_next.set(left + client < width - 8.0);
                    if has_more && left + client >= width - 360.0 {
                        if let Some(cb) = on_near_end {
                            cb.call(());
                        }
                    }
                },
                {children}
            }
            if can_next() || has_more {
                button {
                    class: "row-nav next",
                    title: "Прокрутить вперёд",
                    "aria-label": "Прокрутить вперёд",
                    onclick: move |_| scroll_by(1.0),
                    Icon { svg: icons::CHEV_R, class: "ic" }
                }
            }
        }
    }
}

#[component]
pub fn PosterRow(
    title: String,
    #[props(default)] subtitle: Option<String>,
    items: Vec<TitleSummary>,
    #[props(default)] has_more: bool,
    #[props(default)] loading_more: bool,
    on_open: EventHandler<TitleSummary>,
    #[props(default)] on_load_more: Option<EventHandler<()>>,
    #[props(default)] on_open_all: Option<EventHandler<()>>,
) -> Element {
    let items_len = items.len();
    rsx! {
        section { class: "section",
            div { class: "row-head",
                div { class: "row-titles",
                    h2 { "{title}" }
                    if let Some(sub) = subtitle {
                        span { class: "row-sub", "{sub}" }
                    }
                }
                if let Some(open_all) = on_open_all {
                    button { class: "row-all", onclick: move |_| open_all.call(()),
                        "Все"
                        Icon { svg: icons::CHEV_R, class: "ic sm" }
                    }
                }
            }
            ScrollRow {
                row_class: "row".to_owned(),
                items_len,
                has_more,
                on_near_end: on_load_more,
                for item in items {
                    PosterCard { key: "{item.kind.as_str()}{item.tmdb_id}", item, on_open }
                }
                if loading_more {
                    for i in 0..2 {
                        div { key: "skel-{i}", class: "poster skel",
                            div { class: "img skel-box" }
                            div { class: "skel-line" }
                            div { class: "skel-line short" }
                        }
                    }
                }
                if let Some(open_all) = on_open_all {
                    button { class: "poster poster-all", onclick: move |_| open_all.call(()),
                        div { class: "img",
                            div { class: "all-box",
                                span { class: "all-arrow", Icon { svg: icons::CHEV_R, class: "ic lg" } }
                                span { class: "all-label", "Все" }
                            }
                        }
                        div { class: "name", "Смотреть все" }
                        div { class: "sub", if has_more { "Открыть каталог" } else { "Показать списком" } }
                    }
                }
            }
        }
    }
}

#[component]
fn ShelfSkeleton() -> Element {
    rsx! {
        section { class: "section",
            div { class: "skel-title" }
            div { class: "row",
                for i in 0..7 {
                    div { key: "row-skel-{i}", class: "poster skel",
                        div { class: "img skel-box" }
                        div { class: "skel-line" }
                        div { class: "skel-line short" }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

#[component]
pub fn HomeView(
    feed: Option<HomeFeed>,
    continue_items: Vec<ContinueItem>,
    #[props(default = "for_you".to_owned())] active_category: String,
    #[props(default)] loading_shelves: Vec<String>,
    #[props(default)] loading_row_pages: Vec<String>,
    #[props(default)] loading_more_rows: bool,
    on_open: EventHandler<TitleSummary>,
    on_play: EventHandler<ContinueItem>,
    on_drop: EventHandler<ContinueItem>,
    #[props(default)] on_open_title: Option<EventHandler<(MediaKind, i64)>>,
    #[props(default)] on_category: Option<EventHandler<String>>,
    #[props(default)] on_load_more_items: Option<EventHandler<String>>,
    #[props(default)] on_open_shelf: Option<EventHandler<(String, String, Option<String>)>>,
    #[props(default)] on_load_more_rows: Option<EventHandler<()>>,
) -> Element {
    let cont_len = continue_items.len();
    rsx! {
        if feed.as_ref().is_some_and(|f| f.demo) {
            p { class: "demo-note", "Демо-каталог: без ключа TMDB показываются встроенные примеры без постеров." }
        }
        if !continue_items.is_empty() {
            section { class: "section continue-section",
                div { class: "sec-head",
                    h2 { "Продолжить просмотр" }
                }
                ScrollRow { row_class: "row".to_owned(), items_len: cont_len,
                    for item in continue_items {
                        {
                            let open_cb = on_open_title.or_else(|| {
                                item.title.clone().map(|t| {
                                    Callback::new(move |_| on_open.call(t.clone()))
                                })
                            });
                            rsx! {
                                ContinueCard {
                                    key: "{item.last.kind.as_str()}{item.last.tmdb_id}",
                                    item,
                                    on_play,
                                    on_drop,
                                    on_open: open_cb,
                                }
                            }
                        }
                    }
                }
            }
        }
        match feed {
            None => rsx! {
                ShelfSkeleton {}
                ShelfSkeleton {}
            },
            Some(feed) => {
                let matching: Vec<_> = feed
                    .rows
                    .into_iter()
                    .filter(|r| active_category == "for_you" || r.categories.iter().any(|c| c == &active_category))
                    .collect();
                let mut loaded_rows = Vec::new();
                let mut hidden_titles = Vec::new();
                let mut hidden_count = 0usize;
                for r in matching {
                    if r.loaded {
                        loaded_rows.push(r);
                    } else {
                        if hidden_titles.len() < 3 {
                            hidden_titles.push(r.title.clone());
                        }
                        hidden_count += 1;
                    }
                }
                let hint = if hidden_titles.is_empty() {
                    String::new()
                } else {
                    format!("Дальше: {}…", hidden_titles.join(" · "))
                };
                rsx! {
                    if feed.categories.len() > 1 {
                        div { class: "chips home-cats",
                            for cat in feed.categories {
                                {
                                    let cid = cat.id.clone();
                                    rsx! {
                                        button {
                                            key: "{cat.id}",
                                            class: if active_category == cat.id { "chip active" } else { "chip" },
                                            onclick: move |_| {
                                                if let Some(cb) = on_category {
                                                    cb.call(cid.clone());
                                                }
                                            },
                                            "{cat.label}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if loaded_rows.is_empty() && hidden_count > 0 {
                        ShelfSkeleton {}
                        ShelfSkeleton {}
                    }
                    for row in loaded_rows {
                        {
                            let sid = row.id.clone();
                            let sid_more = row.id.clone();
                            let stitle = row.title.clone();
                            let ssub = row.subtitle.clone();
                            let row_loading = loading_row_pages.iter().any(|id| id == &sid);
                            let load_more_cb = on_load_more_items.map(|cb| {
                                Callback::new(move |_| cb.call(sid_more.clone()))
                            });
                            let open_all_cb = on_open_shelf.map(|cb| {
                                let id = sid.clone();
                                let title = stitle.clone();
                                let sub = ssub.clone();
                                Callback::new(move |_| cb.call((id.clone(), title.clone(), sub.clone())))
                            });
                            rsx! {
                                PosterRow {
                                    key: "{row.id}-{row.title}",
                                    title: row.title,
                                    subtitle: row.subtitle,
                                    items: row.items,
                                    has_more: row.has_more,
                                    loading_more: row_loading,
                                    on_open,
                                    on_load_more: load_more_cb,
                                    on_open_all: open_all_cb,
                                }
                            }
                        }
                    }
                    if hidden_count > 0 {
                        if loading_more_rows || !loading_shelves.is_empty() {
                            ShelfSkeleton {}
                        }
                        div { id: "home-lazy-sentinel", class: "home-more",
                            if !hint.is_empty() {
                                p { class: "muted small", "{hint}" }
                            }
                            if let Some(cb) = on_load_more_rows {
                                button {
                                    class: "btn glass",
                                    disabled: loading_more_rows,
                                    onclick: move |_| cb.call(()),
                                    if loading_more_rows {
                                        "Загружаем подборки…"
                                    } else {
                                        "Показать ещё подборки ({hidden_count})"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn ShelfView(
    title: String,
    #[props(default)] subtitle: Option<String>,
    items: Vec<TitleSummary>,
    has_more: bool,
    loading: bool,
    on_open: EventHandler<TitleSummary>,
    on_load_more: EventHandler<()>,
) -> Element {
    rsx! {
        section { class: "section",
            h2 { "{title}" }
            if let Some(sub) = subtitle {
                p { class: "muted small", "{sub}" }
            }
            if items.is_empty() && loading {
                div { class: "grid",
                    for i in 0..12 {
                        div { key: "grid-skel-{i}", class: "poster skel",
                            div { class: "img skel-box" }
                            div { class: "skel-line" }
                            div { class: "skel-line short" }
                        }
                    }
                }
            } else if items.is_empty() {
                p { class: "muted", "В этой подборке пока нет новых тайтлов." }
            } else {
                div { class: "grid",
                    for item in items {
                        PosterCard { key: "{item.kind.as_str()}{item.tmdb_id}", item, on_open }
                    }
                }
                if has_more {
                    div { class: "home-more",
                        button {
                            class: "btn glass",
                            disabled: loading,
                            onclick: move |_| on_load_more.call(()),
                            if loading { "Загружаем…" } else { "Показать ещё" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn SearchView(results: Option<SearchResults>, on_open: EventHandler<TitleSummary>) -> Element {
    let mut kind = use_signal(|| MyKindFilter::All);
    rsx! {
        section { class: "section search-section",
            match results {
                None => rsx! {
                    div { class: "empty-hint",
                        Icon { svg: icons::SEARCH, class: "ic lg muted" }
                        p { class: "muted", "Введите название фильма или сериала — по-русски или в оригинале." }
                    }
                },
                Some(r) if r.items.is_empty() => rsx! {
                    p { class: "muted", "По запросу «{r.query}» ничего не нашлось." }
                },
                Some(r) => {
                    let total = r.items.len();
                    let movies = r.items.iter().filter(|i| i.kind == MediaKind::Movie).count();
                    let tv = r.items.iter().filter(|i| i.kind == MediaKind::Tv).count();
                    let active = kind();
                    let shown: Vec<TitleSummary> = r
                        .items
                        .into_iter()
                        .filter(|i| active.matches(i.kind))
                        .collect();
                    rsx! {
                        div { class: "chips search-filters",
                            button {
                                class: if active == MyKindFilter::All { "chip active" } else { "chip" },
                                onclick: move |_| kind.set(MyKindFilter::All),
                                "Все · {total}"
                            }
                            if movies > 0 {
                                button {
                                    class: if active == MyKindFilter::Movie { "chip active" } else { "chip" },
                                    onclick: move |_| kind.set(MyKindFilter::Movie),
                                    "Фильмы · {movies}"
                                }
                            }
                            if tv > 0 {
                                button {
                                    class: if active == MyKindFilter::Tv { "chip active" } else { "chip" },
                                    onclick: move |_| kind.set(MyKindFilter::Tv),
                                    "Сериалы · {tv}"
                                }
                            }
                        }
                        if shown.is_empty() {
                            p { class: "muted", "В этой категории ничего не нашлось." }
                        } else {
                            div { class: "grid",
                                for item in shown {
                                    PosterCard { key: "{item.kind.as_str()}{item.tmdb_id}", item, on_open }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// What the main button on a title page does.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayTarget {
    pub season: i32,
    pub episode: i32,
    /// Resume position, 0 to start from the beginning.
    pub position_sec: f64,
}

/// The main button: resume where the user stopped, the next episode, or start.
pub fn play_target(details: &TitleDetails, progress: &[Progress]) -> (PlayTarget, String) {
    let last = progress.iter().max_by_key(|p| p.updated_at);
    match (details.summary.kind, last) {
        (MediaKind::Movie, Some(p)) if !p.finished => (
            PlayTarget {
                season: 0,
                episode: 0,
                position_sec: p.position_sec,
            },
            format!("Продолжить с {}", clock(p.position_sec)),
        ),
        (MediaKind::Movie, _) => (
            PlayTarget {
                season: 0,
                episode: 0,
                position_sec: 0.0,
            },
            "Смотреть".into(),
        ),
        (MediaKind::Tv, Some(p)) if !p.finished => (
            PlayTarget {
                season: p.season,
                episode: p.episode,
                position_sec: p.position_sec,
            },
            format!(
                "Продолжить {} с {}",
                episode_code(p.season, p.episode),
                clock(p.position_sec)
            ),
        ),
        (MediaKind::Tv, Some(p)) => (
            PlayTarget {
                season: p.season,
                episode: p.episode + 1,
                position_sec: 0.0,
            },
            format!("Смотреть {}", episode_code(p.season, p.episode + 1)),
        ),
        (MediaKind::Tv, None) => {
            let first = details
                .seasons
                .iter()
                .map(|s| s.number)
                .filter(|n| *n > 0)
                .min()
                .unwrap_or(1);
            (
                PlayTarget {
                    season: first,
                    episode: 1,
                    position_sec: 0.0,
                },
                format!("Смотреть {}", episode_code(first, 1)),
            )
        }
    }
}

#[component]
pub fn TitleView(
    details: TitleDetails,
    /// The season whose episodes are listed (series only).
    season: Option<Season>,
    progress: Vec<Progress>,
    status: Option<TitleStatus>,
    /// The user's own 1–5 rating.
    my_rating: Option<i16>,
    /// The remembered release for the season about to play, e.g. "LostFilm · 1080p".
    /// "Смотреть" uses it without asking; `None` means the picker opens.
    release_label: Option<String>,
    on_play: EventHandler<PlayTarget>,
    on_change_release: EventHandler<PlayTarget>,
    on_status: EventHandler<Option<TitleStatus>>,
    on_rate: EventHandler<Option<i16>>,
    /// Mark episodes watched (`true`) or not: `(season, episodes, watched)`.
    on_mark: EventHandler<(i32, Vec<i32>, bool)>,
    on_season: EventHandler<i32>,
) -> Element {
    let s = &details.summary;
    let (target, play_label) = play_target(&details, &progress);
    let current = progress
        .iter()
        .max_by_key(|p| p.updated_at)
        .map(|p| (p.season, p.episode));
    let mut meta: Vec<String> = Vec::new();
    if let Some(y) = s.year {
        meta.push(y.to_string());
    }
    if !details.genres.is_empty() {
        meta.push(details.genres.join(", "));
    }
    match (s.kind, details.runtime_min) {
        (MediaKind::Movie, Some(m)) => {
            if m >= 60 {
                meta.push(format!("{} ч {} мин", m / 60, m % 60));
            } else {
                meta.push(format!("{m} мин"));
            }
        }
        (MediaKind::Tv, _) => {
            let n = details.seasons.iter().filter(|s| s.number > 0).count();
            if n > 0 {
                meta.push(format!("{n} {}", plural(n, "сезон", "сезона", "сезонов")));
            }
        }
        _ => {}
    }
    let rate = rating(s);
    // Active resume progress for the inset bar inside the Play CTA button.
    let cta_pct: Option<f64> = progress
        .iter()
        .find(|p| {
            !p.finished
                && p.duration_sec > 0.0
                && p.position_sec > 0.0
                && (s.kind == MediaKind::Movie || (p.season == target.season && p.episode == target.episode))
        })
        .map(percent);
    // Overall poster progress: for series — watched episodes across regular seasons; for movies — watch %.
    let poster_progress: Option<(String, String, f64)> = match s.kind {
        MediaKind::Tv => {
            let total_eps: usize = details
                .seasons
                .iter()
                .filter(|ss| ss.number > 0)
                .map(|ss| ss.episode_count.max(0) as usize)
                .sum();
            let done_eps = progress.iter().filter(|p| p.season > 0 && p.finished).count();
            if total_eps > 0 && (done_eps > 0 || !progress.is_empty()) {
                let pct = (done_eps as f64 / total_eps as f64 * 100.0).clamp(0.0, 100.0);
                Some(("Сериал".to_owned(), format!("{done_eps} / {total_eps}"), pct))
            } else {
                None
            }
        }
        MediaKind::Movie => progress
            .iter()
            .find(|p| p.kind == MediaKind::Movie && !p.finished && p.duration_sec > 0.0 && p.position_sec > 0.0)
            .map(|p| {
                let pct = percent(p);
                ("Просмотрено".to_owned(), format!("{pct:.0}%"), pct)
            }),
    };
    let show_orig = s
        .original_title
        .as_deref()
        .filter(|orig| !orig.is_empty() && !orig.eq_ignore_ascii_case(&s.title));
    let rel_text = release_label.unwrap_or_else(|| "Выбрать раздачу".to_owned());
    let t_play = target.clone();
    let t_rel = target.clone();
    rsx! {
        div { class: "title-hero",
            div { class: "hero-art",
                div { class: "hero-bg", style: "{backdrop_style(s)}" }
            }
            div { class: "title-grid",
                div { class: "title-poster",
                    if let Some(p) = &s.poster_path {
                        img { src: image_url(p, ImageSize::Poster), alt: "{s.title}" }
                    } else {
                        div { class: "placeholder", style: "background: {fallback_bg(s.tmdb_id)}", "{s.title}" }
                    }
                    if let Some((lbl, val, pct)) = poster_progress {
                        div { class: "poster-prog-wrap",
                            div { class: "poster-prog-lbl",
                                span { "{lbl}" }
                                span { "{val}" }
                            }
                            div { class: "poster-prog-track",
                                div { style: "width: {pct:.1}%" }
                            }
                        }
                    }
                }
                div { class: "title-info",
                    h1 { "{s.title}" }
                    if let Some(orig) = show_orig {
                        div { class: "orig-line", "{orig}" }
                    }
                    div { class: "meta-row",
                        if let Some(r) = rate {
                            span { class: "rate-pill",
                                Icon { svg: icons::STAR, class: "ic xs" }
                                "{r}"
                            }
                        }
                        for m in meta { span { "{m}" } }
                        if let Some(id) = &details.imdb_id {
                            a {
                                class: "imdb-pill",
                                href: "https://www.imdb.com/title/{id}/",
                                target: "_blank",
                                rel: "noopener",
                                "IMDb"
                                Icon { svg: icons::EXT, class: "ic xs" }
                            }
                        }
                    }
                    if let Some(t) = details.tagline.as_deref().filter(|t| !t.is_empty()) {
                        p { class: "tagline", "{t}" }
                    }
                    if let Some(o) = s.overview.as_deref().filter(|o| !o.is_empty()) {
                        p { class: "overview", "{o}" }
                    }
                    div { class: "cta-row",
                        button {
                            class: "play-cta-card",
                            onclick: move |_| on_play.call(t_play.clone()),
                            div { class: "play-cta-top",
                                Icon { svg: icons::PLAY, class: "ic sm" }
                                span { "{play_label}" }
                            }
                            if let Some(pct) = cta_pct {
                                div { class: "play-cta-inset-track",
                                    div { style: "width: {pct:.1}%" }
                                }
                            }
                        }
                        button {
                            class: "rel-cta-btn",
                            title: "Сменить раздачу",
                            onclick: move |_| on_change_release.call(t_rel.clone()),
                            span { class: "rel-cta-l",
                                Icon { svg: icons::SWAP, class: "ic sm" }
                                span { class: "rel-cta-txt", "{rel_text}" }
                            }
                            Icon { svg: icons::CHEV_R, class: "ic sm muted" }
                        }
                    }
                    div { class: "user-meta-row",
                        StatusDropdown { status, on_status }
                        Stars { rating: my_rating, on_rate }
                    }
                }
            }
        }
        if let Some(season) = season {
            {
                let all: Vec<i32> = season.episodes.iter().map(|e| e.number).collect();
                let total_in_s = all.len();
                let done_in_s = all
                    .iter()
                    .filter(|n| progress.iter().any(|p| p.season == season.number && p.episode == **n && p.finished))
                    .count();
                let all_done = total_in_s > 0 && done_in_s == total_in_s;
                let s_num = season.number;
                rsx! {
                    section { class: "section",
                        div { class: "sec-head",
                            h2 { "Сезоны и серии" }
                            if total_in_s > 0 {
                                div { class: "season-head-r",
                                    span { class: "sec-sub", "{done_in_s} из {total_in_s} просмотрено" }
                                    button {
                                        class: if all_done { "chip sm active" } else { "chip sm" },
                                        onclick: move |_| on_mark.call((s_num, all.clone(), !all_done)),
                                        Icon { svg: icons::CHECK, class: "ic xs" }
                                        if all_done { "Снять отметки" } else { "Отметить сезон" }
                                    }
                                }
                            }
                        }
                        div { class: "chips season-chips",
                            for ss in details.seasons.iter().cloned() {
                                button {
                                    class: if ss.number == season.number { "chip active" } else { "chip" },
                                    onclick: move |_| on_season.call(ss.number),
                                    "{ss.name}"
                                }
                            }
                        }
                        div { class: "ep-list",
                            for ep in season.episodes.iter().cloned() {
                                EpisodeRow {
                                    key: "{ep.number}",
                                    tmdb_id: s.tmdb_id,
                                    season: season.number,
                                    number: ep.number,
                                    name: ep.name.clone(),
                                    overview: ep.overview.clone(),
                                    still_path: ep.still_path.clone(),
                                    runtime_min: ep.runtime_min,
                                    air_date: ep.air_date.clone(),
                                    progress: progress.iter().find(|p| p.season == season.number && p.episode == ep.number).cloned(),
                                    current: current == Some((season.number, ep.number)),
                                    on_play,
                                    on_change_release,
                                    on_mark,
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// What goes below the description: the cast, then what to watch next.
/// Loaded apart from the title, so a slow or failing TMDB never blanks the page.
#[component]
pub fn TitleExtrasView(
    extras: TitleExtras,
    on_open: EventHandler<TitleSummary>,
    /// An actor was tapped: their id.
    on_person: EventHandler<i64>,
) -> Element {
    let cast_len = extras.cast.len();
    rsx! {
        if !extras.cast.is_empty() {
            section { class: "section cast-section",
                h2 { "В ролях" }
                ScrollRow { row_class: "row cast".to_owned(), items_len: cast_len,
                    for c in extras.cast {
                        CastCard { key: "{c.id}", person: c, on_person }
                    }
                }
            }
        }
        if !extras.recommendations.is_empty() {
            PosterRow { title: "Рекомендации", items: extras.recommendations, on_open }
        }
        if !extras.similar.is_empty() {
            PosterRow { title: "Похожие", items: extras.similar, on_open }
        }
    }
}

#[component]
fn CastCard(person: CastMember, on_person: EventHandler<i64>) -> Element {
    let id = person.id;
    rsx! {
        button { class: "cast-card", onclick: move |_| on_person.call(id),
            div { class: "avatar",
                if let Some(p) = &person.profile_path {
                    img { src: image_url(p, ImageSize::Profile), alt: "{person.name}", loading: "lazy" }
                } else {
                    div { class: "placeholder", style: "background: {fallback_bg(person.id)}", "{initials(&person.name)}" }
                }
            }
            div { class: "who", "{person.name}" }
            if let Some(role) = &person.character { div { class: "role", "{role}" } }
        }
    }
}

/// An actor: photo, a few facts, the biography and everything they played in.
#[component]
pub fn PersonView(person: PersonDetails, on_open: EventHandler<TitleSummary>) -> Element {
    let mut expanded = use_signal(|| false);
    let mut facts: Vec<(&str, String)> = Vec::new();
    if let Some(b) = &person.birthday {
        facts.push(("Родился", ru_date(b)));
    }
    if let Some(d) = &person.deathday {
        facts.push(("Умер", ru_date(d)));
    }
    if let Some(p) = &person.place_of_birth {
        facts.push(("Место рождения", p.clone()));
    }
    // A short biography needs no "read more".
    let long = person.biography.as_ref().is_some_and(|b| b.chars().count() > 420);
    let n = person.credits.len();
    rsx! {
        section { class: "section person",
            div { class: "person-head",
                div { class: "person-photo",
                    if let Some(p) = &person.profile_path {
                        img { src: image_url(p, ImageSize::Poster), alt: "{person.name}" }
                    } else {
                        div { class: "placeholder", style: "background: {fallback_bg(person.id)}", "{initials(&person.name)}" }
                    }
                }
                div { class: "person-info",
                    h1 { "{person.name}" }
                    if !facts.is_empty() {
                        dl { class: "facts",
                            for (k, v) in facts {
                                dt { "{k}" } dd { "{v}" }
                            }
                        }
                    }
                    if let Some(bio) = &person.biography {
                        p { class: if expanded() || !long { "bio" } else { "bio clamp" }, "{bio}" }
                        if long {
                            button { class: "link", onclick: move |_| expanded.toggle(),
                                if expanded() { "Свернуть" } else { "Читать полностью" }
                            }
                        }
                    }
                }
            }
        }
        section { class: "section",
            h2 { "Фильмография" }
            if n == 0 {
                p { class: "muted", "TMDB не знает, где снимался этот актёр." }
            } else {
                div { class: "grid",
                    for item in person.credits {
                        PosterCard { key: "{item.kind.as_str()}{item.tmdb_id}", item, on_open }
                    }
                }
            }
        }
    }
}

/// "Брайан Крэнстон" → "БК".
fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

#[component]
fn EpisodeRow(
    tmdb_id: i64,
    season: i32,
    number: i32,
    name: String,
    overview: Option<String>,
    still_path: Option<String>,
    runtime_min: Option<i32>,
    air_date: Option<String>,
    progress: Option<Progress>,
    current: bool,
    on_play: EventHandler<PlayTarget>,
    on_change_release: EventHandler<PlayTarget>,
    on_mark: EventHandler<(i32, Vec<i32>, bool)>,
) -> Element {
    let mut sub = Vec::new();
    if let Some(m) = runtime_min {
        sub.push(format!("{m} мин"));
    }
    if let Some(d) = &air_date {
        sub.push(ru_date(d));
    }
    let sub = sub.join(" · ");
    let resume_at = progress
        .as_ref()
        .filter(|p| !p.finished)
        .map(|p| p.position_sec)
        .unwrap_or(0.0);
    let target = PlayTarget {
        season,
        episode: number,
        position_sec: resume_at,
    };
    let target_rel = target.clone();
    let done = progress.as_ref().is_some_and(|p| p.finished);
    let in_prog_pct = progress
        .as_ref()
        .filter(|p| !p.finished && p.duration_sec > 0.0 && p.position_sec > 0.0)
        .map(percent);
    let ep_tag = format!("E{number:02}");
    rsx! {
        div { class: if current { "ep-card current" } else { "ep-card" },
            button {
                class: "ep-still",
                title: "Смотреть серию {number}",
                onclick: move |_| on_play.call(target.clone()),
                if let Some(sp) = &still_path {
                    img { src: image_url(sp, ImageSize::Still), alt: "{name}", loading: "lazy" }
                } else {
                    div { class: "ep-still-ph", style: "background: {fallback_bg(tmdb_id + i64::from(season * 100 + number))}" }
                }
                span { class: "ep-num-tag", "{ep_tag}" }
                span { class: if current { "ep-play-dot show" } else { "ep-play-dot" },
                    Icon { svg: icons::PLAY, class: "ic sm" }
                }
                if let Some(pct) = in_prog_pct {
                    div { class: "ep-bar",
                        div { style: "width: {pct:.1}%" }
                    }
                }
            }
            button {
                class: "ep-body",
                onclick: move |_| on_play.call(PlayTarget { season, episode: number, position_sec: resume_at }),
                div { class: "ep-title", "{number}. {name}" }
                if !sub.is_empty() {
                    div { class: "ep-meta", "{sub}" }
                }
                if let Some(desc) = overview.as_deref().filter(|d| !d.is_empty()) {
                    div { class: "ep-desc", "{desc}" }
                }
            }
            div { class: "ep-right",
                match &progress {
                    Some(p) if p.finished => rsx! { div { class: "ep-state done", "Просмотрено" } },
                    Some(p) if p.duration_sec > 0.0 => rsx! { div { class: "ep-state", "{clock(p.position_sec)} из {clock(p.duration_sec)}" } },
                    _ => rsx! {},
                }
                div { class: "ep-acts",
                    button {
                        class: "round-act",
                        title: "Выбрать раздачу для этой серии",
                        "aria-label": "Выбрать раздачу для этой серии",
                        onclick: move |_| on_change_release.call(target_rel.clone()),
                        Icon { svg: icons::SWAP, class: "ic sm" }
                    }
                    button {
                        class: if done { "round-act done" } else { "round-act" },
                        title: if done { "Снять отметку" } else { "Отметить просмотренной" },
                        "aria-label": if done { "Снять отметку" } else { "Отметить просмотренной" },
                        onclick: move |_| on_mark.call((season, vec![number], !done)),
                        Icon { svg: icons::CHECK, class: "ic sm" }
                    }
                }
            }
        }
    }
}

fn status_dot_class(st: Option<TitleStatus>) -> &'static str {
    match st {
        Some(TitleStatus::Watching) => "st-dot watching",
        Some(TitleStatus::Want) => "st-dot want",
        Some(TitleStatus::Watched) => "st-dot watched",
        Some(TitleStatus::Dropped) => "st-dot dropped",
        None => "st-dot none",
    }
}

fn status_auto_hint(st: TitleStatus) -> &'static str {
    match st {
        TitleStatus::Watching => "авто",
        TitleStatus::Want => "вручную",
        TitleStatus::Watched => "≥ 90%",
        TitleStatus::Dropped => "авто",
    }
}

/// Compact mutually-exclusive status dropdown with automatic-status hints.
#[component]
pub fn StatusDropdown(status: Option<TitleStatus>, on_status: EventHandler<Option<TitleStatus>>) -> Element {
    let mut open = use_signal(|| false);
    let label = status.map_or("Статус просмотра", TitleStatus::label);
    let show_auto = matches!(
        status,
        Some(TitleStatus::Watching | TitleStatus::Watched | TitleStatus::Dropped)
    );
    rsx! {
        div { class: "status-dd-wrap",
            if open() {
                div { class: "dd-backdrop", onclick: move |_| open.set(false) }
            }
            button {
                class: if open() { "status-dd-btn open" } else { "status-dd-btn" },
                "aria-expanded": "{open()}",
                onclick: move |_| open.toggle(),
                span { class: "status-dd-left",
                    span { class: "{status_dot_class(status)}" }
                    span { "{label}" }
                    if show_auto {
                        span { class: "st-auto", "авто" }
                    }
                }
                Icon { svg: icons::CHEV_D, class: "ic sm muted" }
            }
            if open() {
                div { class: "status-menu",
                    for st in TitleStatus::ALL {
                        button {
                            class: if status == Some(st) { "st-item on" } else { "st-item" },
                            onclick: move |_| {
                                open.set(false);
                                on_status.call(Some(st));
                            },
                            span { class: "st-item-l",
                                span { class: "{status_dot_class(Some(st))}" }
                                span { "{st.label()}" }
                            }
                            span { class: "st-auto", "{status_auto_hint(st)}" }
                        }
                    }
                    if status.is_some() {
                        div { class: "st-div" }
                        button {
                            class: "st-item clear",
                            onclick: move |_| {
                                open.set(false);
                                on_status.call(None);
                            },
                            span { class: "st-item-l",
                                Icon { svg: icons::CLOSE, class: "ic xs" }
                                span { "Сбросить статус" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Clean interactive 1–5 star rating capsule (no duplicate numeric text; animated pop + sparkle on click).
#[component]
pub fn Stars(rating: Option<i16>, on_rate: EventHandler<Option<i16>>) -> Element {
    let mut hover = use_signal(|| None::<i16>);
    let mut popped = use_signal(|| None::<(i16, u32)>);
    let mut tick = use_signal(|| 0u32);
    let active = hover().or(rating).unwrap_or(0);
    let pop_state = popped();
    rsx! {
        div {
            class: "rating-widget",
            role: "radiogroup",
            "aria-label": "Ваша оценка",
            onmouseleave: move |_| hover.set(None),
            div { class: "rating-stars",
                for n in 1..=5i16 {
                    {
                        let is_on = n <= active;
                        let is_popped = pop_state.is_some_and(|(pn, _)| pn == n) && rating == Some(n);
                        let pop_key = if is_popped {
                            format!("star-{n}-{}", pop_state.map(|(_, t)| t).unwrap_or(0))
                        } else {
                            format!("star-{n}")
                        };
                        let cls = match (is_on, is_popped) {
                            (true, true) => "r-star on pop",
                            (true, false) => "r-star on",
                            _ => "r-star",
                        };
                        rsx! {
                            button {
                                key: "{pop_key}",
                                class: "{cls}",
                                title: if rating == Some(n) { "Убрать оценку" } else { "Поставить {n} из 5" },
                                "aria-label": "{n} из 5",
                                onmouseenter: move |_| hover.set(Some(n)),
                                onclick: move |_| {
                                    let next = if rating == Some(n) { None } else { Some(n) };
                                    if let Some(v) = next {
                                        let t = tick() + 1;
                                        tick.set(t);
                                        popped.set(Some((v, t)));
                                    } else {
                                        popped.set(None);
                                    }
                                    on_rate.call(next);
                                },
                                Icon { svg: icons::STAR, class: "ic star-ic" }
                                if is_popped {
                                    span { class: "r-spark",
                                        Icon { svg: icons::SPARKLE, class: "ic xs" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn LoginView(invite: Option<String>, error: Option<String>, children: Element) -> Element {
    rsx! {
        div { class: "login-page",
            div { class: "login-glow" }
            div { class: "login-card",
                div { class: "login-brand", BrandMark {} }
                h1 { class: "login-title",
                    "Орловский "
                    em { "кинопоказ" }
                }
                p { class: "login-desc",
                    "Фильмы и сериалы с вашего домашнего сервера. Прогресс и выбранная раздача синхронизируются между телефоном, компьютером и ТВ."
                }
                if invite.is_some() {
                    p { class: "login-invite", "Вас пригласили. Войдите через Telegram, чтобы принять приглашение." }
                }
                {children}
                if let Some(e) = error {
                    p { class: "error login-err", "{e}" }
                }
            }
        }
    }
}

fn plural(n: usize, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    let (m10, m100) = (n % 10, n % 100);
    if m10 == 1 && m100 != 11 {
        one
    } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
        few
    } else {
        many
    }
}

/// `2013-08-11` → "11 авг 2013".
fn ru_date(d: &str) -> String {
    const MONTHS: [&str; 12] = [
        "янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "ноя", "дек",
    ];
    let mut it = d.split('-');
    match (it.next(), it.next().and_then(|m| m.parse::<usize>().ok()), it.next()) {
        (Some(y), Some(m @ 1..=12), Some(day)) => format!("{} {} {y}", day.trim_start_matches('0'), MONTHS[m - 1]),
        _ => d.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_of_names() {
        assert_eq!(initials("Брайан Крэнстон"), "БК");
        assert_eq!(initials("cher"), "C");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn plurals_and_dates() {
        assert_eq!(plural(1, "сезон", "сезона", "сезонов"), "сезон");
        assert_eq!(plural(3, "сезон", "сезона", "сезонов"), "сезона");
        assert_eq!(plural(11, "сезон", "сезона", "сезонов"), "сезонов");
        assert_eq!(ru_date("2013-08-01"), "1 авг 2013");
        assert_eq!(ru_date("soon"), "soon");
    }
}
