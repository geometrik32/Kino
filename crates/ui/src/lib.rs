//! Screens and widgets of the Kino interface, as plain Dioxus components.
//!
//! Components take data and report user actions through `EventHandler`s; they
//! do no networking and no routing. The web app wires them to the API, and the
//! `kino-preview` tool renders them to static HTML for screenshots.

#![allow(non_snake_case)]

use dioxus::prelude::*;
use kino_core::{
    clock, episode_code, image_url, ContinueItem, HomeFeed, ImageSize, MediaKind, Progress, SearchResults, Season,
    TitleDetails, TitleStatus, TitleSummary,
};

/// The stylesheet all components rely on.
pub const STYLE: &str = include_str!("style.css");

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

    fn icon(self) -> &'static str {
        match self {
            Tab::Home => "⌂",
            Tab::Search => "⌕",
            Tab::Mine => "☆",
            Tab::Profile => "☺",
        }
    }
}

const TABS: [Tab; 4] = [Tab::Home, Tab::Search, Tab::Mine, Tab::Profile];

mod account;
mod player;
mod releases;
pub use account::*;
pub use player::*;
pub use releases::*;

fn hue(id: i64) -> i64 {
    (id.wrapping_mul(47)).rem_euclid(360)
}

/// Gradient used when a title has no image (demo data, or TMDB has none).
fn fallback_bg(id: i64) -> String {
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

/// Top bar on wide screens, bottom tab bar on phones.
#[component]
pub fn Shell(
    tab: Option<Tab>,
    query: String,
    user_initial: String,
    on_tab: EventHandler<Tab>,
    on_query: EventHandler<String>,
    children: Element,
) -> Element {
    rsx! {
        header { class: "topbar",
            button { class: "logo", onclick: move |_| on_tab.call(Tab::Home), "КИНО", span { "." } }
            nav { class: "nav",
                for t in TABS {
                    button {
                        class: if tab == Some(t) { "active" } else { "" },
                        onclick: move |_| on_tab.call(t),
                        "{t.label()}"
                    }
                }
            }
            label { class: "search",
                span { class: "muted", "⌕" }
                input {
                    r#type: "search",
                    placeholder: "Фильмы и сериалы",
                    value: "{query}",
                    oninput: move |e| on_query.call(e.value()),
                }
            }
            div { class: "avatar", "{user_initial}" }
        }
        main { class: "content", {children} }
        nav { class: "tabbar",
            for t in TABS {
                button {
                    class: if tab == Some(t) { "active" } else { "" },
                    onclick: move |_| on_tab.call(t),
                    b { "{t.icon()}" }
                    "{t.label()}"
                }
            }
        }
    }
}

/// Search field shown at the top of the search screen on phones.
#[component]
pub fn MobileSearch(query: String, on_query: EventHandler<String>) -> Element {
    rsx! {
        label { class: "msearch",
            span { class: "muted", "⌕" }
            input {
                r#type: "search",
                placeholder: "Фильмы и сериалы",
                value: "{query}",
                oninput: move |e| on_query.call(e.value()),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cards
// ---------------------------------------------------------------------------

#[component]
pub fn PosterCard(item: TitleSummary, on_open: EventHandler<TitleSummary>) -> Element {
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
                if let Some(r) = rate {
                    span { class: "badge rate", "{r}" }
                }
            }
            div { class: "name", "{item.title}" }
            div { class: "sub", "{sub}" }
        }
    }
}

/// "Continue watching" card: still frame, progress bar, where you stopped.
#[component]
pub fn ContinueCard(
    item: ContinueItem,
    on_play: EventHandler<ContinueItem>,
    on_drop: EventHandler<ContinueItem>,
) -> Element {
    let p = &item.last;
    let name = item
        .title
        .as_ref()
        .map(|t| t.title.clone())
        .unwrap_or_else(|| "Без названия".into());
    let (line, pct) = match (p.kind, item.next_episode) {
        (MediaKind::Tv, Some((s, e))) => (format!("Следующая серия · {}", episode_code(s, e)), 0.0),
        (MediaKind::Tv, None) => (
            format!(
                "{} · осталось {} мин",
                episode_code(p.season, p.episode),
                minutes_left(p)
            ),
            percent(p),
        ),
        (MediaKind::Movie, _) => (
            format!(
                "{} из {} · осталось {} мин",
                clock(p.position_sec),
                clock(p.duration_sec),
                minutes_left(p)
            ),
            percent(p),
        ),
    };
    let bg = match item.title.as_ref() {
        Some(t) => backdrop_style(t),
        None => format!("background-image: {}", fallback_bg(p.tmdb_id)),
    };
    let release = p.release.as_ref().and_then(|r| r.title.clone());
    let (it, it2) = (item.clone(), item.clone());
    rsx! {
        div { class: "wide",
            button { class: "hit", onclick: move |_| on_play.call(it.clone()),
                div { class: "img",
                    div { class: "hero-bg", style: "{bg}; position:absolute; inset:0; background-size:cover" }
                    div { class: "play", i { "▶" } }
                    div { class: "bar", div { style: "width: {pct:.0}%" } }
                }
                div { class: "name", "{name}" }
                div { class: "sub", "{line}" }
                if let Some(r) = release {
                    div { class: "sub", style: "opacity:.7; white-space:nowrap; overflow:hidden; text-overflow:ellipsis", "{r}" }
                }
            }
            if item.stale {
                div { class: "stale",
                    span { "Давно не смотрели." }
                    button { class: "link", onclick: move |_| on_drop.call(it2.clone()), "Бросить?" }
                }
            }
        }
    }
}

#[component]
pub fn PosterRow(title: String, items: Vec<TitleSummary>, on_open: EventHandler<TitleSummary>) -> Element {
    rsx! {
        section { class: "section",
            h2 { "{title}" }
            div { class: "row",
                for item in items {
                    PosterCard { key: "{item.kind.as_str()}{item.tmdb_id}", item, on_open }
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
    feed: HomeFeed,
    continue_items: Vec<ContinueItem>,
    on_open: EventHandler<TitleSummary>,
    on_play: EventHandler<ContinueItem>,
    on_drop: EventHandler<ContinueItem>,
) -> Element {
    // Hero: the title you watched last, otherwise the first popular one.
    let hero_item = continue_items
        .first()
        .and_then(|c| c.title.clone())
        .or_else(|| feed.rows.first().and_then(|r| r.items.first().cloned()));
    let hero_continue = continue_items.first().cloned();
    rsx! {
        if let Some(h) = hero_item {
            Hero { item: h, resume: hero_continue, on_open, on_play }
        }
        if feed.demo {
            p { class: "demo-note", "Демо-каталог: без ключа TMDB показываются встроенные примеры без постеров." }
        }
        if !continue_items.is_empty() {
            section { class: "section",
                h2 { "Продолжить просмотр" }
                div { class: "row",
                    for item in continue_items {
                        ContinueCard { key: "{item.last.kind.as_str()}{item.last.tmdb_id}", item, on_play, on_drop }
                    }
                }
            }
        }
        for row in feed.rows {
            PosterRow { key: "{row.title}", title: row.title, items: row.items, on_open }
        }
    }
}

#[component]
fn Hero(
    item: TitleSummary,
    resume: Option<ContinueItem>,
    on_open: EventHandler<TitleSummary>,
    on_play: EventHandler<ContinueItem>,
) -> Element {
    let mut meta = vec![kind_label(item.kind).to_owned()];
    if let Some(y) = item.year {
        meta.push(y.to_string());
    }
    let rate = rating(&item);
    let resume_label = resume.as_ref().map(|c| match (c.last.kind, c.next_episode) {
        (MediaKind::Tv, Some((s, e))) => format!("Смотреть {}", episode_code(s, e)),
        (MediaKind::Tv, None) => format!(
            "Продолжить {} с {}",
            episode_code(c.last.season, c.last.episode),
            clock(c.last.position_sec)
        ),
        (MediaKind::Movie, _) => format!("Продолжить с {}", clock(c.last.position_sec)),
    });
    let open_item = item.clone();
    rsx! {
        div { class: "hero",
            div { class: "hero-bg", style: "{backdrop_style(&item)}" }
            div { class: "hero-body",
                h1 { "{item.title}" }
                div { class: "meta",
                    if let Some(r) = rate { span { class: "rating", "★ {r}" } }
                    for m in meta { span { "{m}" } }
                }
                if let Some(o) = &item.overview { p { "{o}" } }
                div { class: "btns",
                    if let (Some(c), Some(label)) = (resume, resume_label) {
                        button { class: "btn primary", onclick: move |_| on_play.call(c.clone()), "▶ {label}" }
                    }
                    button { class: "btn ghost", onclick: move |_| on_open.call(open_item.clone()), "Подробнее" }
                }
            }
        }
    }
}

#[component]
pub fn SearchView(
    query: String,
    results: Option<SearchResults>,
    on_open: EventHandler<TitleSummary>,
    on_query: EventHandler<String>,
) -> Element {
    rsx! {
        MobileSearch { query: query.clone(), on_query }
        section { class: "section",
            match results {
                None => rsx! { p { class: "muted", "Введите название фильма или сериала — по-русски или в оригинале." } },
                Some(r) if r.items.is_empty() => rsx! { p { class: "muted", "По запросу «{r.query}» ничего не нашлось." } },
                Some(r) => rsx! {
                    h2 { "Найдено: {r.items.len()}" }
                    div { class: "grid",
                        for item in r.items {
                            PosterCard { key: "{item.kind.as_str()}{item.tmdb_id}", item, on_open }
                        }
                    }
                },
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
        (MediaKind::Movie, Some(m)) => meta.push(format!("{} ч {} мин", m / 60, m % 60)),
        (MediaKind::Tv, _) => {
            let n = details.seasons.iter().filter(|s| s.number > 0).count();
            meta.push(format!("{n} {}", plural(n, "сезон", "сезона", "сезонов")));
        }
        _ => {}
    }
    let rate = rating(s);
    let movie_progress = progress
        .iter()
        .find(|p| p.kind == MediaKind::Movie && !p.finished)
        .cloned();
    rsx! {
        div { class: "hero",
            div { class: "hero-bg", style: "{backdrop_style(s)}" }
            div { class: "hero-body title-top",
                div { class: "poster",
                    div { class: "img",
                        if let Some(p) = &s.poster_path {
                            img { src: image_url(p, ImageSize::Poster), alt: "{s.title}" }
                        } else {
                            div { class: "placeholder", style: "background: {fallback_bg(s.tmdb_id)}", "{s.title}" }
                        }
                    }
                }
                div {
                    h1 { "{s.title}" }
                    div { class: "meta",
                        if let Some(r) = rate { span { class: "rating", "★ {r}" } }
                        for m in meta { span { "{m}" } }
                    }
                    if let Some(t) = &details.tagline { p { class: "muted", style: "margin-bottom:8px", i { "{t}" } } }
                    if let Some(o) = &s.overview { p { "{o}" } }
                    if let Some(p) = movie_progress {
                        div { style: "position:relative; height:4px; background:var(--line); border-radius:2px; margin:-8px 0 16px; max-width:360px",
                            div { style: "height:100%; width:{percent(&p):.0}%; background:var(--accent); border-radius:2px" }
                        }
                    }
                    div { class: "btns",
                        button { class: "btn primary", onclick: {
                            let t = target.clone();
                            move |_| on_play.call(t.clone())
                        }, "▶ {play_label}" }
                    }
                    StatusPicker { status, on_status }
                    Stars { rating: my_rating, on_rate }
                    div { class: "chosen-rel",
                        if let Some(l) = release_label {
                            span { class: "muted", "Раздача: " }
                            span { "{l}" }
                        } else {
                            span { class: "muted", "Раздача будет выбрана при первом просмотре" }
                        }
                        button { class: "link", onclick: move |_| on_change_release.call(target.clone()), "Сменить" }
                    }
                    if let Some(orig) = &s.original_title {
                        dl { class: "facts",
                            dt { "Оригинал" } dd { "{orig}" }
                            if let Some(id) = &details.imdb_id { dt { "IMDb" } dd { "{id}" } }
                        }
                    }
                }
            }
        }
        if let Some(season) = season {
            section { class: "section",
                h2 { "Сезоны и серии" }
                div { class: "chips",
                    for ss in details.seasons.iter().cloned() {
                        button {
                            class: if ss.number == season.number { "chip active" } else { "chip" },
                            onclick: move |_| on_season.call(ss.number),
                            "{ss.name}"
                        }
                    }
                }
                {
                    let all: Vec<i32> = season.episodes.iter().map(|e| e.number).collect();
                    let all_done = !all.is_empty() && all.iter().all(|n| {
                        progress.iter().any(|p| p.season == season.number && p.episode == *n && p.finished)
                    });
                    let n = season.number;
                    rsx! {
                        div { class: "season-acts",
                            button { class: "link", onclick: move |_| on_mark.call((n, all.clone(), !all_done)),
                                if all_done { "Снять отметки с сезона" } else { "Отметить сезон просмотренным" }
                            }
                        }
                    }
                }
                div { class: "episodes",
                    for ep in season.episodes.iter().cloned() {
                        EpisodeRow {
                            key: "{ep.number}",
                            season: season.number,
                            number: ep.number,
                            name: ep.name.clone(),
                            runtime_min: ep.runtime_min,
                            air_date: ep.air_date.clone(),
                            progress: progress.iter().find(|p| p.season == season.number && p.episode == ep.number).cloned(),
                            current: current == Some((season.number, ep.number)),
                            on_play,
                            on_mark,
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn EpisodeRow(
    season: i32,
    number: i32,
    name: String,
    runtime_min: Option<i32>,
    air_date: Option<String>,
    progress: Option<Progress>,
    current: bool,
    on_play: EventHandler<PlayTarget>,
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
    let done = progress.as_ref().is_some_and(|p| p.finished);
    rsx! {
        div { class: if current { "ep current" } else { "ep" },
            button { class: "hit", onclick: move |_| on_play.call(target.clone()),
                div { class: "num", "{number}" }
                div {
                    div { class: "t", "{name}" }
                    div { class: "s", "{sub}" }
                    if let Some(p) = progress.as_ref().filter(|p| !p.finished) {
                        div { class: "mini", div { style: "width: {percent(p):.0}%" } }
                    }
                }
            }
            match &progress {
                Some(p) if p.finished => rsx! { div { class: "state done", "Просмотрено" } },
                Some(p) => rsx! { div { class: "state", "{clock(p.position_sec)} из {clock(p.duration_sec)}" } },
                None => rsx! { div { class: "state" } },
            }
            button {
                class: if done { "mark on" } else { "mark" },
                title: if done { "Снять отметку" } else { "Отметить просмотренной" },
                onclick: move |_| on_mark.call((season, vec![number], !done)),
                "✓"
            }
        }
    }
}

/// The four statuses; tapping the active one clears it.
#[component]
pub fn StatusPicker(status: Option<TitleStatus>, on_status: EventHandler<Option<TitleStatus>>) -> Element {
    rsx! {
        div { class: "status-row",
            for st in TitleStatus::ALL {
                button {
                    class: if status == Some(st) { "chip active" } else { "chip" },
                    onclick: move |_| on_status.call(if status == Some(st) { None } else { Some(st) }),
                    "{st.label()}"
                }
            }
        }
    }
}

/// 1–5 stars; tapping the current value removes the rating.
#[component]
pub fn Stars(rating: Option<i16>, on_rate: EventHandler<Option<i16>>) -> Element {
    let r = rating.unwrap_or(0);
    rsx! {
        div { class: "stars",
            span { class: "muted", "Ваша оценка" }
            for n in 1..=5i16 {
                button {
                    class: if n <= r { "star on" } else { "star" },
                    onclick: move |_| on_rate.call(if rating == Some(n) { None } else { Some(n) }),
                    "★"
                }
            }
        }
    }
}

#[component]
pub fn LoginView(invite: Option<String>, error: Option<String>, children: Element) -> Element {
    rsx! {
        div { class: "login",
            div { class: "box",
                div { class: "logo", "КИНО", span { "." } }
                p { "Фильмы и сериалы с вашего сервера. Прогресс просмотра синхронизируется между телефоном, компьютером и телевизором." }
                if invite.is_some() {
                    p { class: "rating", "Вас пригласили. Войдите через Telegram, чтобы принять приглашение." }
                } else {
                    p { "Вход только по приглашению, через аккаунт Telegram." }
                }
                {children}
                if let Some(e) = error { p { class: "error", "{e}" } }
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
    fn plurals_and_dates() {
        assert_eq!(plural(1, "сезон", "сезона", "сезонов"), "сезон");
        assert_eq!(plural(3, "сезон", "сезона", "сезонов"), "сезона");
        assert_eq!(plural(11, "сезон", "сезона", "сезонов"), "сезонов");
        assert_eq!(ru_date("2013-08-01"), "1 авг 2013");
        assert_eq!(ru_date("soon"), "soon");
    }
}
