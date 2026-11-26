//! Release picker: the list of torrent releases for a title, with filters,
//! sorting and the remembered choice.

use dioxus::prelude::*;
pub use kino_core::{demo_releases as mock_releases, ReleaseOption};
use kino_core::{format_bitrate, PreloadItem, PreloadState, ReleaseChoiceUpdate, UserPrefs};

use crate::icons::{self, seeder_signal, Icon};

/// Video codecs a release can name.
pub const CODECS: [&str; 3] = ["H.264", "HEVC", "AV1"];
/// Choices for "at least N seeders" and "at most N GB".
pub const SEEDER_STEPS: [u32; 4] = [5, 20, 50, 100];
pub const SIZE_STEPS: [u32; 5] = [5, 10, 20, 40, 80];

/// Seeds below this count make streaming unreliable; highlight preloading instead.
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

fn quality_short(q: &str) -> &str {
    if q == "2160p" {
        "4K"
    } else {
        q
    }
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
    /// "H.264", "HEVC" or "AV1"; releases that do not say are left out.
    pub codec: Option<String>,
    /// Plays in the browser without re-encoding.
    pub direct_play: Option<bool>,
    /// At least this many seeders.
    pub min_seeders: Option<u32>,
    /// At most this many gigabytes.
    pub max_size_gb: Option<u32>,
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

    /// The filters the user keeps in the profile.
    pub fn from_prefs(p: &UserPrefs) -> Self {
        Self {
            quality: p.quality.clone(),
            hdr: p.hdr,
            dolby_vision: p.dolby_vision,
            subtitles: p.subtitles,
            language: p.language.clone(),
            codec: p.codec.clone(),
            direct_play: p.direct_play,
            min_seeders: p.min_seeders.filter(|n| *n > 0),
            max_size_gb: p.max_size_gb.filter(|n| *n > 0),
            ..Self::default()
        }
    }

    /// This filter, with what is unset taken from `other`.
    pub fn or(self, other: Self) -> Self {
        Self {
            quality: self.quality.or(other.quality),
            hdr: self.hdr.or(other.hdr),
            dolby_vision: self.dolby_vision.or(other.dolby_vision),
            subtitles: self.subtitles.or(other.subtitles),
            voice: self.voice.or(other.voice),
            language: self.language.or(other.language),
            season: self.season.or(other.season),
            tracker: self.tracker.or(other.tracker),
            year: self.year.or(other.year),
            codec: self.codec.or(other.codec),
            direct_play: self.direct_play.or(other.direct_play),
            min_seeders: self.min_seeders.or(other.min_seeders),
            max_size_gb: self.max_size_gb.or(other.max_size_gb),
        }
    }

    /// The same filter, narrowed to the season about to be watched.
    pub fn for_season(self, season: i32) -> Self {
        Self {
            season: Some(season),
            ..self
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
            // A release with no single season is a pack of several (or the name does not say): it has the season too.
            && self.season.is_none_or(|s| r.season.is_none_or(|have| have == s))
            && ok(&self.tracker, &r.tracker)
            && self.year.is_none_or(|y| r.year == Some(y))
            && self.codec.as_ref().is_none_or(|c| r.codec.as_ref() == Some(c))
            && ok(&self.direct_play, &r.direct_play)
            && self.min_seeders.is_none_or(|n| r.seeders >= n)
            && self.max_size_gb.is_none_or(|g| r.size_bytes <= u64::from(g) * 1_000_000_000)
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
            self.codec.is_some(),
            self.direct_play.is_some(),
            self.min_seeders.is_some(),
            self.max_size_gb.is_some(),
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
    Bitrate,
    Name,
    Source,
    Date,
    Watched,
}

impl ReleaseSort {
    pub const ALL: [ReleaseSort; 7] = [
        ReleaseSort::Seeders,
        ReleaseSort::Size,
        ReleaseSort::Bitrate,
        ReleaseSort::Name,
        ReleaseSort::Source,
        ReleaseSort::Date,
        ReleaseSort::Watched,
    ];

    /// Name stored in the profile.
    pub fn key(self) -> &'static str {
        match self {
            ReleaseSort::Seeders => "seeders",
            ReleaseSort::Size => "size",
            ReleaseSort::Bitrate => "bitrate",
            ReleaseSort::Name => "name",
            ReleaseSort::Source => "source",
            ReleaseSort::Date => "date",
            ReleaseSort::Watched => "watched",
        }
    }

    /// The sorting named in the profile; the default one for anything unknown.
    pub fn from_key(key: Option<&str>) -> Self {
        Self::ALL.into_iter().find(|s| Some(s.key()) == key).unwrap_or_default()
    }

    pub fn label(self) -> &'static str {
        match self {
            ReleaseSort::Seeders => "По сидам",
            ReleaseSort::Size => "По размеру",
            ReleaseSort::Bitrate => "По битрейту",
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
        // Highest first; releases with an unknown bitrate go last.
        ReleaseSort::Bitrate => list.sort_by_key(|r| (r.bitrate_kbps.is_none(), Reverse(r.bitrate_kbps))),
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
    /// A line under the heading, e.g. that the default filters were relaxed.
    note: Option<String>,
    /// True for native desktop/mobile players that decode MKV/HEVC/AC3/DTS without transcoding.
    #[props(default)]
    native_player: bool,
    /// Active or ready server preloads matching this title/season (matched by infohash).
    #[props(default)]
    preloads: Vec<PreloadItem>,
    on_filter: EventHandler<ReleaseFilter>,
    on_sort: EventHandler<ReleaseSort>,
    on_panel: EventHandler<Option<PickerPanel>>,
    on_pick: EventHandler<ReleaseOption>,
    on_preload: EventHandler<ReleaseOption>,
    #[props(default)] on_cancel_preload: Option<EventHandler<i64>>,
    on_close: EventHandler<()>,
) -> Element {
    let total = releases.len();
    let mut qualities = distinct(&releases, |r| Some(r.quality.clone()));
    qualities.sort_by_key(|q| std::cmp::Reverse(quality_rank(q)));

    let mut shown: Vec<ReleaseOption> = releases.iter().filter(|r| filter.matches(r)).cloned().collect();
    sort_releases(&mut shown, sort, &watched);
    let pinned = chosen
        .as_ref()
        .and_then(|h| releases.iter().find(|r| &r.infohash == h))
        .filter(|r| filter.matches(r))
        .cloned();
    shown.retain(|r| Some(&r.infohash) != chosen.as_ref());
    let preload_for = |hash: &str| {
        preloads
            .iter()
            .find(|p| p.infohash.eq_ignore_ascii_case(hash) && p.state != PreloadState::Deleted)
            .cloned()
    };
    let is_ready_rel =
        |r: &ReleaseOption| r.preloaded || preload_for(&r.infohash).is_some_and(|p| p.state == PreloadState::Ready);
    let (preloaded_top, rest): (Vec<ReleaseOption>, Vec<ReleaseOption>) =
        shown.into_iter().partition(|r| is_ready_rel(r));
    let n_active = filter.active();
    let filter_label = if n_active > 0 {
        format!("Фильтры · {n_active}")
    } else {
        "Фильтры".to_string()
    };
    let toggle = move |p: PickerPanel| {
        if panel == Some(p) {
            on_panel.call(None)
        } else {
            on_panel.call(Some(p))
        }
    };
    let shown_count = preloaded_top.len() + rest.len() + usize::from(pinned.is_some());
    let has_preloaded_q = |q: &str| releases.iter().any(|r| is_ready_rel(r) && r.quality == q);
    let f_all = filter.clone();
    let f_dp = filter.clone();
    rsx! {
        div {
            class: "sheet-bg",
            onclick: move |_| on_close.call(()),
            div {
                class: "sheet",
                role: "dialog",
                "aria-modal": "true",
                "aria-label": "Выбор раздачи",
                tabindex: -1,
                onclick: move |e| e.stop_propagation(),
                onkeydown: move |e| {
                    if e.key() == Key::Escape {
                        e.stop_propagation();
                        on_close.call(());
                    }
                },
                div { class: "sheet-head",
                    div {
                        h3 {
                            "Выбор раздачи"
                            if mock { span { class: "mock", "примеры" } }
                        }
                        div { class: "hint", "{heading}" }
                    }
                    button {
                        class: "sheet-close",
                        title: "Закрыть",
                        "aria-label": "Закрыть",
                        onclick: move |_| on_close.call(()),
                        Icon { svg: icons::CLOSE, class: "ic sm" }
                    }
                }
                if let Some(n) = &note { div { class: "hint note", "{n}" } }
                div { class: "picker-toolbar",
                    if qualities.len() > 1 {
                        button {
                            class: if filter.quality.is_none() { "chip sm active" } else { "chip sm" },
                            onclick: move |_| on_filter.call(ReleaseFilter { quality: None, ..f_all.clone() }),
                            "Все"
                        }
                        for q in qualities.iter().take(4).cloned() {
                            {
                                let active = filter.quality.as_deref() == Some(q.as_str());
                                let f_q = filter.clone();
                                let q_val = q.clone();
                                let q_has_srv = has_preloaded_q(&q);
                                rsx! {
                                    button {
                                        key: "q-{q}",
                                        class: if active { "chip sm active" } else { "chip sm" },
                                        onclick: move |_| {
                                            let next = if active { None } else { Some(q_val.clone()) };
                                            on_filter.call(ReleaseFilter { quality: next, ..f_q.clone() });
                                        },
                                        if q_has_srv {
                                            span { class: "hot-dot", title: "Есть скачанный файл на сервере" }
                                        }
                                        "{quality_short(&q)}"
                                    }
                                }
                            }
                        }
                    }
                    if !native_player {
                        button {
                            class: if filter.direct_play == Some(true) { "chip sm accent" } else { "chip sm" },
                            onclick: move |_| {
                                let next = if f_dp.direct_play == Some(true) { None } else { Some(true) };
                                on_filter.call(ReleaseFilter { direct_play: next, ..f_dp.clone() });
                            },
                            Icon { svg: icons::BOLT, class: "ic xs" }
                            "Играет сразу"
                        }
                    }
                    button {
                        class: if panel == Some(PickerPanel::Filter) || n_active > 0 { "chip sm active" } else { "chip sm" },
                        onclick: move |_| toggle(PickerPanel::Filter),
                        Icon { svg: icons::FILTER, class: "ic xs" }
                        "{filter_label}"
                    }
                    button {
                        class: if panel == Some(PickerPanel::Sort) { "chip sm active" } else { "chip sm" },
                        onclick: move |_| toggle(PickerPanel::Sort),
                        Icon { svg: icons::SORT, class: "ic xs" }
                        "{sort.label()}"
                    }
                    span { class: "count", "{shown_count} из {total}" }
                }
                match panel {
                    Some(PickerPanel::Filter) => rsx! { FilterPanel { releases: releases.clone(), filter: filter.clone(), native_player, on_filter } },
                    Some(PickerPanel::Sort) => rsx! {
                        div { class: "filter-drawer sort-drawer",
                            for s in ReleaseSort::ALL {
                                button {
                                    class: if s == sort { "opt on" } else { "opt" },
                                    onclick: move |_| { on_sort.call(s); on_panel.call(None); },
                                    span { "{s.label()}" }
                                    if s == sort { Icon { svg: icons::CHECK, class: "ic sm" } }
                                }
                            }
                        }
                    },
                    None => rsx! {},
                }
                div { class: "rel-list",
                    if pinned.as_ref().is_some_and(is_ready_rel) {
                        if let Some(r) = &pinned {
                            {
                                let dl = preload_for(&r.infohash);
                                rsx! {
                                    ReleaseRow { key: "pinned-{r.infohash}", release: r.clone(), chosen: true, watched: true, native_player, preload: dl, on_pick, on_preload, on_cancel_preload }
                                }
                            }
                        }
                        for r in preloaded_top {
                            {
                                let dl = preload_for(&r.infohash);
                                rsx! {
                                    ReleaseRow {
                                        key: "{r.infohash}",
                                        watched: watched.contains(&r.infohash),
                                        release: r,
                                        chosen: false,
                                        native_player,
                                        preload: dl,
                                        on_pick,
                                        on_preload,
                                        on_cancel_preload,
                                    }
                                }
                            }
                        }
                    } else {
                        for r in preloaded_top {
                            {
                                let dl = preload_for(&r.infohash);
                                rsx! {
                                    ReleaseRow {
                                        key: "{r.infohash}",
                                        watched: watched.contains(&r.infohash),
                                        release: r,
                                        chosen: false,
                                        native_player,
                                        preload: dl,
                                        on_pick,
                                        on_preload,
                                        on_cancel_preload,
                                    }
                                }
                            }
                        }
                        if let Some(r) = pinned {
                            {
                                let dl = preload_for(&r.infohash);
                                rsx! {
                                    ReleaseRow { key: "pinned-{r.infohash}", release: r, chosen: true, watched: true, native_player, preload: dl, on_pick, on_preload, on_cancel_preload }
                                }
                            }
                        }
                    }
                    for r in rest {
                        {
                            let dl = preload_for(&r.infohash);
                            rsx! {
                                ReleaseRow {
                                    key: "{r.infohash}",
                                    watched: watched.contains(&r.infohash),
                                    release: r,
                                    chosen: false,
                                    native_player,
                                    preload: dl,
                                    on_pick,
                                    on_preload,
                                    on_cancel_preload,
                                }
                            }
                        }
                    }
                    if shown_count == 0 {
                        if let Some(lang) = filter.language.clone() {
                            {
                                let f_nolang = ReleaseFilter { language: None, ..filter.clone() };
                                rsx! {
                                    p { class: "muted rel-empty",
                                        "В озвучке «{lang}» подходящих раздач не найдено (всего: {total}). "
                                        button { class: "link", onclick: move |_| on_filter.call(f_nolang.clone()), "Показать на всех языках" }
                                        if n_active > 1 {
                                            " · "
                                            button { class: "link", onclick: move |_| on_filter.call(ReleaseFilter::default()), "Сбросить фильтры" }
                                        }
                                    }
                                }
                            }
                        } else {
                            p { class: "muted rel-empty",
                                "Под фильтр ничего не подходит. "
                                button { class: "link", onclick: move |_| on_filter.call(ReleaseFilter::default()), "Сбросить фильтры" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn FilterPanel(
    releases: Vec<ReleaseOption>,
    filter: ReleaseFilter,
    #[props(default)] native_player: bool,
    on_filter: EventHandler<ReleaseFilter>,
) -> Element {
    let mut qualities = distinct(&releases, |r| Some(r.quality.clone()));
    qualities.sort_by_key(|q| std::cmp::Reverse(quality_rank(q)));
    let voices = distinct(&releases, |r| Some(r.voice.clone()));
    let languages = distinct(&releases, |r| Some(r.language.clone()));
    let seasons = distinct(&releases, |r| r.season);
    let trackers = distinct(&releases, |r| Some(r.tracker.clone()));
    let years = distinct(&releases, |r| r.year);
    let codecs = distinct(&releases, |r| r.codec.clone());
    let set = move |f: ReleaseFilter| on_filter.call(f);
    let f = filter.clone();
    let flag_label = |v: Option<bool>, pos: &'static str, neg: &'static str| -> &'static str {
        if v == Some(false) {
            neg
        } else {
            pos
        }
    };
    let toggle_flag = |v: Option<bool>| -> Option<bool> {
        if v.is_some() {
            None
        } else {
            Some(true)
        }
    };
    rsx! {
        div { class: "filter-drawer",
            if filter.active() > 0 {
                div { class: "filter-reset-row",
                    button { class: "link", onclick: move |_| set(ReleaseFilter::default()), "Сбросить все фильтры" }
                }
            }
            if qualities.len() > 1 || f.quality.is_some() {
                FilterRow { name: "Качество", any: "Любое",
                    options: qualities, selected: f.quality.clone(),
                    on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { quality: v, ..f.clone() }) } }
            }
            div { class: "f-row",
                div { class: "f-lbl", "Параметры" }
                div { class: "f-opts",
                    button {
                        class: if f.hdr.is_some() { "chip sm active" } else { "chip sm" },
                        onclick: { let f = f.clone(); move |_| set(ReleaseFilter { hdr: toggle_flag(f.hdr), ..f.clone() }) },
                        "{flag_label(f.hdr, \"HDR\", \"Без HDR\")}"
                    }
                    button {
                        class: if f.dolby_vision.is_some() { "chip sm active" } else { "chip sm" },
                        onclick: { let f = f.clone(); move |_| set(ReleaseFilter { dolby_vision: toggle_flag(f.dolby_vision), ..f.clone() }) },
                        "{flag_label(f.dolby_vision, \"Dolby Vision\", \"Без DV\")}"
                    }
                    button {
                        class: if f.subtitles.is_some() { "chip sm active" } else { "chip sm" },
                        onclick: { let f = f.clone(); move |_| set(ReleaseFilter { subtitles: toggle_flag(f.subtitles), ..f.clone() }) },
                        "{flag_label(f.subtitles, \"Субтитры\", \"Без субтитров\")}"
                    }
                    if !native_player {
                        button {
                            class: if f.direct_play.is_some() { "chip sm active" } else { "chip sm" },
                            onclick: { let f = f.clone(); move |_| set(ReleaseFilter { direct_play: toggle_flag(f.direct_play), ..f.clone() }) },
                            "{flag_label(f.direct_play, \"Без перекодирования\", \"С перекодированием\")}"
                        }
                    }
                }
            }
            if voices.len() > 1 || f.voice.is_some() {
                FilterRow { name: "Перевод", any: "Любой",
                    options: voices, selected: f.voice.clone(),
                    on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { voice: v, ..f.clone() }) } }
            }
            if languages.len() > 1 || f.language.is_some() {
                FilterRow { name: "Язык", any: "Любой",
                    options: languages, selected: f.language.clone(),
                    on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { language: v, ..f.clone() }) } }
            }
            if seasons.len() > 1 || f.season.is_some() {
                FilterRow { name: "Сезон", any: "Любой",
                    options: seasons.iter().map(|s| s.to_string()).collect::<Vec<_>>(), selected: f.season.map(|s| s.to_string()),
                    on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { season: v.and_then(|s| s.parse().ok()), ..f.clone() }) } }
            }
            if codecs.len() > 1 || f.codec.is_some() {
                FilterRow { name: "Кодек", any: "Любой",
                    options: CODECS.map(String::from).to_vec(), selected: f.codec.clone(),
                    on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { codec: v, ..f.clone() }) } }
            }
            FilterRow { name: "Раздающих", any: "Любое",
                options: SEEDER_STEPS.map(|n| format!("от {n}")).to_vec(), selected: f.min_seeders.map(|n| format!("от {n}")),
                on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { min_seeders: v.and_then(|s| s.trim_start_matches("от ").parse().ok()), ..f.clone() }) } }
            FilterRow { name: "Размер", any: "Любой",
                options: SIZE_STEPS.map(|n| format!("до {n} ГБ")).to_vec(), selected: f.max_size_gb.map(|n| format!("до {n} ГБ")),
                on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { max_size_gb: v.and_then(|s| s.trim_start_matches("до ").trim_end_matches(" ГБ").parse().ok()), ..f.clone() }) } }
            if trackers.len() > 1 || f.tracker.is_some() {
                FilterRow { name: "Трекер", any: "Любой",
                    options: trackers, selected: f.tracker.clone(),
                    on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { tracker: v, ..f.clone() }) } }
            }
            if years.len() > 1 || f.year.is_some() {
                FilterRow { name: "Год", any: "Любой",
                    options: years.iter().map(|y| y.to_string()).collect::<Vec<_>>(), selected: f.year.map(|y| y.to_string()),
                    on_select: { let f = f.clone(); move |v: Option<String>| set(ReleaseFilter { year: v.and_then(|y| y.parse().ok()), ..f.clone() }) } }
            }
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

#[component]
fn ReleaseRow(
    release: ReleaseOption,
    chosen: bool,
    watched: bool,
    #[props(default)] native_player: bool,
    #[props(default)] preload: Option<PreloadItem>,
    on_pick: EventHandler<ReleaseOption>,
    on_preload: EventHandler<ReleaseOption>,
    #[props(default)] on_cancel_preload: Option<EventHandler<i64>>,
) -> Element {
    let mut confirm_cancel = use_signal(|| false);
    let low = release.seeders < LOW_SEEDS;
    let is_uhd = quality_rank(&release.quality) >= 2160;
    let is_ready = release.preloaded || preload.as_ref().is_some_and(|p| p.state == PreloadState::Ready);
    let active_dl = preload
        .as_ref()
        .filter(|p| matches!(p.state, PreloadState::Queued | PreloadState::Downloading));
    let q_sub = match (release.dolby_vision, release.hdr, release.codec.as_deref()) {
        (true, true, _) => "DV · HDR",
        (true, false, _) => "DV",
        (false, true, _) => "HDR",
        (false, false, Some(c)) => c,
        _ => "",
    };
    let (a, b) = (release.clone(), release.clone());
    let added = ru_short_date(&release.added);
    rsx! {
        div {
            class: if chosen { "rel-card picked" } else { "rel-card" },
            role: "button",
            tabindex: "0",
            onclick: move |_| on_pick.call(a.clone()),
            div { class: if is_uhd { "q-box uhd" } else { "q-box" },
                b { "{quality_short(&release.quality)}" }
                if !q_sub.is_empty() { small { "{q_sub}" } }
            }
            div { class: "rel-main",
                div { class: "rel-voice",
                    strong { "{release.voice}" }
                    if is_ready {
                        span { class: "tag srv", title: "Файл уже скачан на сервер и запускается мгновенно",
                            Icon { svg: icons::DOWNLOAD, class: "ic xs" }
                            "На сервере"
                        }
                    } else if let Some(dl) = active_dl {
                        span { class: "tag acc", title: "{dl.status_text()}", "{dl.compact_status_text()}" }
                    }
                    if chosen {
                        span { class: "tag acc", "Ваша раздача" }
                    } else if watched {
                        span { class: "tag ok", "Смотрели" }
                    }
                    if !release.picked_by.is_empty() {
                        {
                            let cnt = release.picked_by.len();
                            let tip = format!("Выбрали участники: {}", release.picked_by.join(", "));
                            rsx! {
                                span { class: "tag acc", title: "{tip}",
                                    Icon { svg: icons::USERS, class: "ic xs" }
                                    "{cnt}"
                                }
                            }
                        }
                    }
                    if release.direct_play || native_player {
                        span { class: "tag ok",
                            Icon { svg: icons::BOLT, class: "ic xs" }
                            "Играет сразу"
                        }
                    } else if release.codec.is_some() || release.hdr {
                        // Known to need the server's help; unknown codecs are not guessed.
                        span { class: "tag warn", "Перекодируется" }
                    }
                    if release.subtitles {
                        span { class: "tag", "Субтитры" }
                    }
                    span { class: "tag", "{release.tracker}" }
                }
                div { class: "rel-raw", title: "{release.title} · {added}", "{release.title}" }
            }
            div { class: "pre-slot",
                if is_ready {
                    span {
                        class: "pre-btn done",
                        title: "Файл уже скачан на сервер",
                        Icon { svg: icons::CHECK, class: "ic sm" }
                        "На сервере"
                    }
                } else if let Some(dl) = active_dl {
                    if let Some(on_cancel) = on_cancel_preload {
                        {
                            let dl_id = dl.id;
                            let status_lbl = dl.compact_status_text();
                            rsx! {
                                button {
                                    class: if confirm_cancel() { "pre-btn cancel-confirm" } else { "pre-btn active" },
                                    title: if confirm_cancel() {
                                        "Нажмите ещё раз, чтобы отменить загрузку"
                                    } else {
                                        "Нажмите, чтобы отменить загрузку"
                                    },
                                    onblur: move |_| confirm_cancel.set(false),
                                    onclick: move |e| {
                                        e.stop_propagation();
                                        if *confirm_cancel.peek() {
                                            confirm_cancel.set(false);
                                            on_cancel.call(dl_id);
                                        } else {
                                            confirm_cancel.set(true);
                                        }
                                    },
                                    if confirm_cancel() {
                                        Icon { svg: icons::CLOSE, class: "ic xs" }
                                        "Отменить?"
                                    } else {
                                        "{status_lbl}"
                                    }
                                }
                            }
                        }
                    } else {
                        span { class: "pre-btn active", "{dl.compact_status_text()}" }
                    }
                } else {
                    button {
                        class: if low { "pre-btn recommend" } else { "pre-btn" },
                        title: if low {
                            "Мало сидов — лучше скачать заранее на сервер"
                        } else {
                            "Скачать заранее на сервер"
                        },
                        onclick: move |e| {
                            e.stop_propagation();
                            on_preload.call(b.clone());
                        },
                        Icon { svg: icons::DOWNLOAD, class: "ic sm" }
                        "Заранее"
                    }
                }
            }
            div { class: "rel-metrics",
                div { class: "sz", "{human_size(release.size_bytes)}" }
                if let Some(kbps) = release.bitrate_kbps {
                    div {
                        class: "br",
                        title: if release.bitrate_approx {
                            "Приблизительно: размер раздачи, делённый на длительность. Включает звук."
                        } else {
                            "Битрейт видео из названия раздачи"
                        },
                        if release.bitrate_approx { "≈ " }
                        "{format_bitrate(kbps)}"
                    }
                }
                div {
                    class: if low { "sd low" } else { "sd ok" },
                    title: "Раздающих: {release.seeders} · добавлено {added}",
                    span { class: "sig-wrap", "aria-hidden": "true", dangerous_inner_html: seeder_signal(release.seeders) }
                    span { "{release.seeders}" }
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
            user_rating: None,
            user_status: None,
            peers: vec![],
            preloaded: false,
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

        let mut by_rate = all.clone();
        by_rate[3].bitrate_kbps = None;
        sort_releases(&mut by_rate, ReleaseSort::Bitrate, &[]);
        assert_eq!(by_rate.last().unwrap().bitrate_kbps, None, "unknown goes last");
        let known: Vec<_> = by_rate.iter().filter_map(|r| r.bitrate_kbps).collect();
        assert!(known.windows(2).all(|w| w[0] >= w[1]));

        let mut list = all.clone();
        let watched = vec!["c".repeat(40)];
        sort_releases(&mut list, ReleaseSort::Watched, &watched);
        assert_eq!(list[0].infohash, watched[0]);
        assert!(ReleaseFilter {
            season: Some(3),
            ..Default::default()
        }
        .matches(&all[9]));
        // A season filter keeps that season and packs, and drops other seasons.
        let s2 = ReleaseFilter::default().for_season(2);
        assert!(all.iter().filter(|r| r.season == Some(2)).all(|r| s2.matches(r)));
        assert!(!all.iter().filter(|r| r.season == Some(3)).any(|r| s2.matches(r)));
        let mut pack = all[0].clone();
        pack.season = None;
        assert!(s2.matches(&pack));
        // Defaults from the profile become filters; what the user already picked wins over them.
        let prefs = UserPrefs {
            language: Some("Русский".into()),
            codec: Some("H.264".into()),
            min_seeders: Some(50),
            max_size_gb: Some(30),
            hdr: Some(false),
            ..UserPrefs::default()
        };
        let d = ReleaseFilter::from_prefs(&prefs);
        assert_eq!(d.active(), 5);
        let fit: Vec<_> = all.iter().filter(|r| d.matches(r)).collect();
        assert!(!fit.is_empty());
        assert!(fit.iter().all(|r| r.language == "Русский"
            && r.codec.as_deref() == Some("H.264")
            && r.seeders >= 50
            && r.size_bytes <= 30_000_000_000
            && !r.hdr));
        let mine = ReleaseFilter {
            quality: Some("720p".into()),
            ..Default::default()
        }
        .or(ReleaseFilter::from_prefs(&UserPrefs {
            quality: Some("2160p".into()),
            language: Some("Русский".into()),
            ..UserPrefs::default()
        }));
        assert_eq!(
            (mine.quality.as_deref(), mine.language.as_deref()),
            (Some("720p"), Some("Русский"))
        );
        assert_eq!(ReleaseSort::from_key(Some("bitrate")), ReleaseSort::Bitrate);
        assert_eq!(ReleaseSort::from_key(Some("nonsense")), ReleaseSort::Seeders);
        assert!(ReleaseSort::ALL
            .iter()
            .all(|s| ReleaseSort::from_key(Some(s.key())) == *s));
        assert_eq!(human_size(28_400_000_000), "28.4 ГБ");
        assert_eq!(ru_short_date("2024-03-08"), "08.03.24");
    }

    #[test]
    fn preloaded_releases_sort_above_others() {
        let mut all = mock_releases(&title(), None);
        // Mark an item near the bottom as preloaded and confirm partitioning places it first.
        let last_idx = all.len() - 1;
        all[last_idx].preloaded = true;
        let pre_hash = all[last_idx].infohash.clone();
        let (pre, rest): (Vec<_>, Vec<_>) = all.into_iter().partition(|r| r.preloaded);
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].infohash, pre_hash);
        assert!(!rest.is_empty());
    }
}
