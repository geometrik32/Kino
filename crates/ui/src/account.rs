//! "Мои" (titles by status) and "Профиль" (history, downloads, settings).

use chrono::{DateTime, FixedOffset, Utc};
use dioxus::prelude::*;

use crate::releases::{ReleaseSort, CODECS, SEEDER_STEPS, SIZE_STEPS};
use kino_core::{
    clock, episode_code, HistoryEntry, MediaKind, PreloadItem, PreloadState, TitleState, TitleStatus, TitleSummary,
    UserPrefs,
};

use crate::PosterCard;

#[component]
pub fn MyView(
    items: Vec<TitleState>,
    /// `None` shows every title with a status.
    filter: Option<TitleStatus>,
    on_filter: EventHandler<Option<TitleStatus>>,
    on_open: EventHandler<TitleSummary>,
) -> Element {
    let count = |st: TitleStatus| items.iter().filter(|i| i.status == Some(st)).count();
    let shown: Vec<TitleState> = items
        .iter()
        .filter(|i| filter.is_none() || i.status == filter)
        .cloned()
        .collect();
    rsx! {
        section { class: "section",
            h2 { "Мои фильмы и сериалы" }
            div { class: "chips",
                button { class: if filter.is_none() { "chip active" } else { "chip" }, onclick: move |_| on_filter.call(None),
                    "Все · {items.len()}"
                }
                for st in TitleStatus::ALL {
                    button {
                        class: if filter == Some(st) { "chip active" } else { "chip" },
                        onclick: move |_| on_filter.call(Some(st)),
                        "{st.label()} · {count(st)}"
                    }
                }
            }
            if shown.is_empty() {
                p { class: "muted", "Здесь пока пусто. Статус появляется сам, когда вы начинаете смотреть, или его можно поставить на странице фильма." }
            } else {
                div { class: "grid",
                    for item in shown {
                        if let Some(t) = item.title.clone() {
                            div { key: "{item.kind.as_str()}{item.tmdb_id}", class: "mine",
                                PosterCard { item: t, on_open }
                                div { class: "mine-meta",
                                    if filter.is_none() {
                                        if let Some(st) = item.status { span { class: "tag", "{st.label()}" } }
                                    }
                                    if let Some(r) = item.rating { span { class: "my-stars", "{stars(r)}" } }
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

/// Quality, voice-over and subtitle options offered in the settings.
const QUALITIES: [&str; 3] = ["2160p", "1080p", "720p"];
const LANGUAGES: [&str; 3] = ["Русский", "Английский", "Украинский"];
const SUBTITLES: [&str; 2] = ["Русский", "Английский"];

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
    on_open: EventHandler<(MediaKind, i64)>,
    on_logout: EventHandler<()>,
) -> Element {
    let tz = FixedOffset::east_opt(utc_offset_min * 60).unwrap_or(FixedOffset::east_opt(0).expect("UTC"));
    let p = prefs.clone();
    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    rsx! {
        section { class: "section profile-head",
            div { class: "avatar big",
                crate::AvatarContent { initial, url: avatar_url, alt: name.clone() }
            }
            div {
                h2 { "{name}" }
                button { class: "link", onclick: move |_| on_logout.call(()), "Выйти" }
            }
        }
        section { class: "section",
            h2 { "Оформление" }
            div { class: "panel",
                div { class: "frow",
                    div { class: "fname", "Тема" }
                    div { class: "fopts",
                        for t in [Theme::Dark, Theme::Light] {
                            button {
                                class: if theme == t { "chip sm active" } else { "chip sm" },
                                onclick: move |_| on_theme.call(t),
                                "{t.label()}"
                            }
                        }
                        if in_telegram {
                            button {
                                class: if theme == Theme::Auto { "chip sm active" } else { "chip sm" },
                                onclick: move |_| on_theme.call(Theme::Auto),
                                "{Theme::Auto.label()}"
                            }
                        }
                    }
                }
            }
        }
        section { class: "section",
            h2 { "Раздачи по умолчанию" }
            p { class: "muted small", "Эти фильтры сразу применяются к списку раздач. В списке их можно изменить или сбросить. Если по ним ничего не нашлось, покажем все раздачи." }
            div { class: "panel",
                PrefRow { name: "Качество", any: "Любое", options: QUALITIES.map(String::from).to_vec(), selected: p.quality.clone(),
                    on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { quality: v, ..p.clone() }) } }
                PrefRow { name: "Язык озвучки", any: "Любой", options: LANGUAGES.map(String::from).to_vec(), selected: p.language.clone(),
                    on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { language: v, ..p.clone() }) } }
                PrefYesNo { name: "HDR", value: p.hdr,
                    on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { hdr: v, ..p.clone() }) } }
                PrefYesNo { name: "Dolby Vision", value: p.dolby_vision,
                    on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { dolby_vision: v, ..p.clone() }) } }
                PrefYesNo { name: "Субтитры в раздаче", value: p.subtitles,
                    on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { subtitles: v, ..p.clone() }) } }
                PrefRow { name: "Кодек", any: "Любой", options: CODECS.map(String::from).to_vec(), selected: p.codec.clone(),
                    on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { codec: v, ..p.clone() }) } }
                PrefYesNo { name: "Играет без перекодирования", value: p.direct_play,
                    on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { direct_play: v, ..p.clone() }) } }
                PrefRow { name: "Раздающих", any: "Любое число", options: SEEDER_STEPS.map(|n| format!("от {n}")).to_vec(), selected: p.min_seeders.map(|n| format!("от {n}")),
                    on_select: { let p = p.clone(); move |v: Option<String>| on_prefs.call(UserPrefs { min_seeders: v.and_then(|s| s.trim_start_matches("от ").parse().ok()), ..p.clone() }) } }
                PrefRow { name: "Размер", any: "Любой", options: SIZE_STEPS.map(|n| format!("до {n} ГБ")).to_vec(), selected: p.max_size_gb.map(|n| format!("до {n} ГБ")),
                    on_select: { let p = p.clone(); move |v: Option<String>| on_prefs.call(UserPrefs { max_size_gb: v.and_then(|s| s.trim_start_matches("до ").trim_end_matches(" ГБ").parse().ok()), ..p.clone() }) } }
                PrefRow { name: "Сортировка", any: "По раздающим", options: ReleaseSort::ALL.iter().skip(1).map(|s| s.label().to_owned()).collect(), selected: p.sort.as_deref().map(|k| ReleaseSort::from_key(Some(k))).filter(|s| *s != ReleaseSort::Seeders).map(|s| s.label().to_owned()),
                    on_select: { let p = p.clone(); move |v: Option<String>| on_prefs.call(UserPrefs { sort: v.and_then(|l| ReleaseSort::ALL.into_iter().find(|s| s.label() == l)).map(|s| s.key().to_owned()), ..p.clone() }) } }
            }
        }
        section { class: "section",
            h2 { "Плеер" }
            p { class: "muted small", "Язык озвучки выше выбирает и звуковую дорожку в файле. Выбор в плеере запоминается для каждого фильма отдельно." }
            div { class: "panel",
                PrefRow { name: "Субтитры", any: "Без субтитров", options: SUBTITLES.map(String::from).to_vec(), selected: p.subtitle_lang.clone(),
                    on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { subtitle_lang: v, ..p.clone() }) } }
            }
        }
        section { class: "section",
            h2 { "Загрузки на сервер" }
            if preloads.is_empty() {
                p { class: "muted", "Нет загрузок. Для раздач с малым числом сидов в выборе раздачи есть кнопка «Скачать заранее»." }
            }
            for item in preloads {
                PreloadRow { key: "{item.id}", item, now, on_delete: on_delete_preload }
            }
        }
        section { class: "section",
            h2 { "История просмотров" }
            if history.is_empty() {
                p { class: "muted", "Пока ничего не смотрели." }
            }
            div { class: "history",
                for (i, h) in history.into_iter().enumerate() {
                    {
                        let when = h.last_at.with_timezone(&tz).format("%d.%m.%Y %H:%M").to_string();
                        let name = h.title.as_ref().map(|t| t.title.clone()).unwrap_or_else(|| "Без названия".into());
                        let ep = (h.kind == MediaKind::Tv).then(|| episode_code(h.season, h.episode));
                        let span = format!("{} → {}", clock(h.position_from), clock(h.position_to));
                        let (kind, id) = (h.kind, h.tmdb_id);
                        rsx! {
                            button { key: "{i}", class: "hrow", onclick: move |_| on_open.call((kind, id)),
                                div { class: "t", "{name}"
                                    if let Some(e) = ep { span { class: "muted", " · {e}" } }
                                }
                                div { class: "s",
                                    "{when} · {span}"
                                    if let Some(d) = &h.device { " · {d}" }
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
fn PrefRow(
    name: &'static str,
    any: &'static str,
    options: Vec<String>,
    selected: Option<String>,
    on_select: EventHandler<Option<String>>,
) -> Element {
    rsx! {
        div { class: "frow",
            div { class: "fname", "{name}" }
            div { class: "fopts",
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

/// "Не важно / Есть / Нет".
#[component]
fn PrefYesNo(name: &'static str, value: Option<bool>, on_select: EventHandler<Option<bool>>) -> Element {
    rsx! {
        div { class: "frow",
            div { class: "fname", "{name}" }
            div { class: "fopts",
                for (label, v) in [("Не важно", None), ("Нужно", Some(true)), ("Не нужно", Some(false))] {
                    button { class: if value == v { "chip sm active" } else { "chip sm" }, onclick: move |_| on_select.call(v), "{label}" }
                }
            }
        }
    }
}

#[component]
fn PreloadRow(item: PreloadItem, now: DateTime<Utc>, on_delete: EventHandler<i64>) -> Element {
    let pct = (item.progress * 100.0).round();
    let note = match (item.state, item.delete_after) {
        (_, Some(at)) => {
            let days = (at - now).num_hours().max(0) as f64 / 24.0;
            if days < 1.0 {
                "просмотрено, удалится сегодня".to_string()
            } else {
                format!("просмотрено, удалится через {} дн.", days.ceil())
            }
        }
        (PreloadState::Downloading, None) => format!("{pct}%"),
        (PreloadState::Ready, None) => "можно смотреть без ожидания".to_string(),
        _ => String::new(),
    };
    let size = item.size_bytes.map(|b| crate::human_size(b.max(0) as u64));
    let id = item.id;
    let cls = match item.state {
        PreloadState::Ready => "tag ok",
        PreloadState::Failed => "tag err",
        _ => "tag",
    };
    rsx! {
        div { class: "rel",
            div {
                div { class: "rt", "{item.title.clone().unwrap_or_else(|| item.infohash.clone())}" }
                div { class: "tags",
                    span { class: "{cls}", "{item.state.label()}" }
                    if !note.is_empty() { span { class: "tag", "{note}" } }
                    if let Some(s) = size { span { class: "tag", "{s}" } }
                }
                if item.state == PreloadState::Downloading {
                    div { class: "mini wide-bar", div { style: "width: {pct}%" } }
                }
            }
            div { class: "side",
                button { class: "link", onclick: move |_| on_delete.call(id), "Удалить" }
            }
        }
    }
}
