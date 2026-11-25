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
    clock, episode_code, format_rating_short, image_url, rating_description, CastMember, ContinueItem, EpisodeRating,
    HomeFeed, ImageSize, MediaKind, PeerBadge, PersonDetails, Progress, RewatchMode, SearchResults, Season,
    SeasonRating, TitleDetails, TitleExtras, TitleStatus, TitleSummary,
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

fn is_safe_tmdb_path(p: &str) -> bool {
    p.starts_with('/')
        && p.len() <= 128
        && p.bytes()
            .skip(1)
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn is_valid_imdb_id(id: &str) -> bool {
    id.starts_with("tt") && id.len() >= 4 && id.len() <= 16 && id[2..].bytes().all(|b| b.is_ascii_digit())
}

pub(crate) fn title_href(kind: MediaKind, tmdb_id: i64) -> String {
    format!("/?open={}_{tmdb_id}", kind.as_str())
}

pub(crate) fn person_href(id: i64) -> String {
    format!("/?person={id}")
}

fn encode_query_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            use std::fmt::Write as _;
            let _ = write!(&mut out, "%{b:02X}");
        }
    }
    out
}

pub(crate) fn shelf_href(id: &str) -> String {
    format!("/?shelf={}", encode_query_component(id))
}

/// True only for an unmodified primary click, allowing middle-click and Ctrl/Cmd-click to open links in new tabs on web.
#[cfg(target_arch = "wasm32")]
pub(crate) fn is_plain_primary_click(e: &Event<MouseData>) -> bool {
    let m = e.modifiers();
    let no_mods = !m.ctrl() && !m.meta() && !m.shift() && !m.alt();
    let primary = e
        .trigger_button()
        .is_none_or(|b| b == dioxus::html::input_data::MouseButton::Primary);
    no_mods && primary
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn is_plain_primary_click(_e: &Event<MouseData>) -> bool {
    true
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn handle_aux_click(_e: Event<PointerData>) {}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn handle_aux_click(e: Event<PointerData>) {
    e.prevent_default();
}

fn backdrop_style(t: &TitleSummary) -> String {
    match &t.backdrop_path {
        Some(p) if is_safe_tmdb_path(p) => {
            let url = image_url(p, ImageSize::Backdrop);
            if !url.contains(['\'', '"', '\\', '(', ')', ';', '\n', '\r']) {
                return format!("background-image: url('{url}')");
            }
            format!("background-image: {}", fallback_bg(t.tmdb_id))
        }
        _ => format!("background-image: {}", fallback_bg(t.tmdb_id)),
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
            div { class: "top-left",
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
            }
            div { class: "top-center",
                div { class: "search-box",
                    span { class: "muted", "aria-hidden": "true", Icon { svg: icons::SEARCH, class: "ic sm" } }
                    input {
                        id: "kino-search-input",
                        r#type: "search",
                        "aria-label": "Поиск фильмов и сериалов",
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
            }
            div { class: "top-right",
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

fn peer_summary_line(p: &PeerBadge) -> String {
    let mut parts = vec![p.name.clone()];
    if p.watching_now {
        parts.push("смотрит сейчас".to_owned());
    } else if let Some(st) = p.status {
        parts.push(st.label().to_lowercase());
    }
    if let Some(r) = p.rating {
        parts.push(format!("{}★", format_rating_short(r)));
    }
    parts.join(" · ")
}

#[component]
pub fn PosterCard(
    item: TitleSummary,
    #[props(default)] status_badge: Option<TitleStatus>,
    #[props(default)] hide_status_badge: bool,
    on_open: EventHandler<TitleSummary>,
) -> Element {
    let sub = match item.year {
        Some(y) => format!("{} · {y}", kind_label(item.kind)),
        None => kind_label(item.kind).to_owned(),
    };
    let rate = rating(&item);
    let eff_status = status_badge.or(if hide_status_badge { None } else { item.user_status });
    let peer_tip = if item.peers.is_empty() {
        String::new()
    } else {
        item.peers.iter().map(peer_summary_line).collect::<Vec<_>>().join("\n")
    };
    let any_live = item.peers.iter().any(|p| p.watching_now);
    let peer_count = item.peers.len();
    let href = title_href(item.kind, item.tmdb_id);
    let it = item.clone();
    rsx! {
        a {
            class: "poster",
            href: "{href}",
            onclick: move |e: Event<MouseData>| {
                if is_plain_primary_click(&e) {
                    e.prevent_default();
                    on_open.call(it.clone());
                }
            },
            onauxclick: handle_aux_click,
            div { class: "img",
                if let Some(p) = &item.poster_path {
                    img { src: image_url(p, ImageSize::Poster), alt: "{item.title}", loading: "lazy" }
                } else {
                    div { class: "placeholder", style: "background: {fallback_bg(item.tmdb_id)}", "{item.title}" }
                }
                match eff_status {
                    Some(TitleStatus::Want) => rsx! {
                        span { class: "p-badge status want", title: "Хочу посмотреть",
                            Icon { svg: icons::BOOKMARK, class: "ic sm" }
                        }
                    },
                    Some(TitleStatus::Watching) => rsx! {
                        span { class: "p-badge status watching", title: "Смотрю",
                            Icon { svg: icons::PLAY, class: "ic xs" }
                        }
                    },
                    Some(TitleStatus::WaitingEpisode | TitleStatus::WaitingRelease) => rsx! {
                        span { class: "p-badge status waiting", title: eff_status.map_or("", TitleStatus::label),
                            Icon { svg: icons::CLOCK, class: "ic sm" }
                        }
                    },
                    _ => rsx! {},
                }
                if let Some(ur) = item.user_rating.filter(|r| *r > 0.0) {
                    span { class: "p-badge rate mine", title: "Моя оценка",
                        "★ {format_rating_short(ur)}"
                    }
                } else if let Some(r) = rate {
                    span { class: "p-badge rate", "{r}" }
                }
                if peer_count > 0 {
                    span {
                        class: if any_live { "p-badge peers live" } else { "p-badge peers" },
                        title: "{peer_tip}",
                        Icon { svg: icons::USERS, class: "ic xs" }
                        if peer_count > 1 || any_live {
                            span { "{peer_count}" }
                        }
                    }
                }
                if item.preloaded {
                    span {
                        class: "p-badge preloaded",
                        title: "Скачано на сервер — запускается мгновенно",
                        Icon { svg: icons::DOWNLOAD, class: "ic xs" }
                    }
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
    let href = title_href(kind, tmdb_id);
    let it_play = item.clone();
    let it_drop = item.clone();
    let it_drop_menu = item.clone();
    rsx! {
        div { class: "cw-card",
            div { class: "cw-thumb",
                a {
                    class: "cw-thumb-link",
                    href: "{href}",
                    onclick: move |e: Event<MouseData>| {
                        if menu_open() {
                            e.prevent_default();
                            menu_open.set(false);
                        } else if is_plain_primary_click(&e) {
                            e.prevent_default();
                            on_play.call(it_play.clone());
                        }
                    },
                    onauxclick: handle_aux_click,
                    div { class: "cw-bg", style: "{bg}" }
                    div { class: "cw-scrim" }
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
                div { class: "cw-top-acts",
                    if let Some(open_cb) = on_open {
                        a {
                            class: "glass-circle",
                            href: "{href}",
                            title: "Открыть страницу",
                            "aria-label": "Открыть страницу",
                            onclick: move |e: Event<MouseData>| {
                                e.stop_propagation();
                                if is_plain_primary_click(&e) {
                                    e.prevent_default();
                                    menu_open.set(false);
                                    open_cb.call((kind, tmdb_id));
                                }
                            },
                            onauxclick: handle_aux_click,
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
                            a {
                                class: "cw-pop-item",
                                href: "{href}",
                                onclick: move |e: Event<MouseData>| {
                                    if is_plain_primary_click(&e) {
                                        e.prevent_default();
                                        menu_open.set(false);
                                        open_cb.call((kind, tmdb_id));
                                    }
                                },
                                onauxclick: handle_aux_click,
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
            }
            div { class: "cw-foot",
                a {
                    class: "cw-title",
                    href: "{href}",
                    onclick: move |e: Event<MouseData>| {
                        if is_plain_primary_click(&e) {
                            e.prevent_default();
                            if let Some(open_cb) = on_open {
                                open_cb.call((kind, tmdb_id));
                            } else {
                                on_play.call(item.clone());
                            }
                        }
                    },
                    onauxclick: handle_aux_click,
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
    #[props(default)] initial_scroll_left: f64,
    #[props(default)] on_scroll_left: Option<EventHandler<f64>>,
    children: Element,
) -> Element {
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    let mut can_prev = use_signal(|| false);
    let mut can_next = use_signal(|| false);
    let mut restored = use_signal(|| false);

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
                onmounted: move |e| {
                    let data = e.data();
                    mounted.set(Some(data.clone()));
                    if !*restored.peek() && initial_scroll_left > 1.0 {
                        restored.set(true);
                        let target_x = initial_scroll_left;
                        spawn(async move {
                            let _ = data
                                .scroll(PixelsVector2D::new(target_x, 0.0), ScrollBehavior::Instant)
                                .await;
                            if let (Ok(offset), Ok(size), Ok(rect)) = (
                                data.get_scroll_offset().await,
                                data.get_scroll_size().await,
                                data.get_client_rect().await,
                            ) {
                                let w = rect.width();
                                can_prev.set(offset.x > 8.0);
                                can_next.set(offset.x + w < size.width - 8.0);
                            }
                        });
                    }
                },
                onscroll: move |e| {
                    let left = e.scroll_left();
                    if let Some(cb) = on_scroll_left {
                        cb.call(left);
                    }
                    let width = f64::from(e.scroll_width());
                    let client = f64::from(e.client_width());
                    let next_prev = left > 8.0;
                    let next_next = left + client < width - 8.0;
                    if next_prev != *can_prev.peek() {
                        can_prev.set(next_prev);
                    }
                    if next_next != *can_next.peek() {
                        can_next.set(next_next);
                    }
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
    #[props(default)] shelf_id: String,
    title: String,
    #[props(default)] subtitle: Option<String>,
    items: Vec<TitleSummary>,
    #[props(default)] has_more: bool,
    #[props(default)] loading_more: bool,
    on_open: EventHandler<TitleSummary>,
    #[props(default)] on_load_more: Option<EventHandler<()>>,
    #[props(default)] on_open_all: Option<EventHandler<()>>,
    #[props(default)] on_load_more_shelf: Option<EventHandler<String>>,
    #[props(default)] on_open_shelf: Option<EventHandler<(String, String, Option<String>)>>,
    #[props(default)] initial_scroll_left: f64,
    #[props(default)] on_scroll_left: Option<EventHandler<f64>>,
) -> Element {
    let items_len = items.len();
    let has_open_all = on_open_all.is_some() || (on_open_shelf.is_some() && !shelf_id.is_empty());
    let sid_more = shelf_id.clone();
    let near_end = use_callback(move |_: ()| {
        if let Some(cb) = on_load_more {
            cb.call(());
        } else if let Some(cb) = on_load_more_shelf {
            cb.call(sid_more.clone());
        }
    });
    let near_end_opt = (on_load_more.is_some() || on_load_more_shelf.is_some()).then_some(near_end);
    let sid_btn = shelf_id.clone();
    let title_btn = title.clone();
    let sub_btn = subtitle.clone();
    let sid_card = shelf_id.clone();
    let title_card = title.clone();
    let sub_card = subtitle.clone();
    let shelf_link = (!shelf_id.is_empty()).then(|| shelf_href(&shelf_id));
    rsx! {
        section { class: "section",
            div { class: "row-head",
                div { class: "row-titles",
                    h2 { "{title}" }
                    if let Some(sub) = subtitle {
                        span { class: "row-sub", "{sub}" }
                    }
                }
                if has_open_all {
                    if let Some(href) = &shelf_link {
                        a {
                            class: "row-all",
                            href: "{href}",
                            onclick: move |e: Event<MouseData>| {
                                if is_plain_primary_click(&e) {
                                    e.prevent_default();
                                    if let Some(open_all) = on_open_all {
                                        open_all.call(());
                                    } else if let Some(cb) = on_open_shelf {
                                        cb.call((sid_btn.clone(), title_btn.clone(), sub_btn.clone()));
                                    }
                                }
                            },
                            onauxclick: handle_aux_click,
                            "Все"
                            Icon { svg: icons::CHEV_R, class: "ic sm" }
                        }
                    } else {
                        button {
                            class: "row-all",
                            onclick: move |_| {
                                if let Some(open_all) = on_open_all {
                                    open_all.call(());
                                }
                            },
                            "Все"
                            Icon { svg: icons::CHEV_R, class: "ic sm" }
                        }
                    }
                }
            }
            ScrollRow {
                row_class: "row".to_owned(),
                items_len,
                has_more,
                on_near_end: near_end_opt,
                initial_scroll_left,
                on_scroll_left,
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
                if has_open_all {
                    if let Some(href) = &shelf_link {
                        a {
                            class: "poster poster-all",
                            href: "{href}",
                            onclick: move |e: Event<MouseData>| {
                                if is_plain_primary_click(&e) {
                                    e.prevent_default();
                                    if let Some(open_all) = on_open_all {
                                        open_all.call(());
                                    } else if let Some(cb) = on_open_shelf {
                                        cb.call((sid_card.clone(), title_card.clone(), sub_card.clone()));
                                    }
                                }
                            },
                            onauxclick: handle_aux_click,
                            div { class: "img",
                                div { class: "all-box",
                                    span { class: "all-arrow", Icon { svg: icons::CHEV_R, class: "ic lg" } }
                                    span { class: "all-label", "Все" }
                                }
                            }
                            div { class: "name", "Смотреть все" }
                            div { class: "sub", if has_more { "Открыть каталог" } else { "Показать списком" } }
                        }
                    } else {
                        button {
                            class: "poster poster-all",
                            onclick: move |_| {
                                if let Some(open_all) = on_open_all {
                                    open_all.call(());
                                }
                            },
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
    #[props(default)] row_scrolls: std::collections::HashMap<String, f64>,
    #[props(default)] on_row_scroll: Option<EventHandler<(String, f64)>>,
    #[props(default)] initial_cats_scroll: f64,
    #[props(default)] on_cats_scroll: Option<EventHandler<f64>>,
) -> Element {
    let cont_len = continue_items.len();
    let mut cats_restored = use_signal(|| false);
    let cont_scroll = row_scrolls.get("__continue__").copied().unwrap_or(0.0);
    let on_cont_scroll = on_row_scroll.map(|cb| {
        Callback::new(move |left: f64| {
            cb.call(("__continue__".to_owned(), left));
        })
    });
    rsx! {
        if feed.as_ref().is_some_and(|f| f.demo) {
            p { class: "demo-note", "Демо-каталог: без ключа TMDB показываются встроенные примеры без постеров." }
        }
        if !continue_items.is_empty() {
            section { class: "section continue-section",
                div { class: "sec-head",
                    h2 { "Продолжить просмотр" }
                }
                ScrollRow {
                    row_class: "row".to_owned(),
                    items_len: cont_len,
                    initial_scroll_left: cont_scroll,
                    on_scroll_left: on_cont_scroll,
                    for item in continue_items {
                        ContinueCard {
                            key: "{item.last.kind.as_str()}{item.last.tmdb_id}",
                            item,
                            on_play,
                            on_drop,
                            on_open: on_open_title,
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
                        div {
                            class: "chips home-cats",
                            onmounted: move |e| {
                                if !*cats_restored.peek() && initial_cats_scroll > 1.0 {
                                    cats_restored.set(true);
                                    let data = e.data();
                                    let x = initial_cats_scroll;
                                    spawn(async move {
                                        let _ = data.scroll(PixelsVector2D::new(x, 0.0), ScrollBehavior::Instant).await;
                                    });
                                }
                            },
                            onscroll: move |e| {
                                if let Some(cb) = on_cats_scroll {
                                    cb.call(e.scroll_left());
                                }
                            },
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
                            let row_loading = loading_row_pages.iter().any(|id| id == &row.id);
                            let row_init_scroll = row_scrolls.get(&row.id).copied().unwrap_or(0.0);
                            let sid_scroll = row.id.clone();
                            let on_scroll_row = on_row_scroll.map(|cb| {
                                Callback::new(move |left: f64| {
                                    cb.call((sid_scroll.clone(), left));
                                })
                            });
                            rsx! {
                                PosterRow {
                                    key: "{row.id}-{row.title}",
                                    shelf_id: row.id,
                                    title: row.title,
                                    subtitle: row.subtitle,
                                    items: row.items,
                                    has_more: row.has_more,
                                    loading_more: row_loading,
                                    on_open,
                                    on_load_more_shelf: on_load_more_items,
                                    on_open_shelf,
                                    initial_scroll_left: row_init_scroll,
                                    on_scroll_left: on_scroll_row,
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
pub fn SearchView(
    results: Option<SearchResults>,
    on_open: EventHandler<TitleSummary>,
    #[props(default)] initial_kind: MyKindFilter,
    #[props(default)] on_kind_change: Option<EventHandler<MyKindFilter>>,
) -> Element {
    let mut kind = use_signal(|| initial_kind);
    let mut set_kind = move |next: MyKindFilter| {
        kind.set(next);
        if let Some(cb) = on_kind_change {
            cb.call(next);
        }
    };
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
                                onclick: move |_| set_kind(MyKindFilter::All),
                                "Все · {total}"
                            }
                            if movies > 0 {
                                button {
                                    class: if active == MyKindFilter::Movie { "chip active" } else { "chip" },
                                    onclick: move |_| set_kind(MyKindFilter::Movie),
                                    "Фильмы · {movies}"
                                }
                            }
                            if tv > 0 {
                                button {
                                    class: if active == MyKindFilter::Tv { "chip active" } else { "chip" },
                                    onclick: move |_| set_kind(MyKindFilter::Tv),
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

fn first_regular_season(details: &TitleDetails) -> i32 {
    details
        .seasons
        .iter()
        .map(|s| s.number)
        .filter(|n| *n > 0)
        .min()
        .unwrap_or(1)
}

fn next_play_episode(details: &TitleDetails, season: i32, episode: i32) -> Option<(i32, i32)> {
    if details.seasons.is_empty() {
        return Some((season, episode + 1));
    }
    let cur = details.seasons.iter().find(|s| s.number == season)?;
    let next = if episode < cur.episode_count {
        (season, episode + 1)
    } else {
        let next_s = details
            .seasons
            .iter()
            .filter(|s| s.number > season.max(0) && s.episode_count > 0)
            .min_by_key(|s| s.number)?;
        (next_s.number, 1)
    };
    match details.last_episode.filter(|(s, _)| *s > 0) {
        Some(last) if next > last => None,
        _ => Some(next),
    }
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
        (MediaKind::Tv, Some(p)) => {
            let (season, episode) =
                next_play_episode(details, p.season, p.episode).unwrap_or((first_regular_season(details), 1));
            (
                PlayTarget {
                    season,
                    episode,
                    position_sec: 0.0,
                },
                format!("Смотреть {}", episode_code(season, episode)),
            )
        }
        (MediaKind::Tv, None) => {
            let first = first_regular_season(details);
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

fn rewatch_targets(episode_ratings: &[EpisodeRating], mode: RewatchMode) -> Vec<(i32, i32)> {
    let mut eps: Vec<(i32, i32)> = episode_ratings
        .iter()
        .filter(|e| e.season > 0 && mode.matches(Some(e.rating)))
        .map(|e| (e.season, e.episode))
        .collect();
    eps.sort_unstable();
    eps.dedup();
    eps
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
    /// Effective 1–5 rating (e.g. derived bottom-up average once ≥80% is rated).
    #[props(default)]
    effective_rating: Option<f32>,
    #[props(default)] season_ratings: Vec<SeasonRating>,
    #[props(default)] episode_ratings: Vec<EpisodeRating>,
    #[props(default)] peers: Vec<PeerBadge>,
    #[props(default)] today_ymd: String,
    /// The remembered release for the season about to play, e.g. "LostFilm · 1080p".
    /// "Смотреть" uses it without asking; `None` means the picker opens.
    release_label: Option<String>,
    /// Label of the active release for the currently selected season tab (series only).
    #[props(default)]
    season_release_label: Option<String>,
    on_play: EventHandler<PlayTarget>,
    #[props(default)] on_play_rewatch: Option<EventHandler<(PlayTarget, RewatchMode)>>,
    on_change_release: EventHandler<PlayTarget>,
    #[props(default)] on_change_season_release: Option<EventHandler<i32>>,
    #[props(default)] on_change_episode_release: Option<EventHandler<PlayTarget>>,
    on_status: EventHandler<Option<TitleStatus>>,
    on_rate: EventHandler<Option<i16>>,
    #[props(default)] on_rate_season: Option<EventHandler<(i32, Option<i16>)>>,
    #[props(default)] on_rate_episode: Option<EventHandler<(i32, i32, Option<i16>)>>,
    /// Mark episodes watched (`true`) or not: `(season, episodes, watched)`.
    on_mark: EventHandler<(i32, Vec<i32>, bool)>,
    on_season: EventHandler<i32>,
) -> Element {
    let mut rewatch_open = use_signal(|| false);
    let s = &details.summary;
    let (target, play_label) = play_target(&details, &progress);
    let current = progress
        .iter()
        .max_by_key(|p| p.updated_at)
        .map(|p| (p.season, p.episode));
    let is_unreleased = !today_ymd.is_empty() && details.is_unreleased(&today_ymd);
    let selectable_statuses = TitleStatus::selectable_for(s.kind, is_unreleased);
    let fav5 = rewatch_targets(&episode_ratings, RewatchMode::Only5);
    let fav4 = rewatch_targets(&episode_ratings, RewatchMode::FourPlus);
    let has_rewatch_modes = s.kind == MediaKind::Tv && !fav4.is_empty() && on_play_rewatch.is_some();
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
    if is_unreleased {
        if let Some(d) = details.release_date.as_deref().filter(|d| !d.is_empty()) {
            meta.push(format!("Премьера {}", ru_date(d)));
        }
    }
    if status == Some(TitleStatus::WaitingEpisode) {
        if let Some(ne) = &details.next_episode {
            let when = ne.air_date.as_deref().map(ru_date);
            match when {
                Some(d) if !d.is_empty() => {
                    meta.push(format!("Новая серия {} · {d}", episode_code(ne.season, ne.episode),))
                }
                _ => meta.push(format!("Новая серия {}", episode_code(ne.season, ne.episode))),
            }
        }
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
                        if let Some(proj) = details.localized_status() {
                            {
                                let cls = match details.series_status.as_deref() {
                                    Some("Returning Series") => "proj-pill ongoing",
                                    Some("Ended" | "Released") => "proj-pill ended",
                                    Some("Canceled") => "proj-pill canceled",
                                    _ => "proj-pill",
                                };
                                rsx! { span { class: "{cls}", "{proj}" } }
                            }
                        }
                        if s.preloaded {
                            span { class: "proj-pill ready", title: "Готово на диске сервера — запускается без буферизации",
                                Icon { svg: icons::DOWNLOAD, class: "ic xs" }
                                "На сервере"
                            }
                        }
                        for m in meta { span { "{m}" } }
                        if let Some(id) = details.imdb_id.as_deref().filter(|id| is_valid_imdb_id(id)) {
                            a {
                                class: "imdb-pill",
                                href: "https://www.imdb.com/title/{id}/",
                                target: "_blank",
                                rel: "noopener noreferrer",
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
                        if has_rewatch_modes {
                            {
                                let t_all = t_play.clone();
                                let t_menu_all = t_play.clone();
                                let first5 = fav5.first().copied();
                                let first4 = fav4.first().copied();
                                let c5 = fav5.len();
                                let c4 = fav4.len();
                                let offer_menu_on_main =
                                    status == Some(TitleStatus::Watched) && target.position_sec == 0.0;
                                rsx! {
                                    div { class: "play-split",
                                        if rewatch_open() {
                                            div { class: "dd-backdrop", onclick: move |_| rewatch_open.set(false) }
                                        }
                                        button {
                                            class: "play-cta-card",
                                            onclick: move |_| {
                                                if offer_menu_on_main {
                                                    rewatch_open.toggle();
                                                } else if let Some(cb) = on_play_rewatch {
                                                    cb.call((t_all.clone(), RewatchMode::All));
                                                } else {
                                                    on_play.call(t_all.clone());
                                                }
                                            },
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
                                            class: if rewatch_open() { "play-split-toggle open" } else { "play-split-toggle" },
                                            title: "Режим запуска",
                                            "aria-label": "Режим запуска",
                                            "aria-expanded": "{rewatch_open()}",
                                            onclick: move |_| rewatch_open.toggle(),
                                            Icon { svg: icons::CHEV_D, class: "ic sm" }
                                        }
                                        if rewatch_open() {
                                            div { class: "rewatch-menu",
                                                button {
                                                    class: "st-item",
                                                    onclick: move |_| {
                                                        rewatch_open.set(false);
                                                        if let Some(cb) = on_play_rewatch {
                                                            cb.call((t_menu_all.clone(), RewatchMode::All));
                                                        }
                                                    },
                                                    span { class: "st-item-l",
                                                        Icon { svg: icons::PLAY, class: "ic xs acc" }
                                                        span { "{RewatchMode::All.label()}" }
                                                    }
                                                }
                                                if let Some((fs, fe)) = first5 {
                                                    button {
                                                        class: "st-item",
                                                        onclick: move |_| {
                                                            rewatch_open.set(false);
                                                            if let Some(cb) = on_play_rewatch {
                                                                cb.call((
                                                                    PlayTarget { season: fs, episode: fe, position_sec: 0.0 },
                                                                    RewatchMode::Only5,
                                                                ));
                                                            }
                                                        },
                                                        span { class: "st-item-l",
                                                            Icon { svg: icons::STAR, class: "ic xs" }
                                                            span { "{RewatchMode::Only5.label()}" }
                                                        }
                                                        span { class: "st-auto", "{c5}" }
                                                    }
                                                }
                                                if let Some((fs, fe)) = first4 {
                                                    button {
                                                        class: "st-item",
                                                        onclick: move |_| {
                                                            rewatch_open.set(false);
                                                            if let Some(cb) = on_play_rewatch {
                                                                cb.call((
                                                                    PlayTarget { season: fs, episode: fe, position_sec: 0.0 },
                                                                    RewatchMode::FourPlus,
                                                                ));
                                                            }
                                                        },
                                                        span { class: "st-item-l",
                                                            Icon { svg: icons::STAR, class: "ic xs" }
                                                            span { "{RewatchMode::FourPlus.label()}" }
                                                        }
                                                        span { class: "st-auto", "{c4}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
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
                        StatusDropdown { status, selectable: selectable_statuses.to_vec(), on_status }
                        Stars {
                            rating: my_rating,
                            effective: effective_rating,
                            show_caption: true,
                            on_rate,
                        }
                    }
                    if !peers.is_empty() {
                        div { class: "peers-strip",
                            span { class: "peers-lbl",
                                Icon { svg: icons::USERS, class: "ic xs" }
                                "Участники:"
                            }
                            for p in peers.iter() {
                                span {
                                    key: "{p.user_id}",
                                    class: if p.watching_now { "peer-chip live" } else { "peer-chip" },
                                    span { class: "peer-av",
                                        AvatarContent {
                                            initial: initials(&p.name),
                                            url: p.avatar_url.clone(),
                                            alt: p.name.clone(),
                                        }
                                    }
                                    span { class: "peer-name", "{p.name}" }
                                    if p.watching_now {
                                        span { class: "peer-meta live", "смотрит" }
                                    } else if let Some(st) = p.status {
                                        span { class: "peer-meta", "{st.label()}" }
                                    }
                                    if let Some(r) = p.rating {
                                        span { class: "peer-rate", "★ {format_rating_short(r)}" }
                                    }
                                }
                            }
                        }
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
                let sr = season_ratings.iter().find(|r| r.season == s_num);
                let sr_manual = sr.and_then(|r| r.manual_rating);
                let sr_eff = sr.and_then(|r| r.effective_rating);
                rsx! {
                    section { class: "section",
                        div { class: "sec-head",
                            h2 { "Сезоны и серии" }
                            if total_in_s > 0 {
                                div { class: "season-head-r",
                                    if s_num > 0 {
                                        if let Some(on_rs) = on_rate_season {
                                            Stars {
                                                rating: sr_manual,
                                                effective: sr_eff,
                                                compact: true,
                                                on_rate: move |r| on_rs.call((s_num, r)),
                                            }
                                        }
                                    }
                                    span { class: "sec-sub", "{done_in_s} из {total_in_s} просмотрено" }
                                    if s_num > 0 {
                                        {
                                            let s_rel_lbl = season_release_label
                                                .clone()
                                                .unwrap_or_else(|| format!("Раздача {s_num} сезона"));
                                            let s_rel_active = season_release_label.is_some();
                                            let on_sr = on_change_season_release;
                                            let on_cr = on_change_release;
                                            rsx! {
                                                button {
                                                    class: if s_rel_active { "chip sm active" } else { "chip sm" },
                                                    title: "Выбрать раздачу для этого сезона",
                                                    onclick: move |_| {
                                                        if let Some(cb) = on_sr {
                                                            cb.call(s_num);
                                                        } else {
                                                            on_cr.call(PlayTarget { season: s_num, episode: 0, position_sec: 0.0 });
                                                        }
                                                    },
                                                    Icon { svg: icons::SWAP, class: "ic xs" }
                                                    "{s_rel_lbl}"
                                                }
                                            }
                                        }
                                    }
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
                                {
                                    let ep_num = ep.number;
                                    let ep_r = episode_ratings
                                        .iter()
                                        .find(|r| r.season == season.number && r.episode == ep_num)
                                        .map(|r| r.rating);
                                    rsx! {
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
                                            rating: ep_r,
                                            on_play,
                                            on_change_release: on_change_episode_release.unwrap_or(on_change_release),
                                            on_mark,
                                            on_rate: move |r| {
                                                if let Some(cb) = on_rate_episode {
                                                    cb.call((s_num, ep_num, r));
                                                }
                                            },
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
        if !extras.collection.is_empty() {
            PosterRow {
                title: "Франшиза",
                subtitle: extras.collection_name,
                items: extras.collection,
                on_open,
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
    let href = person_href(id);
    rsx! {
        a {
            class: "cast-card",
            href: "{href}",
            onclick: move |e: Event<MouseData>| {
                if is_plain_primary_click(&e) {
                    e.prevent_default();
                    on_person.call(id);
                }
            },
            onauxclick: handle_aux_click,
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
    #[props(default)] rating: Option<i16>,
    on_play: EventHandler<PlayTarget>,
    on_change_release: EventHandler<PlayTarget>,
    on_mark: EventHandler<(i32, Vec<i32>, bool)>,
    #[props(default)] on_rate: Option<EventHandler<Option<i16>>>,
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
                    Some(p) if p.position_sec > 0.0 => rsx! { div { class: "ep-state", "на {clock(p.position_sec)}" } },
                    _ => rsx! {},
                }
                if season > 0 {
                    if let Some(cb) = on_rate {
                        Stars {
                            rating,
                            compact: true,
                            on_rate: move |r| cb.call(r),
                        }
                    }
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
        Some(TitleStatus::WaitingEpisode | TitleStatus::WaitingRelease) => "st-dot waiting",
        Some(TitleStatus::Want) => "st-dot want",
        Some(TitleStatus::Watched) => "st-dot watched",
        Some(TitleStatus::Dropped) => "st-dot dropped",
        None => "st-dot none",
    }
}

/// Compact mutually-exclusive status dropdown grouped into manual and automatic choices.
#[component]
pub fn StatusDropdown(
    status: Option<TitleStatus>,
    #[props(default)] selectable: Vec<TitleStatus>,
    on_status: EventHandler<Option<TitleStatus>>,
) -> Element {
    let mut open = use_signal(|| false);
    let label = status.map_or("Статус просмотра", TitleStatus::label);
    let options: &[TitleStatus] = if selectable.is_empty() {
        &TitleStatus::ALL
    } else {
        &selectable
    };
    let (manual, automatic): (Vec<TitleStatus>, Vec<TitleStatus>) =
        options.iter().copied().partition(|st| st.is_manual());
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
                }
                Icon { svg: icons::CHEV_D, class: "ic sm muted" }
            }
            if open() {
                div { class: "status-menu",
                    for st in manual.iter().copied() {
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
                        }
                    }
                    if !manual.is_empty() && !automatic.is_empty() {
                        div { class: "st-div" }
                    }
                    for st in automatic.iter().copied() {
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

/// Interactive 1–5 star rating capsule with proportional fill, hover descriptions, and fractional display.
#[component]
pub fn Stars(
    rating: Option<i16>,
    #[props(default)] effective: Option<f32>,
    #[props(default)] compact: bool,
    #[props(default)] show_caption: bool,
    on_rate: EventHandler<Option<i16>>,
) -> Element {
    let mut hover = use_signal(|| None::<i16>);
    let mut popped = use_signal(|| None::<(i16, u32)>);
    let mut tick = use_signal(|| 0u32);
    let hovered = hover();
    let display_val = hovered
        .map(f32::from)
        .or(effective)
        .or_else(|| rating.map(f32::from))
        .unwrap_or(0.0);
    let show_fraction =
        hovered.is_none() && effective.is_some_and(|e| (e - e.round()).abs() > 0.04 || rating.is_none());
    let pop_state = popped();
    rsx! {
        div {
            class: if compact { "rating-widget compact" } else { "rating-widget" },
            role: "radiogroup",
            "aria-label": "Ваша оценка",
            onmouseleave: move |_| hover.set(None),
            if show_caption {
                if let Some(h) = hovered {
                    div { class: "rating-tip", "{h}★ — {rating_description(h)}" }
                }
            }
            div { class: "rating-stars",
                for n in 1..=5i16 {
                    {
                        let fill = ((display_val - f32::from(n - 1)).clamp(0.0, 1.0) * 100.0).round();
                        let is_on = fill > 0.0;
                        let is_popped = !compact && pop_state.is_some_and(|(pn, _)| pn == n) && rating == Some(n);
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
                        let desc = rating_description(n);
                        let tip = if rating == Some(n) {
                            format!("{n}/5 — {desc} (нажмите, чтобы снять)")
                        } else {
                            format!("{n}/5 — {desc}")
                        };
                        rsx! {
                            button {
                                key: "{pop_key}",
                                class: "{cls}",
                                role: "radio",
                                "aria-checked": if rating == Some(n) { "true" } else { "false" },
                                title: "{tip}",
                                "aria-label": "{n} из 5: {desc}",
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
                                span { class: "r-star-stack",
                                    span { class: "r-star-bg",
                                        Icon { svg: icons::STAR, class: "ic star-ic" }
                                    }
                                    span { class: "r-star-fill", style: "width: {fill:.0}%",
                                        Icon { svg: icons::STAR, class: "ic star-ic" }
                                    }
                                }
                                if is_popped {
                                    span { class: "r-spark",
                                        Icon { svg: icons::SPARKLE, class: "ic xs" }
                                    }
                                }
                            }
                        }
                    }
                }
                if show_fraction {
                    if let Some(eff) = effective {
                        span { class: "r-val", "{format_rating_short(eff)}" }
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

    #[test]
    fn play_target_handles_next_season_and_finished_series() {
        use chrono::Utc;
        use kino_core::SeasonSummary;

        let season_sum = |number, episode_count| SeasonSummary {
            number,
            name: format!("S{number}"),
            episode_count,
            year: None,
        };
        let details = TitleDetails {
            summary: TitleSummary {
                kind: MediaKind::Tv,
                tmdb_id: 1,
                title: "Сериал".into(),
                original_title: None,
                year: Some(2020),
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
                user_rating: None,
                user_status: None,
                peers: vec![],
                preloaded: false,
            },
            genres: vec![],
            runtime_min: None,
            tagline: None,
            seasons: vec![season_sum(1, 2), season_sum(2, 3)],
            imdb_id: None,
            last_episode: Some((2, 3)),
            series_status: None,
            release_date: None,
            next_episode: None,
        };
        let ep = |season, episode, finished| Progress {
            kind: MediaKind::Tv,
            tmdb_id: 1,
            season,
            episode,
            position_sec: 100.0,
            duration_sec: 200.0,
            finished,
            release: None,
            updated_at: Utc::now(),
        };

        let (_, lbl) = play_target(&details, &[ep(1, 2, true)]);
        assert_eq!(lbl, "Смотреть S02E01");

        let (t, lbl) = play_target(&details, &[ep(1, 1, true), ep(2, 3, true)]);
        assert_eq!((t.season, t.episode), (1, 1));
        assert_eq!(lbl, "Смотреть S01E01");
    }
}
