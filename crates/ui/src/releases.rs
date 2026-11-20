//! Release picker: the list of torrent releases for a title, with filters,
//! sorting and the remembered choice.

use dioxus::prelude::*;
use kino_core::ReleaseChoiceUpdate;
pub use kino_core::{demo_releases as mock_releases, ReleaseOption};

/// Seeds below this count make streaming unreliable; offer preloading instead.
pub const LOW_SEEDS: u32 = 5;

/// "28.4 ГБ".
pub fn human_size(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 100.0 {
        format!("{gb:.0} ГБ")
    } else if gb >= 1.0 {
        format!("{gb:.1} ГБ")
    } else {
        format!("{:.0} МБ", bytes as f64 / 1e6)
    }
}

fn quality_rank(q: &str) -> u32 {
    q.trim_end_matches('p').parse().unwrap_or(0)
}

/// Active filters; `None` means "any".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReleaseFilter {
    pub quality: Option<String>,
    pub hdr: Option<bool>,
    pub dolby_vision: Option<bool>,
    pub subtitles: Option<bool>,
    pub voice: Option<String>,
    pub language: Option<String>,
    pub season: Option<i32>,
    pub tracker: Option<String>,
    pub year: Option<i32>,
}

impl ReleaseFilter {
    /// Pre-filter a new season to the same quality and voice-over as last time.
    pub fn like(choice: &ReleaseChoiceUpdate) -> Self {
        Self {
            quality: choice.quality.clone(),
            voice: choice.voice.clone(),
            ..Self::default()
        }
    }

    pub fn matches(&self, r: &ReleaseOption) -> bool {
        fn ok<T: PartialEq>(want: &Option<T>, have: &T) -> bool {
            want.as_ref().is_none_or(|w| w == have)
        }
        ok(&self.quality, &r.quality)
            && ok(&self.hdr, &r.hdr)
            && ok(&self.dolby_vision, &r.dolby_vision)
            && ok(&self.subtitles, &r.subtitles)
            && ok(&self.voice, &r.voice)
            && ok(&self.language, &r.language)
            && self.season.is_none_or(|s| r.season == Some(s))
            && ok(&self.tracker, &r.tracker)
            && self.year.is_none_or(|y| r.year == Some(y))
    }

    pub fn active(&self) -> usize {
        [
            self.quality.is_some(),
            self.hdr.is_some(),
            self.dolby_vision.is_some(),
            self.subtitles.is_some(),
            self.voice.is_some(),
            self.language.is_some(),
            self.season.is_some(),
            self.tracker.is_some(),
            self.year.is_some(),
        ]
        .iter()
        .filter(|on| **on)
        .count()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReleaseSort {
    #[default]
    Seeders,
    Size,
    Name,
    Source,
    Date,
    Watched,
}

impl ReleaseSort {
    pub const ALL: [ReleaseSort; 6] = [
        ReleaseSort::Seeders,
        ReleaseSort::Size,
        ReleaseSort::Name,
        ReleaseSort::Source,
        ReleaseSort::Date,
        ReleaseSort::Watched,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ReleaseSort::Seeders => "По раздающим",
            ReleaseSort::Size => "По размеру",
            ReleaseSort::Name => "По названию",
            ReleaseSort::Source => "По источнику",
            ReleaseSort::Date => "По дате",
            ReleaseSort::Watched => "По просмотренным",
        }
    }
}

/// Sorts releases; `watched` are info-hashes the user has played before.
pub fn sort_releases(list: &mut [ReleaseOption], sort: ReleaseSort, watched: &[String]) {
    use std::cmp::Reverse;
    match sort {
        ReleaseSort::Seeders => list.sort_by_key(|r| Reverse(r.seeders)),
        ReleaseSort::Size => list.sort_by_key(|r| Reverse(r.size_bytes)),
        ReleaseSort::Name => list.sort_by_key(|r| r.title.to_lowercase()),
        ReleaseSort::Source => list.sort_by_key(|r| (r.tracker.clone(), Reverse(r.seeders))),
        ReleaseSort::Date => list.sort_by_key(|r| Reverse(r.added.clone())),
        ReleaseSort::Watched => list.sort_by_key(|r| (!watched.contains(&r.infohash), Reverse(r.seeders))),
    }
}

/// Which panel of the picker is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerPanel {
    Filter,
    Sort,
}

/// Distinct values of a field, for the filter options.
fn distinct<T: Ord + Clone>(list: &[ReleaseOption], f: impl Fn(&ReleaseOption) -> Option<T>) -> Vec<T> {
    let mut v: Vec<T> = list.iter().filter_map(f).collect();
    v.sort();
    v.dedup();
    v
}

/// Release picker shown after "Смотреть" (when nothing is remembered) or "Сменить".
#[component]
pub fn ReleasePicker(
    heading: String,
    releases: Vec<ReleaseOption>,
    /// The remembered release; shown on top and marked.
    chosen: Option<String>,
    /// Releases the user has played before.
    watched: Vec<String>,
    filter: ReleaseFilter,
    sort: ReleaseSort,
    panel: Option<PickerPanel>,
    /// Marks the list as sample data.
    mock: bool,
    on_filter: EventHandler<ReleaseFilter>,
    on_sort: EventHandler<ReleaseSort>,
    on_panel: EventHandler<Option<PickerPanel>>,
    on_pick: EventHandler<ReleaseOption>,
    on_preload: EventHandler<ReleaseOption>,
    on_close: EventHandler<()>,
) -> Element {
    let total = releases.len();
    let mut shown: Vec<ReleaseOption> = releases.iter().filter(|r| filter.matches(r)).cloned().collect();
    sort_releases(&mut shown, sort, &watched);
    let pinned = chosen
        .as_ref()
        .and_then(|h| releases.iter().find(|r| &r.infohash == h))
        .filter(|r| filter.matches(r))
        .cloned();
    shown.retain(|r| Some(&r.infohash) != chosen.as_ref());
    let n_active = filter.active();
    let filter_label = if n_active > 0 {
        format!("Фильтр · {n_active}")
    } else {
        "Фильтр".to_string()
    };
    let toggle = move |p: PickerPanel| {
        if panel == Some(p) {
            on_panel.call(None)
        } else {
            on_panel.call(Some(p))
        }
    };
    let shown_count = shown.len() + usize::from(pinned.is_some());
    rsx! {
        div { class: "sheet-bg", onclick: move |_| on_close.call(()),
            div { class: "sheet", onclick: move |e| e.stop_propagation(),
                h3 {
                    "Выбор раздачи"
                    if mock { span { class: "mock", "примеры" } }
                }
                div { class: "hint", "{heading}" }
                div { class: "toolbar",
                    button {
                        class: if panel == Some(PickerPanel::Filter) || n_active > 0 { "chip active" } else { "chip" },
                        onclick: move |_| toggle(PickerPanel::Filter),
                        "⚲ {filter_label}"
                    }
                    button {
                        class: if panel == Some(PickerPanel::Sort) { "chip active" } else { "chip" },
                        onclick: move |_| toggle(PickerPanel::Sort),
                        "⇅ {sort.label()}"
                    }
                    span { class: "count", "{shown_count} из {total}" }
                }
                match panel {
                    Some(PickerPanel::Filter) => rsx! { FilterPanel { releases: releases.clone(), filter: filter.clone(), on_filter } },
                    Some(PickerPanel::Sort) => rsx! {
                        div { class: "panel",
                            for s in ReleaseSort::ALL {
                                button {
                                    class: if s == sort { "opt on" } else { "opt" },
                                    onclick: move |_| { on_sort.call(s); on_panel.call(None); },
                                    "{s.label()}"
                                    if s == sort { span { "✓" } }
                                }
                            }
                        }
                    },
                    None => rsx! {},
                }
                if let Some(r) = pinned {
                    ReleaseRow { key: "pinned-{r.infohash}", release: r, chosen: true, watched: true, on_pick, on_preload }
                }
                for r in shown {
                    ReleaseRow {
                        key: "{r.infohash}",
                        watched: watched.contains(&r.infohash),
                        release: r,
                        chosen: false,
                        on_pick,
                        on_preload,
                    }
                }
                if shown_count == 0 {
                    p { class: "muted", "Под фильтр ничего не подходит. ",
                        button { class: "link", onclick: move |_| on_filter.call(ReleaseFilter::default()), "Сбросить фильтр" }
                    }
                }
            }
        }
    }
}

#[component]
fn FilterPanel(releases: Vec<ReleaseOption>, filter: ReleaseFilter, on_filter: EventHandler<ReleaseFilter>) -> Element {
    let mut qualities = distinct(&releases, |r| Some(r.quality.clone()));
    qualities.sort_by_key(|q| std::cmp::Reverse(quality_rank(q)));
    let voices = distinct(&releases, |r| Some(r.voice.clone()));
    let languages = distinct(&releases, |r| Some(r.language.clone()));
    let seasons = distinct(&releases, |r| r.season);
    let trackers = distinct(&releases, |r| Some(r.tracker.clone()));
    let years = distinct(&releases, |r| r.year);
    let set = move |f: ReleaseFilter| on_filter.call(f);
    let f = filter.clone();
    rsx! {
        div { class: "panel",
            if filter.active() > 0 {
                button { class: "link reset", onclick: move |_| set(ReleaseFilter::default()), "Сбросить фильтр" }
            }
            FilterRow { name: "Качество", any: "Любое",
                options: qualities.clone(), selected: f.quality.clone(),
                on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { quality: v, ..f.clone() }) } }
            YesNoRow { name: "HDR", value: f.hdr,
                on_select: { let f = f.clone(); move |v| set(ReleaseFilter { hdr: v, ..f.clone() }) } }
            YesNoRow { name: "Dolby Vision", value: f.dolby_vision,
                on_select: { let f = f.clone(); move |v| set(ReleaseFilter { dolby_vision: v, ..f.clone() }) } }
            YesNoRow { name: "Субтитры", value: f.subtitles,
                on_select: { let f = f.clone(); move |v| set(ReleaseFilter { subtitles: v, ..f.clone() }) } }
            FilterRow { name: "Перевод", any: "Любой",
                options: voices, selected: f.voice.clone(),
                on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { voice: v, ..f.clone() }) } }
            FilterRow { name: "Язык", any: "Любой",
                options: languages, selected: f.language.clone(),
                on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { language: v, ..f.clone() }) } }
            if !seasons.is_empty() {
                FilterRow { name: "Сезон", any: "Любой",
                    options: seasons.iter().map(|s| s.to_string()).collect::<Vec<_>>(), selected: f.season.map(|s| s.to_string()),
                    on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { season: v.and_then(|s| s.parse().ok()), ..f.clone() }) } }
            }
            FilterRow { name: "Трекер", any: "Любой",
                options: trackers, selected: f.tracker.clone(),
                on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { tracker: v, ..f.clone() }) } }
            FilterRow { name: "Год", any: "Любой",
                options: years.iter().map(|y| y.to_string()).collect::<Vec<_>>(), selected: f.year.map(|y| y.to_string()),
                on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { year: v.and_then(|y| y.parse().ok()), ..f.clone() }) } }
        }
    }
}

/// One filter: a label and chips "Любой / value / value".
#[component]
fn FilterRow(
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
fn YesNoRow(name: &'static str, value: Option<bool>, on_select: EventHandler<Option<bool>>) -> Element {
    rsx! {
        div { class: "frow",
            div { class: "fname", "{name}" }
            div { class: "fopts",
                for (label, v) in [("Не важно", None), ("Есть", Some(true)), ("Нет", Some(false))] {
                    button { class: if value == v { "chip sm active" } else { "chip sm" }, onclick: move |_| on_select.call(v), "{label}" }
                }
            }
        }
    }
}

#[component]
fn ReleaseRow(
    release: ReleaseOption,
    chosen: bool,
    watched: bool,
    on_pick: EventHandler<ReleaseOption>,
    on_preload: EventHandler<ReleaseOption>,
) -> Element {
    let low = release.seeders < LOW_SEEDS;
    let (a, b) = (release.clone(), release.clone());
    let added = ru_short_date(&release.added);
    rsx! {
        div { class: if chosen { "rel last" } else { "rel" },
            div {
                div { class: "rt", "{release.title}" }
                div { class: "tags",
                    span { class: "tag q", "{release.quality}" }
                    if release.hdr { span { class: "tag q", "HDR" } }
                    if release.dolby_vision { span { class: "tag q", "Dolby Vision" } }
                    span { class: "tag", "{release.voice}" }
                    if release.subtitles { span { class: "tag", "субтитры" } }
                    span { class: "tag", "{release.tracker}" }
                    if release.direct_play {
                        span { class: "tag ok", "играет сразу" }
                    } else if release.codec.is_some() || release.hdr {
                        // Known to need the server's help; unknown codecs are not guessed.
                        span { class: "tag warn", "перекодируется" }
                    }
                    if chosen {
                        span { class: "tag ok", "ваша раздача" }
                    } else if watched {
                        span { class: "tag ok", "смотрели" }
                    }
                }
            }
            div { class: "side",
                div { "{human_size(release.size_bytes)}" }
                div { class: if low { "seeds low" } else { "seeds" }, "▲ {release.seeders}" }
                div { "{added}" }
            }
            div { class: "act",
                button { class: "btn primary", onclick: move |_| on_pick.call(a.clone()),
                    if chosen { "▶ Смотреть" } else { "▶ Выбрать и смотреть" }
                }
                if low {
                    button { class: "btn ghost", onclick: move |_| on_preload.call(b.clone()), "⇣ Скачать заранее" }
                }
            }
        }
    }
}

/// `2024-03-08` → "08.03.24".
fn ru_short_date(d: &str) -> String {
    let p: Vec<&str> = d.split('-').collect();
    match p.as_slice() {
        [y, m, day] if y.len() == 4 => format!("{day}.{m}.{}", &y[2..]),
        _ => d.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kino_core::{MediaKind, TitleSummary};

    fn title() -> TitleSummary {
        TitleSummary {
            kind: MediaKind::Tv,
            tmdb_id: 1,
            title: "Т".into(),
            original_title: None,
            year: Some(2008),
            poster_path: None,
            backdrop_path: None,
            rating: None,
            overview: None,
        }
    }

    #[test]
    fn filter_and_sort() {
        let all = mock_releases(&title(), Some(2));
        let f = ReleaseFilter {
            quality: Some("2160p".into()),
            hdr: Some(true),
            ..Default::default()
        };
        assert_eq!(f.active(), 2);
        let mut hits: Vec<_> = all.iter().filter(|r| f.matches(r)).cloned().collect();
        assert!(hits.iter().all(|r| r.quality == "2160p" && r.hdr));
        sort_releases(&mut hits, ReleaseSort::Size, &[]);
        assert!(hits.windows(2).all(|w| w[0].size_bytes >= w[1].size_bytes));

        let mut list = all.clone();
        let watched = vec!["c".repeat(40)];
        sort_releases(&mut list, ReleaseSort::Watched, &watched);
        assert_eq!(list[0].infohash, watched[0]);
        assert!(ReleaseFilter {
            season: Some(3),
            ..Default::default()
        }
        .matches(&all[9]));
        assert_eq!(human_size(28_400_000_000), "28.4 ГБ");
        assert_eq!(ru_short_date("2024-03-08"), "08.03.24");
    }
}
