//! "Мои" (titles by status) and "Профиль" (history, downloads, settings).

use chrono::{DateTime, FixedOffset, Utc};
use dioxus::prelude::*;
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
const VOICES: [&str; 5] = ["Дубляж", "LostFilm", "NewStudio", "Кубик в Кубе", "Оригинал"];
const SUBTITLES: [&str; 2] = ["Русский", "Английский"];

#[component]
pub fn ProfileView(
    name: String,
    history: Vec<HistoryEntry>,
    preloads: Vec<PreloadItem>,
    prefs: UserPrefs,
    /// Current time and the viewer's UTC offset in minutes, for dates.
    now: DateTime<Utc>,
    utc_offset_min: i32,
    on_prefs: EventHandler<UserPrefs>,
    on_delete_preload: EventHandler<i64>,
    on_open: EventHandler<(MediaKind, i64)>,
    on_logout: EventHandler<()>,
) -> Element {
    let tz = FixedOffset::east_opt(utc_offset_min * 60).unwrap_or(FixedOffset::east_opt(0).expect("UTC"));
    let p = prefs.clone();
    rsx! {
        section { class: "section profile-head",
            div { class: "avatar big", "{name.chars().next().unwrap_or('?')}" }
            div {
                h2 { "{name}" }
                button { class: "link", onclick: move |_| on_logout.call(()), "Выйти" }
            }
        }
        section { class: "section",
            h2 { "Настройки по умолчанию" }
            p { class: "muted small", "Используются, когда раздача выбирается впервые, и для дорожек в плеере." }
            div { class: "panel",
                PrefRow { name: "Качество", any: "Любое", options: QUALITIES.map(String::from).to_vec(), selected: p.quality.clone(),
                    on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { quality: v, ..p.clone() }) } }
                PrefRow { name: "Перевод", any: "Любой", options: VOICES.map(String::from).to_vec(), selected: p.voice.clone(),
                    on_select: { let p = p.clone(); move |v| on_prefs.call(UserPrefs { voice: v, ..p.clone() }) } }
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
        PreloadState::Failed => "tag warn",
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
