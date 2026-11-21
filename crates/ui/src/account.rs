//! "Мои" (titles by status, type and sort) and "Профиль" (theme, downloads, default release settings, history).

use chrono::{DateTime, FixedOffset, Utc};
use dioxus::prelude::*;
use kino_core::{
    clock, episode_code, image_url, HistoryEntry, ImageSize, MediaKind, PreloadItem, PreloadState, TitleState,
    TitleStatus, TitleSummary, UserPrefs,
};

use crate::icons::{self, Icon};
use crate::releases::{ReleaseSort, CODECS, SEEDER_STEPS, SIZE_STEPS};
use crate::{fallback_bg, PosterCard};

/// Filter by media kind in "Мои" and Search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MyKindFilter {
    #[default]
    All,
    Movie,
    Tv,
}

impl MyKindFilter {
    pub const ALL: [MyKindFilter; 3] = [MyKindFilter::All, MyKindFilter::Movie, MyKindFilter::Tv];

    pub fn matches(self, kind: MediaKind) -> bool {
        match self {
            MyKindFilter::All => true,
            MyKindFilter::Movie => kind == MediaKind::Movie,
            MyKindFilter::Tv => kind == MediaKind::Tv,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MyKindFilter::All => "Все типы",
            MyKindFilter::Movie => "Фильмы",
            MyKindFilter::Tv => "Сериалы",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MyKindFilter::All => "all",
            MyKindFilter::Movie => "movie",
            MyKindFilter::Tv => "tv",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "movie" => MyKindFilter::Movie,
            "tv" => MyKindFilter::Tv,
            _ => MyKindFilter::All,
        }
    }
}

/// Sort order in "Мои".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MySort {
    #[default]
    Updated,
    MyRating,
    Rating,
    Title,
}

impl MySort {
    pub const ALL: [MySort; 4] = [MySort::Updated, MySort::MyRating, MySort::Rating, MySort::Title];

    pub fn label(self) -> &'static str {
        match self {
            MySort::Updated => "По дате",
            MySort::MyRating => "По моей оценке",
            MySort::Rating => "По рейтингу",
            MySort::Title => "По названию",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MySort::Updated => "updated",
            MySort::MyRating => "my_rating",
            MySort::Rating => "rating",
            MySort::Title => "title",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "my_rating" => MySort::MyRating,
            "rating" => MySort::Rating,
            "title" => MySort::Title,
            _ => MySort::Updated,
        }
    }
}

/// Persisted filter and sort state of the "Мои" section.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MyFilters {
    pub status: Option<TitleStatus>,
    pub kind: MyKindFilter,
    pub sort: MySort,
}

impl MyFilters {
    pub fn encode(self) -> String {
        let st = self.status.map_or("all", TitleStatus::as_str);
        format!("{st}|{}|{}", self.kind.as_str(), self.sort.as_str())
    }

    pub fn decode(s: &str) -> Self {
        let mut parts = s.split('|');
        let status = parts.next().and_then(TitleStatus::parse);
        let kind = parts.next().map(MyKindFilter::parse).unwrap_or_default();
        let sort = parts.next().map(MySort::parse).unwrap_or_default();
        Self { status, kind, sort }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MyDropdown {
    Kind,
    Sort,
}

fn status_tag_class(st: TitleStatus) -> &'static str {
    match st {
        TitleStatus::Watching => "tag acc",
        TitleStatus::Watched => "tag ok",
        TitleStatus::Dropped => "tag warn",
        TitleStatus::Want => "tag",
    }
}

#[component]
pub fn MyView(
    items: Vec<TitleState>,
    filters: MyFilters,
    on_filters: EventHandler<MyFilters>,
    on_open: EventHandler<TitleSummary>,
) -> Element {
    let mut open_dd = use_signal(|| None::<MyDropdown>);

    let by_kind: Vec<&TitleState> = items.iter().filter(|i| filters.kind.matches(i.kind)).collect();
    let total_kind = by_kind.len();
    let count = |st: TitleStatus| by_kind.iter().filter(|i| i.status == Some(st)).count();

    let mut shown: Vec<TitleState> = by_kind
        .iter()
        .copied()
        .filter(|i| filters.status.is_none() || i.status == filters.status)
        .cloned()
        .collect();
    match filters.sort {
        MySort::Updated => shown.sort_by(|a, b| b.updated_at.cmp(&a.updated_at)),
        MySort::MyRating => shown.sort_by(|a, b| {
            b.rating
                .unwrap_or(0)
                .cmp(&a.rating.unwrap_or(0))
                .then_with(|| b.updated_at.cmp(&a.updated_at))
        }),
        MySort::Rating => shown.sort_by(|a, b| {
            let ra = a.title.as_ref().and_then(|t| t.rating).unwrap_or(0.0);
            let rb = b.title.as_ref().and_then(|t| t.rating).unwrap_or(0.0);
            rb.total_cmp(&ra).then_with(|| b.updated_at.cmp(&a.updated_at))
        }),
        MySort::Title => shown.sort_by(|a, b| {
            let ta = a.title.as_ref().map(|t| t.title.to_lowercase()).unwrap_or_default();
            let tb = b.title.as_ref().map(|t| t.title.to_lowercase()).unwrap_or_default();
            ta.cmp(&tb)
        }),
    }

    rsx! {
        section { class: "section mine-section",
            if open_dd().is_some() {
                div { class: "dd-backdrop", onclick: move |_| open_dd.set(None) }
            }
            div { class: "unified-bar",
                div { class: "pill-group status-pills",
                    button {
                        class: if filters.status.is_none() { "pill-btn active" } else { "pill-btn" },
                        onclick: move |_| on_filters.call(MyFilters { status: None, ..filters }),
                        "Все · {total_kind}"
                    }
                    for st in TitleStatus::ALL {
                        button {
                            class: if filters.status == Some(st) { "pill-btn active" } else { "pill-btn" },
                            onclick: move |_| on_filters.call(MyFilters { status: Some(st), ..filters }),
                            "{st.label()} · {count(st)}"
                        }
                    }
                }
                div { class: "pill-group right-pills",
                    div { class: "dd-wrap",
                        button {
                            class: if filters.kind != MyKindFilter::All || open_dd() == Some(MyDropdown::Kind) {
                                "pill-btn accent"
                            } else {
                                "pill-btn outline"
                            },
                            onclick: move |_| {
                                let next = if open_dd() == Some(MyDropdown::Kind) { None } else { Some(MyDropdown::Kind) };
                                open_dd.set(next);
                            },
                            span { "{filters.kind.label()}" }
                            Icon { svg: icons::CHEV_D, class: "ic xs" }
                        }
                        if open_dd() == Some(MyDropdown::Kind) {
                            div { class: "dd-menu right",
                                for k in MyKindFilter::ALL {
                                    button {
                                        class: if filters.kind == k { "st-item on" } else { "st-item" },
                                        onclick: move |_| {
                                            open_dd.set(None);
                                            on_filters.call(MyFilters { kind: k, ..filters });
                                        },
                                        span { "{k.label()}" }
                                        if filters.kind == k { Icon { svg: icons::CHECK, class: "ic xs" } }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "dd-wrap",
                        button {
                            class: if filters.sort != MySort::Updated || open_dd() == Some(MyDropdown::Sort) {
                                "pill-btn accent"
                            } else {
                                "pill-btn outline"
                            },
                            onclick: move |_| {
                                let next = if open_dd() == Some(MyDropdown::Sort) { None } else { Some(MyDropdown::Sort) };
                                open_dd.set(next);
                            },
                            Icon { svg: icons::SORT, class: "ic xs" }
                            span { "{filters.sort.label()}" }
                            Icon { svg: icons::CHEV_D, class: "ic xs" }
                        }
                        if open_dd() == Some(MyDropdown::Sort) {
                            div { class: "dd-menu right",
                                for s in MySort::ALL {
                                    button {
                                        class: if filters.sort == s { "st-item on" } else { "st-item" },
                                        onclick: move |_| {
                                            open_dd.set(None);
                                            on_filters.call(MyFilters { sort: s, ..filters });
                                        },
                                        span { "{s.label()}" }
                                        if filters.sort == s { Icon { svg: icons::CHECK, class: "ic xs" } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if shown.is_empty() {
                p { class: "muted",
                    if items.is_empty() {
                        "Здесь пока пусто. Статус появляется сам, когда вы начинаете смотреть, или его можно поставить на странице фильма."
                    } else {
                        "Под выбранные фильтры ничего не подходит."
                    }
                }
            } else {
                div { class: "grid",
                    for item in shown {
                        if let Some(t) = item.title.clone() {
                            div { key: "{item.kind.as_str()}{item.tmdb_id}", class: "mine",
                                PosterCard {
                                    item: t,
                                    status_badge: if filters.status.is_none() { item.status } else { None },
                                    on_open,
                                }
                                div { class: "mine-meta",
                                    if let Some(st) = item.status.filter(|_| filters.status.is_none()) {
                                        span { class: "{status_tag_class(st)}", "{st.label()}" }
                                    }
                                    if let Some(r) = item.rating {
                                        span { class: "my-stars", "{stars(r)}" }
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

fn stars(r: i16) -> String {
    let r = r.clamp(0, 5) as usize;
    format!("{}{}", "★".repeat(r), "☆".repeat(5 - r))
}

/// Quality and language options offered in the settings.
const QUALITIES: [&str; 3] = ["2160p", "1080p", "720p"];
const LANGUAGES: [&str; 3] = ["Русский", "Английский", "Украинский"];

/// Colour theme selected in the profile (`dark` / `light`, or `auto` inside Telegram).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
    Auto,
}

impl Theme {
    pub fn label(self) -> &'static str {
        match self {
            Theme::Dark => "Тёмная",
            Theme::Light => "Светлая",
            Theme::Auto => "Как в Telegram",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::Auto => "auto",
        }
    }

    pub fn parse(s: Option<&str>, in_telegram: bool) -> Self {
        match s {
            Some("dark") => Theme::Dark,
            Some("light") => Theme::Light,
            Some("auto") if in_telegram => Theme::Auto,
            _ if in_telegram => Theme::Auto,
            _ => Theme::Dark,
        }
    }
}

fn prefs_summary(p: &UserPrefs) -> Vec<(String, bool)> {
    let mut tags = Vec::new();
    if let Some(q) = &p.quality {
        tags.push((if q == "2160p" { "4K".into() } else { q.clone() }, true));
    }
    if let Some(l) = &p.language {
        tags.push((l.clone(), false));
    }
    if p.hdr == Some(true) {
        tags.push(("HDR".into(), false));
    } else if p.hdr == Some(false) {
        tags.push(("Без HDR".into(), false));
    }
    if p.dolby_vision == Some(true) {
        tags.push(("Dolby Vision".into(), false));
    } else if p.dolby_vision == Some(false) {
        tags.push(("Без DV".into(), false));
    }
    if let Some(c) = &p.codec {
        tags.push((c.clone(), false));
    }
    if p.direct_play == Some(true) {
        tags.push(("Без перекодирования".into(), false));
    }
    if let Some(n) = p.min_seeders.filter(|n| *n > 0) {
        tags.push((format!("от {n} сидов"), false));
    }
    if let Some(g) = p.max_size_gb.filter(|g| *g > 0) {
        tags.push((format!("до {g} ГБ"), false));
    }
    if let Some(s) = p.sort.as_deref() {
        let sort = ReleaseSort::from_key(Some(s));
        if sort != ReleaseSort::Seeders {
            tags.push((sort.label().to_owned(), false));
        }
    }
    tags
}

fn device_icon(dev: &str) -> &'static str {
    let d = dev.to_lowercase();
    if d.contains("телефон") || d.contains("android") || d.contains("iphone") || d.contains("telegram") {
        icons::PHONE
    } else if d.contains("телевизор") || d.contains("tv") {
        icons::TV
    } else {
        icons::LAPTOP
    }
}

#[component]
pub fn ProfileView(
    name: String,
    #[props(default)] avatar_url: Option<String>,
    history: Vec<HistoryEntry>,
    preloads: Vec<PreloadItem>,
    prefs: UserPrefs,
    theme: Theme,
    in_telegram: bool,
    /// Current time and the viewer's UTC offset in minutes, for dates.
    now: DateTime<Utc>,
    utc_offset_min: i32,
    on_theme: EventHandler<Theme>,
    on_prefs: EventHandler<UserPrefs>,
    on_delete_preload: EventHandler<i64>,
    #[props(default)] on_refresh: Option<EventHandler<()>>,
    on_open: EventHandler<(MediaKind, i64)>,
    on_logout: EventHandler<()>,
) -> Element {
    let mut prefs_open = use_signal(|| false);
    let mut hist_limit = use_signal(|| 10usize);
    let tz = FixedOffset::east_opt(utc_offset_min * 60).unwrap_or(FixedOffset::east_opt(0).expect("UTC"));
    let p = prefs.clone();
    let summary_tags = prefs_summary(&p);
    let has_custom_prefs = !summary_tags.is_empty();
    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    let total_history = history.len();
    let shown_history: Vec<HistoryEntry> = history.into_iter().take(hist_limit()).collect();
    let remaining_history = total_history.saturating_sub(shown_history.len());
    let chev_cls = if prefs_open() { "ic xs rot" } else { "ic xs" };
    rsx! {
        div { class: "profile-wrap",
            section { class: "section",
                div { class: "profile-card",
                    div { class: "profile-card-l",
                        div { class: "tg-avatar big",
                            crate::AvatarContent { initial, url: avatar_url, alt: name.clone() }
                        }
                        div {
                            h2 { class: "profile-name", "{name}" }
                            div { class: "muted profile-sub", "Синхронизация прогресса и раздач активна" }
                        }
                    }
                    button { class: "btn glass sm", onclick: move |_| on_logout.call(()),
                        Icon { svg: icons::LOGOUT, class: "ic sm" }
                        "Выйти"
                    }
                }
            }

            section { class: "section",
                div { class: "sec-head",
                    h2 { "Оформление" }
                }
                div { class: "panel theme-panel",
                    div {
                        div { class: "theme-lbl", "Цветовая тема" }
                        div { class: "muted theme-sub", "Плеер всегда остаётся в тёмном кинозале" }
                    }
                    div { class: "seg",
                        button {
                            class: if theme == Theme::Dark { "active" } else { "" },
                            onclick: move |_| on_theme.call(Theme::Dark),
                            Icon { svg: icons::MOON, class: "ic xs" }
                            "{Theme::Dark.label()}"
                        }
                        button {
                            class: if theme == Theme::Light { "active" } else { "" },
                            onclick: move |_| on_theme.call(Theme::Light),
                            Icon { svg: icons::SUN, class: "ic xs" }
                            "{Theme::Light.label()}"
                        }
                        if in_telegram {
                            button {
                                class: if theme == Theme::Auto { "active" } else { "" },
                                onclick: move |_| on_theme.call(Theme::Auto),
                                "{Theme::Auto.label()}"
                            }
                        }
                    }
                }
            }

            section { class: "section",
                div { class: "sec-head",
                    h2 { "Загрузки на сервер" }
                    if let Some(refresh) = on_refresh {
                        button { class: "chip sm", onclick: move |_| refresh.call(()),
                            Icon { svg: icons::REFRESH, class: "ic xs" }
                            "Обновить"
                        }
                    }
                }
                if preloads.is_empty() {
                    div { class: "panel muted-box",
                        "Нет активных загрузок. Для редких раздач с малым числом сидов нажмите «Заранее» в списке раздач."
                    }
                } else {
                    for item in preloads {
                        PreloadRow { key: "{item.id}", item, now, on_open, on_delete: on_delete_preload }
                    }
                }
            }

            section { class: "section",
                div { class: "sec-head",
                    h2 { "Раздачи и плеер по умолчанию" }
                }
                div { class: if prefs_open() { "acc-card open" } else { "acc-card" },
                    onclick: move |_| prefs_open.toggle(),
                    div { class: "acc-info",
                        div { class: "acc-title", "Фильтры выбора раздач" }
                        div { class: "acc-tags",
                            if summary_tags.is_empty() {
                                span { class: "tag", "Без ограничений · по сидам" }
                            } else {
                                for (lbl, accent) in summary_tags {
                                    span { class: if accent { "tag acc" } else { "tag" }, "{lbl}" }
                                }
                            }
                        }
                    }
                    button {
                        class: "btn glass sm",
                        onclick: move |e| {
                            e.stop_propagation();
                            prefs_open.toggle();
                        },
                        if prefs_open() { "Свернуть" } else { "Настроить" }
                        Icon { svg: icons::CHEV_D, class: chev_cls }
                    }
                }
                if prefs_open() {
                    div { class: "panel acc-body",
                        p { class: "muted acc-note", "Эти фильтры сразу применяются при открытии списка раздач. Выбранный язык озвучки сохраняется всегда; если по остальным ограничениям ничего не нашлось, ослабляются только они." }
                        PrefRow { name: "Качество", any: "Любое", options: QUALITIES.map(String::from).to_vec(), selected: p.quality.clone(),
                            on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { quality: v, subtitle_lang: None, subtitles: None, ..p.clone() }) } }
                        PrefRow { name: "Язык озвучки", any: "Любой", options: LANGUAGES.map(String::from).to_vec(), selected: p.language.clone(),
                            on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { language: v, subtitle_lang: None, subtitles: None, ..p.clone() }) } }
                        PrefYesNo { name: "HDR", value: p.hdr,
                            on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { hdr: v, subtitle_lang: None, subtitles: None, ..p.clone() }) } }
                        PrefYesNo { name: "Dolby Vision", value: p.dolby_vision,
                            on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { dolby_vision: v, subtitle_lang: None, subtitles: None, ..p.clone() }) } }
                        PrefRow { name: "Кодек", any: "Любой", options: CODECS.map(String::from).to_vec(), selected: p.codec.clone(),
                            on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { codec: v, subtitle_lang: None, subtitles: None, ..p.clone() }) } }
                        PrefYesNo { name: "Играет сразу", value: p.direct_play,
                            on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { direct_play: v, subtitle_lang: None, subtitles: None, ..p.clone() }) } }
                        PrefRow { name: "Раздающих", any: "Любое число", options: SEEDER_STEPS.map(|n| format!("от {n}")).to_vec(), selected: p.min_seeders.map(|n| format!("от {n}")),
                            on_select: { let p = p.clone(); move |v: Option<String>| on_prefs.call(UserPrefs { min_seeders: v.and_then(|s| s.trim_start_matches("от ").parse().ok()), subtitle_lang: None, subtitles: None, ..p.clone() }) } }
                        PrefRow { name: "Размер", any: "Любой", options: SIZE_STEPS.map(|n| format!("до {n} ГБ")).to_vec(), selected: p.max_size_gb.map(|n| format!("до {n} ГБ")),
                            on_select: { let p = p.clone(); move |v: Option<String>| on_prefs.call(UserPrefs { max_size_gb: v.and_then(|s| s.trim_start_matches("до ").trim_end_matches(" ГБ").parse().ok()), subtitle_lang: None, subtitles: None, ..p.clone() }) } }
                        PrefRow { name: "Сортировка", any: "По сидам", options: ReleaseSort::ALL.iter().skip(1).map(|s| s.label().to_owned()).collect(), selected: p.sort.as_deref().map(|k| ReleaseSort::from_key(Some(k))).filter(|s| *s != ReleaseSort::Seeders).map(|s| s.label().to_owned()),
                            on_select: { let p = p.clone(); move |v: Option<String>| on_prefs.call(UserPrefs { sort: v.and_then(|l| ReleaseSort::ALL.into_iter().find(|s| s.label() == l)).map(|s| s.key().to_owned()), subtitle_lang: None, subtitles: None, ..p.clone() }) } }
                        if has_custom_prefs {
                            div { class: "acc-reset",
                                button { class: "link", onclick: move |_| on_prefs.call(UserPrefs::default()), "Сбросить настройки по умолчанию" }
                            }
                        }
                    }
                }
            }

            section { class: "section",
                div { class: "sec-head",
                    h2 { "История просмотров" }
                }
                if shown_history.is_empty() {
                    div { class: "panel muted-box", "Пока ничего не смотрели." }
                } else {
                    div { class: "panel hist-panel",
                        for (i, h) in shown_history.into_iter().enumerate() {
                            {
                                let when = h.last_at.with_timezone(&tz).format("%d.%m.%Y %H:%M").to_string();
                                let name = h.title.as_ref().map(|t| t.title.clone()).unwrap_or_else(|| "Без названия".into());
                                let poster = h.title.as_ref().and_then(|t| t.poster_path.clone());
                                let ep = (h.kind == MediaKind::Tv).then(|| episode_code(h.season, h.episode));
                                let span = format!("{} → {}", clock(h.position_from), clock(h.position_to));
                                let (kind, id) = (h.kind, h.tmdb_id);
                                let dev = h.device.clone();
                                rsx! {
                                    button { key: "{i}-{id}", class: "hist-row", onclick: move |_| on_open.call((kind, id)),
                                        div { class: "hist-thumb",
                                            if let Some(path) = poster {
                                                img { src: image_url(&path, ImageSize::Poster), alt: "{name}", loading: "lazy" }
                                            } else {
                                                div { class: "hist-thumb-ph", style: "background: {fallback_bg(id)}" }
                                            }
                                        }
                                        div { class: "hist-body",
                                            div { class: "hist-t",
                                                "{name}"
                                                if let Some(e) = ep { span { class: "muted", " · {e}" } }
                                            }
                                            div { class: "hist-s", "{when} · {span}" }
                                        }
                                        if let Some(d) = dev {
                                            span { class: "dev-pill",
                                                Icon { svg: device_icon(&d), class: "ic xs" }
                                                "{d}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if remaining_history > 0 {
                        div { class: "hist-more",
                            button {
                                class: "chip sm",
                                onclick: move |_| hist_limit.set(hist_limit() + 10),
                                "Показать ещё ({remaining_history})"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn PrefRow(
    name: &'static str,
    any: &'static str,
    options: Vec<String>,
    selected: Option<String>,
    on_select: EventHandler<Option<String>>,
) -> Element {
    rsx! {
        div { class: "f-row",
            div { class: "f-lbl", "{name}" }
            div { class: "f-opts",
                button { class: if selected.is_none() { "chip sm active" } else { "chip sm" }, onclick: move |_| on_select.call(None), "{any}" }
                for o in options {
                    button {
                        class: if selected.as_ref() == Some(&o) { "chip sm active" } else { "chip sm" },
                        onclick: { let v = o.clone(); move |_| on_select.call(Some(v.clone())) },
                        "{o}"
                    }
                }
            }
        }
    }
}

/// "Не важно / Нужно / Не нужно".
#[component]
fn PrefYesNo(name: &'static str, value: Option<bool>, on_select: EventHandler<Option<bool>>) -> Element {
    rsx! {
        div { class: "f-row",
            div { class: "f-lbl", "{name}" }
            div { class: "f-opts",
                for (label, v) in [("Не важно", None), ("Нужно", Some(true)), ("Не нужно", Some(false))] {
                    button { class: if value == v { "chip sm active" } else { "chip sm" }, onclick: move |_| on_select.call(v), "{label}" }
                }
            }
        }
    }
}

#[component]
fn PreloadRow(
    item: PreloadItem,
    now: DateTime<Utc>,
    on_open: EventHandler<(MediaKind, i64)>,
    on_delete: EventHandler<i64>,
) -> Element {
    let pct = (item.progress * 100.0).clamp(0.0, 100.0).round();
    let status_text = match item.state {
        PreloadState::Downloading => format!("Скачивается · {pct:.0}%"),
        PreloadState::Ready => "Готово · играет с диска".to_owned(),
        other => other.label().to_owned(),
    };
    let delete_tag = match (item.state, item.delete_after) {
        (_, Some(at)) => {
            let days = (at - now).num_hours().max(0) as f64 / 24.0;
            if days < 1.0 {
                Some(("Просмотрено · удалится сегодня".to_string(), "tag warn"))
            } else {
                Some((format!("Просмотрено · удалится через {} дн.", days.ceil()), "tag warn"))
            }
        }
        (PreloadState::Ready, None) => Some(("Удалится после просмотра".to_string(), "tag")),
        _ => None,
    };
    let size = item.size_bytes.map(|b| crate::human_size(b.max(0) as u64));
    let id = item.id;
    let (kind, tmdb_id) = (item.kind, item.tmdb_id);
    let cls = match item.state {
        PreloadState::Ready => "tag ok",
        PreloadState::Downloading => "tag acc",
        PreloadState::Failed => "tag err",
        _ => "tag",
    };
    let badge_lbl = if item.kind == MediaKind::Tv && item.season > 0 {
        format!("S{:02}", item.season)
    } else if item.kind == MediaKind::Tv {
        "TV".to_owned()
    } else {
        "HD".to_owned()
    };
    let title = item.title.clone().unwrap_or_else(|| item.infohash.clone());
    rsx! {
        div { class: "dl-row", onclick: move |_| on_open.call((kind, tmdb_id)),
            div { class: "dl-poster", style: "background: {fallback_bg(tmdb_id)}",
                span { class: "dl-poster-tag", "{badge_lbl}" }
            }
            div { class: "dl-body",
                div { class: "dl-title", "{title}" }
                div { class: "dl-tags",
                    span { class: "{cls}", "{status_text}" }
                    if let Some(s) = size { span { class: "tag", "{s}" } }
                    if let Some((txt, tcls)) = delete_tag {
                        span { class: "{tcls}",
                            Icon { svg: icons::CLOCK, class: "ic xs" }
                            "{txt}"
                        }
                    }
                }
                if item.state == PreloadState::Downloading {
                    div { class: "dl-bar",
                        div { style: "width: {pct:.0}%" }
                    }
                }
            }
            button {
                class: "round-act danger",
                title: "Удалить загрузку",
                "aria-label": "Удалить загрузку",
                onclick: move |e| {
                    e.stop_propagation();
                    on_delete.call(id);
                },
                Icon { svg: icons::TRASH, class: "ic sm" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn my_filters_round_trip() {
        let f = MyFilters {
            status: Some(TitleStatus::Watching),
            kind: MyKindFilter::Tv,
            sort: MySort::MyRating,
        };
        assert_eq!(MyFilters::decode(&f.encode()), f);
        assert_eq!(MyFilters::decode("invalid"), MyFilters::default());
    }
}
