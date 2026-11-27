//! "Мои" (titles by status, type and sort) and "Профиль" (theme, downloads, default release settings, history).

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Utc};
use dioxus::prelude::*;
use kino_core::{
    clock, episode_code, format_episode_ranges, format_speed, image_url, normalize_quality_key, CalendarEntry,
    HistoryEntry, ImageSize, MediaKind, PreloadEpisodeItem, PreloadItem, PreloadPhase, PreloadState, TitleState,
    TitleStatus, TitleSummary, UserPrefs, UserPublicProfile, UserSocialLibrary,
};

use crate::icons::{self, Icon};
use crate::releases::{ReleaseSort, CODECS, SEEDER_STEPS, SIZE_STEPS};
use crate::{
    fallback_bg, handle_aux_click, is_plain_primary_click, plural, ru_date, title_href, AvatarContent, PosterCard,
};

/// Sub-section inside "Мои".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MySection {
    #[default]
    Lists,
    Users,
    Calendar,
}

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
        TitleStatus::WaitingEpisode | TitleStatus::WaitingRelease => "tag wait",
        TitleStatus::Watched => "tag ok",
        TitleStatus::Dropped => "tag warn",
        TitleStatus::Want => "tag",
    }
}

fn state_rating_val(s: &TitleState) -> f32 {
    s.effective_rating.or_else(|| s.rating.map(f32::from)).unwrap_or(0.0)
}

#[component]
pub fn MyView(
    items: Vec<TitleState>,
    filters: MyFilters,
    #[props(default)] section: MySection,
    #[props(default)] users: Vec<UserPublicProfile>,
    #[props(default)] selected_user_id: Option<i64>,
    #[props(default)] peer_library: Option<UserSocialLibrary>,
    #[props(default)] peer_loading: bool,
    #[props(default)] peer_error: Option<String>,
    #[props(default)] calendar: Vec<CalendarEntry>,
    #[props(default)] today_ymd: String,
    #[props(default)] on_section: Option<EventHandler<MySection>>,
    #[props(default)] on_select_user: Option<EventHandler<i64>>,
    #[props(default)] initial_peer_status: Option<TitleStatus>,
    #[props(default)] on_peer_status: Option<EventHandler<Option<TitleStatus>>>,
    #[props(default)] initial_cal_month: Option<(i32, u32)>,
    #[props(default)] on_cal_month: Option<EventHandler<Option<(i32, u32)>>>,
    #[props(default)] initial_cal_day: Option<String>,
    #[props(default)] on_cal_day: Option<EventHandler<Option<String>>>,
    #[props(default)] on_open_id: Option<EventHandler<(MediaKind, i64)>>,
    on_filters: EventHandler<MyFilters>,
    on_open: EventHandler<TitleSummary>,
) -> Element {
    let mut open_dd = use_signal(|| None::<MyDropdown>);
    let mut peer_status_filter = use_signal(|| initial_peer_status);
    let mut cal_month = use_signal(|| initial_cal_month);
    let mut cal_day = use_signal(|| initial_cal_day);
    let mut set_peer_status = move |next: Option<TitleStatus>| {
        peer_status_filter.set(next);
        if let Some(cb) = on_peer_status {
            cb.call(next);
        }
    };
    let mut set_cal_month = move |next: Option<(i32, u32)>| {
        cal_month.set(next);
        if let Some(cb) = on_cal_month {
            cb.call(next);
        }
    };
    let mut set_cal_day = move |next: Option<String>| {
        cal_day.set(next.clone());
        if let Some(cb) = on_cal_day {
            cb.call(next);
        }
    };

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
        MySort::Updated => shown.sort_by_key(|a| std::cmp::Reverse(a.updated_at)),
        MySort::MyRating => shown.sort_by(|a, b| {
            state_rating_val(b)
                .total_cmp(&state_rating_val(a))
                .then_with(|| b.updated_at.cmp(&a.updated_at))
        }),
        MySort::Rating => shown.sort_by(|a, b| {
            let ra = a.title.as_ref().and_then(|t| t.rating).unwrap_or(0.0);
            let rb = b.title.as_ref().and_then(|t| t.rating).unwrap_or(0.0);
            rb.total_cmp(&ra).then_with(|| b.updated_at.cmp(&a.updated_at))
        }),
        MySort::Title => {
            shown.sort_by_cached_key(|a| a.title.as_ref().map(|t| t.title.to_lowercase()).unwrap_or_default())
        }
    }

    rsx! {
        section { class: "section mine-section",
            if open_dd().is_some() {
                div { class: "dd-backdrop", onclick: move |_| open_dd.set(None) }
            }
            if let Some(on_sec) = on_section {
                div { class: "my-subtabs",
                    button {
                        class: if section == MySection::Lists { "pill-btn active" } else { "pill-btn outline" },
                        onclick: move |_| on_sec.call(MySection::Lists),
                        Icon { svg: icons::BOOKMARK, class: "ic xs" }
                        span { "Мои списки" }
                    }
                    button {
                        class: if section == MySection::Users { "pill-btn active" } else { "pill-btn outline" },
                        onclick: move |_| on_sec.call(MySection::Users),
                        Icon { svg: icons::USERS, class: "ic xs" }
                        span { "Участники · {users.len()}" }
                    }
                    button {
                        class: if section == MySection::Calendar { "pill-btn active" } else { "pill-btn outline" },
                        onclick: move |_| on_sec.call(MySection::Calendar),
                        Icon { svg: icons::CALENDAR, class: "ic xs" }
                        if calendar.is_empty() {
                            span { "Календарь" }
                        } else {
                            span { "Календарь · {calendar.len()}" }
                        }
                    }
                }
            }

            match section {
                MySection::Lists => rsx! {
                    div { class: "unified-bar",
                        div { class: "pill-group status-pills",
                            button {
                                class: if filters.status.is_none() { "pill-btn active" } else { "pill-btn" },
                                onclick: move |_| on_filters.call(MyFilters { status: None, ..filters }),
                                "Все · {total_kind}"
                            }
                            for st in TitleStatus::ALL {
                                {
                                    let cnt = count(st);
                                    rsx! {
                                        button {
                                            class: if filters.status == Some(st) { "pill-btn active" } else { "pill-btn" },
                                            onclick: move |_| on_filters.call(MyFilters { status: Some(st), ..filters }),
                                            "{st.label()} · {cnt}"
                                        }
                                    }
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
                                if let Some(mut t) = item.title.clone() {
                                    {
                                        let r_val = item.effective_rating.or_else(|| item.rating.map(f32::from));
                                        t.user_rating = r_val.or(t.user_rating);
                                        rsx! {
                                            div { key: "{item.kind.as_str()}{item.tmdb_id}", class: "mine",
                                                PosterCard {
                                                    item: t,
                                                    status_badge: if filters.status.is_none() { item.status } else { None },
                                                    hide_status_badge: filters.status.is_some(),
                                                    on_open,
                                                }
                                                if let Some(st) = item.status.filter(|_| filters.status.is_none()) {
                                                    div { class: "mine-meta",
                                                        span { class: "{status_tag_class(st)}", "{st.label()}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
                MySection::Users => {
                    let active_id = selected_user_id.or_else(|| users.first().map(|u| u.id));
                    rsx! {
                        if users.is_empty() {
                            p { class: "muted", "Пока нет других участников." }
                        } else {
                            div { class: "users-grid",
                                for u in users {
                                    {
                                        let uid = u.id;
                                        let is_sel = active_id == Some(uid);
                                        let init = crate::initials(&u.name);
                                        rsx! {
                                            button {
                                                key: "{uid}",
                                                class: if is_sel { "user-card active" } else { "user-card" },
                                                onclick: move |_| {
                                                    set_peer_status(None);
                                                    if let Some(cb) = on_select_user {
                                                        cb.call(uid);
                                                    }
                                                },
                                                div { class: "user-card-head",
                                                    div { class: "user-av",
                                                        AvatarContent {
                                                            initial: init,
                                                            url: u.avatar_url.clone(),
                                                            alt: u.name.clone(),
                                                        }
                                                    }
                                                    div { class: "user-card-info",
                                                        div { class: "user-card-name", "{u.name}" }
                                                        div { class: "user-card-counts",
                                                            "Смотрит: {u.watching_count} · Посмотрел: {u.watched_count} · Хочет: {u.want_count}"
                                                        }
                                                    }
                                                }
                                                if let Some(wn) = &u.watching_now {
                                                    if let Some(wt) = &wn.title {
                                                        div { class: "user-now-pill",
                                                            span { class: "live-dot" }
                                                            span {
                                                                if wn.kind == MediaKind::Tv && wn.season > 0 {
                                                                    "Сейчас: {wt.title} ({episode_code(wn.season, wn.episode)})"
                                                                } else {
                                                                    "Сейчас: {wt.title}"
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
                            if peer_loading {
                                p { class: "muted", "Загружаем коллекцию участника…" }
                            } else if let Some(err) = peer_error {
                                p { class: "err-text", "{err}" }
                            } else if let Some(lib) = peer_library {
                                {
                                    let pfilter = peer_status_filter();
                                    let filtered: Vec<TitleState> = lib
                                        .items
                                        .iter()
                                        .filter(|it| pfilter.is_none() || it.status == pfilter)
                                        .cloned()
                                        .collect();
                                    rsx! {
                                        div { class: "peer-lib-head",
                                            h3 { "Коллекция: {lib.profile.name}" }
                                            if let Some(wn) = lib.profile.watching_now.as_ref().and_then(|w| w.title.clone().map(|t| (w.clone(), t))) {
                                                {
                                                    let (w_meta, w_title) = wn;
                                                    let href = title_href(w_meta.kind, w_title.tmdb_id);
                                                    let wt_open = w_title.clone();
                                                    rsx! {
                                                        a {
                                                            class: "pill-btn accent",
                                                            href: "{href}",
                                                            onclick: move |e: Event<MouseData>| {
                                                                if is_plain_primary_click(&e) {
                                                                    e.prevent_default();
                                                                    on_open.call(wt_open.clone());
                                                                }
                                                            },
                                                            onauxclick: handle_aux_click,
                                                            Icon { svg: icons::PLAY, class: "ic xs" }
                                                            if w_meta.kind == MediaKind::Tv && w_meta.season > 0 {
                                                                "Смотрит: {w_title.title} · {episode_code(w_meta.season, w_meta.episode)}"
                                                            } else {
                                                                "Смотрит: {w_title.title}"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        div { class: "pill-group status-pills",
                                            button {
                                                class: if pfilter.is_none() { "pill-btn active" } else { "pill-btn" },
                                                onclick: move |_| set_peer_status(None),
                                                "Все · {lib.items.len()}"
                                            }
                                            for st in TitleStatus::ALL {
                                                {
                                                    let cnt = lib.items.iter().filter(|i| i.status == Some(st)).count();
                                                    if cnt > 0 {
                                                        rsx! {
                                                            button {
                                                                class: if pfilter == Some(st) { "pill-btn active" } else { "pill-btn" },
                                                                onclick: move |_| set_peer_status(Some(st)),
                                                                "{st.label()} · {cnt}"
                                                            }
                                                        }
                                                    } else {
                                                        rsx! {}
                                                    }
                                                }
                                            }
                                        }
                                        if filtered.is_empty() {
                                            p { class: "muted", "В этом списке пока пусто." }
                                        } else {
                                            div { class: "grid",
                                                for item in filtered {
                                                    if let Some(mut t) = item.title.clone() {
                                                        {
                                                            let r_val = item.effective_rating.or_else(|| item.rating.map(f32::from));
                                                            t.user_rating = r_val;
                                                            let peer_tip = format!("Оценка: {}", lib.profile.name);
                                                            rsx! {
                                                                div { key: "peer-{item.kind.as_str()}{item.tmdb_id}", class: "mine",
                                                                    PosterCard {
                                                                        item: t,
                                                                        status_badge: item.status,
                                                                        hide_status_badge: true,
                                                                        rating_tooltip: Some(peer_tip),
                                                                        on_open,
                                                                    }
                                                                    if item.status.is_some() {
                                                                        div { class: "mine-meta",
                                                                            if let Some(st) = item.status {
                                                                                span { class: "{status_tag_class(st)}", "{st.label()}" }
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
                                }
                            }
                        }
                    }
                },
                MySection::Calendar => {
                    let default_ym = calendar
                        .first()
                        .and_then(|e| parse_ymd_ym(&e.date))
                        .or_else(|| parse_ymd_ym(&today_ymd))
                        .unwrap_or((2026, 11));
                    let (yr, mo) = cal_month().unwrap_or(default_ym);
                    let month_prefix = format!("{yr:04}-{mo:02}-");
                    let lead_empty = weekday_mon0(yr, mo, 1);
                    let dim = days_in_month(yr, mo);
                    let sel_day = cal_day();
                    let month_entries: Vec<CalendarEntry> = calendar
                        .iter()
                        .filter(|e| e.date.starts_with(&month_prefix))
                        .cloned()
                        .collect();
                    let shown_entries: Vec<CalendarEntry> = if let Some(d) = sel_day.as_deref() {
                        calendar.iter().filter(|e| e.date == d).cloned().collect()
                    } else if !month_entries.is_empty() {
                        month_entries.clone()
                    } else {
                        calendar.clone()
                    };
                    rsx! {
                        if calendar.is_empty() {
                            div { class: "panel muted-box",
                                "Пока нет запланированных премьер или новых серий для ваших отслеживаемых фильмов и сериалов."
                            }
                        } else {
                            div { class: "cal-wrap",
                                div { class: "cal-head",
                                    button {
                                        class: "round-act",
                                        title: "Предыдущий месяц",
                                        "aria-label": "Предыдущий месяц",
                                        onclick: move |_| {
                                            set_cal_day(None);
                                            set_cal_month(Some(shift_ym((yr, mo), -1)));
                                        },
                                        Icon { svg: icons::CHEV_L, class: "ic sm" }
                                    }
                                    div { class: "cal-month-title", "{month_name_ru(mo)} {yr}" }
                                    button {
                                        class: "round-act",
                                        title: "Следующий месяц",
                                        "aria-label": "Следующий месяц",
                                        onclick: move |_| {
                                            set_cal_day(None);
                                            set_cal_month(Some(shift_ym((yr, mo), 1)));
                                        },
                                        Icon { svg: icons::CHEV_R, class: "ic sm" }
                                    }
                                    if sel_day.is_some() {
                                        button {
                                            class: "chip sm",
                                            onclick: move |_| set_cal_day(None),
                                            "Весь месяц"
                                        }
                                    }
                                }
                                div { class: "cal-grid",
                                    for wd in ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"] {
                                        div { key: "wd-{wd}", class: "cal-wd", "{wd}" }
                                    }
                                    for idx in 0..lead_empty {
                                        div { key: "empty-{idx}", class: "cal-cell empty" }
                                    }
                                    for day in 1..=dim {
                                        {
                                            let ymd = format!("{yr:04}-{mo:02}-{day:02}");
                                            let day_items: Vec<&CalendarEntry> =
                                                month_entries.iter().filter(|e| e.date == ymd).collect();
                                            let has_events = !day_items.is_empty();
                                            let is_today = ymd == today_ymd;
                                            let is_sel = sel_day.as_deref() == Some(ymd.as_str());
                                            let cls = match (has_events, is_today, is_sel) {
                                                (_, _, true) => "cal-cell active selected",
                                                (true, true, false) => "cal-cell active today",
                                                (true, false, false) => "cal-cell active",
                                                (false, true, false) => "cal-cell today",
                                                _ => "cal-cell",
                                            };
                                            let titles_preview: String = day_items
                                                .iter()
                                                .filter_map(|e| e.title.as_ref().map(|t| t.title.clone()))
                                                .take(2)
                                                .collect::<Vec<_>>()
                                                .join(", ");
                                            let ev_cnt = day_items.len();
                                            rsx! {
                                                button {
                                                    key: "{ymd}",
                                                    class: "{cls}",
                                                    disabled: !has_events,
                                                    title: if titles_preview.is_empty() { String::new() } else { titles_preview.clone() },
                                                    onclick: move |_| {
                                                        if is_sel {
                                                            set_cal_day(None);
                                                        } else {
                                                            set_cal_day(Some(ymd.clone()));
                                                        }
                                                    },
                                                    span { class: "cal-day-num", "{day}" }
                                                    if has_events {
                                                        span { class: "cal-day-dot",
                                                            if ev_cnt > 1 { "{ev_cnt}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            div { class: "cal-list",
                                if sel_day.is_some() {
                                    div { class: "cal-sub-head",
                                        "События на {ru_date(sel_day.as_deref().unwrap_or(\"\"))}"
                                    }
                                } else if month_entries.is_empty() {
                                    div { class: "cal-sub-head",
                                        "В этом месяце выходов нет — ближайшие даты:"
                                    }
                                }
                                for entry in shown_entries {
                                    {
                                        let t_opt = entry.title.clone();
                                        let title_txt = entry
                                            .title
                                            .as_ref()
                                            .map(|t| t.title.clone())
                                            .unwrap_or_else(|| format!("#{}", entry.tmdb_id));
                                        let poster = entry.title.as_ref().and_then(|t| t.poster_path.clone());
                                        let date_lbl = ru_date(&entry.date);
                                        let ep_lbl = match (entry.season, entry.episode) {
                                            (Some(s), Some(e)) => {
                                                let code = episode_code(s, e);
                                                match &entry.episode_name {
                                                    Some(n) if !n.is_empty() => format!("{code} · {n}"),
                                                    _ => code,
                                                }
                                            }
                                            _ => "Премьера фильма".to_owned(),
                                        };
                                        let st = entry.status;
                                        let kind = entry.kind;
                                        let id = entry.tmdb_id;
                                        let href = title_href(kind, id);
                                        rsx! {
                                            a {
                                                key: "cal-{entry.date}-{kind.as_str()}-{id}",
                                                class: "cal-row",
                                                href: "{href}",
                                                onclick: move |e: Event<MouseData>| {
                                                    if is_plain_primary_click(&e) {
                                                        e.prevent_default();
                                                        if let Some(t) = t_opt.clone() {
                                                            on_open.call(t);
                                                        } else if let Some(cb) = on_open_id {
                                                            cb.call((kind, id));
                                                        }
                                                    }
                                                },
                                                onauxclick: handle_aux_click,
                                                div { class: "cal-date", "{date_lbl}" }
                                                div { class: "cal-poster",
                                                    if let Some(p) = poster {
                                                        img { src: image_url(&p, ImageSize::Poster), alt: "{title_txt}", loading: "lazy" }
                                                    } else {
                                                        div { class: "hist-thumb-ph", style: "background: {fallback_bg(id)}" }
                                                    }
                                                }
                                                div { class: "cal-body",
                                                    div { class: "cal-title", "{title_txt}" }
                                                    div { class: "cal-sub", "{ep_lbl}" }
                                                }
                                                span { class: "{status_tag_class(st)}", "{st.label()}" }
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
}

fn parse_ymd_ym(ymd: &str) -> Option<(i32, u32)> {
    let mut parts = ymd.split('-');
    let y = parts.next()?.parse::<i32>().ok()?;
    let m = parts.next()?.parse::<u32>().ok()?.clamp(1, 12);
    Some((y, m))
}

fn shift_ym((y, m): (i32, u32), delta: i32) -> (i32, u32) {
    let total = y * 12 + (m as i32 - 1) + delta;
    let ny = total.div_euclid(12);
    let nm = (total.rem_euclid(12) + 1) as u32;
    (ny, nm)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = shift_ym((y, m), 1);
    let start = NaiveDate::from_ymd_opt(y, m, 1);
    let end = NaiveDate::from_ymd_opt(ny, nm, 1);
    match (start, end) {
        (Some(a), Some(b)) => (b - a).num_days().max(28) as u32,
        _ => 30,
    }
}

fn weekday_mon0(y: i32, m: u32, d: u32) -> u32 {
    NaiveDate::from_ymd_opt(y, m, d)
        .map(|dt| dt.weekday().num_days_from_monday())
        .unwrap_or(0)
}

fn month_name_ru(m: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "Январь",
        "Февраль",
        "Март",
        "Апрель",
        "Май",
        "Июнь",
        "Июль",
        "Август",
        "Сентябрь",
        "Октябрь",
        "Ноябрь",
        "Декабрь",
    ];
    NAMES.get((m.saturating_sub(1)) as usize).copied().unwrap_or("")
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

fn device_quality_label(raw: Option<&str>) -> Option<String> {
    match raw.map(str::trim).filter(|s| !s.is_empty())? {
        "ask" => Some("Спрашивать".to_owned()),
        other => Some(normalize_quality_key(other).to_owned()),
    }
}

fn device_quality_value(lbl: Option<&str>) -> Option<String> {
    match lbl?.trim() {
        "Спрашивать" => Some("ask".to_owned()),
        other if !other.is_empty() => Some(normalize_quality_key(other).to_owned()),
        _ => None,
    }
}

fn prefs_summary(p: &UserPrefs) -> Vec<(String, bool)> {
    let mut tags = Vec::new();
    if let Some(dq) = device_quality_label(p.desktop_quality.as_deref()) {
        tags.push((format!("ПК: {dq}"), true));
    }
    if let Some(mq) = device_quality_label(p.mobile_quality.as_deref()) {
        tags.push((format!("Телефон: {mq}"), true));
    }
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
    } else if p.direct_play == Some(false) {
        tags.push(("С перекодированием".into(), false));
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

/// Update state displayed inside the native app profile section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppUpdateUi {
    UpToDate,
    Checking,
    Available(String),
    Downloading(u8),
    Installing,
    Error { can_retry_install: bool, message: String },
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
    #[props(default)] app_version: Option<String>,
    #[props(default)] update_state: Option<AppUpdateUi>,
    #[props(default)] on_check_update: Option<EventHandler<()>>,
    #[props(default)] on_install_update: Option<EventHandler<()>>,
    /// Current time and the viewer's UTC offset in minutes, for dates.
    now: DateTime<Utc>,
    utc_offset_min: i32,
    on_theme: EventHandler<Theme>,
    on_prefs: EventHandler<UserPrefs>,
    on_delete_preload: EventHandler<i64>,
    #[props(default)] on_delete_preload_episode: Option<EventHandler<(i64, i32, i32)>>,
    #[props(default)] dl_error: Option<String>,
    #[props(default)] on_refresh: Option<EventHandler<()>>,
    #[props(default)] initial_prefs_open: bool,
    #[props(default)] on_prefs_open: Option<EventHandler<bool>>,
    #[props(default)] initial_hist_limit: Option<usize>,
    #[props(default)] on_hist_limit: Option<EventHandler<usize>>,
    #[props(default)] initial_expanded_series: Vec<i64>,
    #[props(default)] on_toggle_series: Option<EventHandler<i64>>,
    on_open: EventHandler<(MediaKind, i64)>,
    on_logout: EventHandler<()>,
) -> Element {
    let mut prefs_open = use_signal(|| initial_prefs_open);
    let mut hist_limit = use_signal(|| initial_hist_limit.unwrap_or(10));
    let mut expanded_series = use_signal(|| initial_expanded_series);
    let mut toggle_prefs = move || {
        let next = !*prefs_open.peek();
        prefs_open.set(next);
        if let Some(cb) = on_prefs_open {
            cb.call(next);
        }
    };
    let toggle_series = Callback::new(move |tmdb_id: i64| {
        expanded_series.with_mut(|list| {
            if let Some(pos) = list.iter().position(|&x| x == tmdb_id) {
                list.remove(pos);
            } else {
                list.push(tmdb_id);
            }
        });
        if let Some(cb) = on_toggle_series {
            cb.call(tmdb_id);
        }
    });
    let mut bump_hist_limit = move || {
        let next = *hist_limit.peek() + 10;
        hist_limit.set(next);
        if let Some(cb) = on_hist_limit {
            cb.call(next);
        }
    };
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

            if let Some(ver) = app_version {
                section { class: "section",
                    div { class: "sec-head",
                        h2 { "Приложение" }
                    }
                    div { class: "panel theme-panel",
                        div {
                            div { class: "theme-lbl", "Версия {ver}" }
                            div { class: "muted theme-sub",
                                match update_state.as_ref() {
                                    Some(AppUpdateUi::Checking) => "Проверяем наличие обновлений…".to_owned(),
                                    Some(AppUpdateUi::UpToDate) => "Установлена последняя версия".to_owned(),
                                    Some(AppUpdateUi::Available(new_v)) => format!("Доступна новая версия {new_v}"),
                                    Some(AppUpdateUi::Downloading(pct)) => format!("Скачиваем обновление… {pct}%"),
                                    Some(AppUpdateUi::Installing) => "Запускаем установку обновления…".to_owned(),
                                    Some(AppUpdateUi::Error { message, .. }) => message.clone(),
                                    None => "Обновления проверяются автоматически".to_owned(),
                                }
                            }
                        }
                        div { class: "update-actions",
                            match update_state.as_ref() {
                                Some(AppUpdateUi::Available(new_v)) => rsx! {
                                    if let Some(cb) = on_install_update {
                                        button {
                                            class: "btn primary sm",
                                            onclick: move |_| cb.call(()),
                                            Icon { svg: icons::DOWNLOAD, class: "ic xs" }
                                            "Обновить до {new_v}"
                                        }
                                    }
                                },
                                Some(AppUpdateUi::Downloading(_)) | Some(AppUpdateUi::Installing) | Some(AppUpdateUi::Checking) => rsx! {
                                    button { class: "btn glass sm", disabled: true, "Подождите…" }
                                },
                                Some(AppUpdateUi::Error { can_retry_install, .. }) => rsx! {
                                    if *can_retry_install {
                                        if let Some(cb) = on_install_update {
                                            button {
                                                class: "btn primary sm",
                                                onclick: move |_| cb.call(()),
                                                Icon { svg: icons::DOWNLOAD, class: "ic xs" }
                                                "Повторить обновление"
                                            }
                                        }
                                    } else if let Some(cb) = on_check_update {
                                        button {
                                            class: "btn glass sm",
                                            onclick: move |_| cb.call(()),
                                            Icon { svg: icons::REFRESH, class: "ic xs" }
                                            "Проверить снова"
                                        }
                                    }
                                },
                                _ => rsx! {
                                    if let Some(cb) = on_check_update {
                                        button {
                                            class: "btn glass sm",
                                            onclick: move |_| cb.call(()),
                                            Icon { svg: icons::REFRESH, class: "ic xs" }
                                            "Проверить обновления"
                                        }
                                    }
                                },
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
                if let Some(err) = dl_error {
                    p { class: "err-text", "{err}" }
                }
                if preloads.is_empty() {
                    div { class: "panel muted-box",
                        "Нет активных загрузок. Для редких раздач с малым числом сидов нажмите «Заранее» в списке раздач или значок загрузки напротив нужной серии."
                    }
                } else {
                    {
                        let groups = group_profile_downloads(preloads);
                        let expanded_ids = expanded_series();
                        rsx! {
                            for entry in groups {
                                match entry {
                                    DownloadGroup::Movie(item) => rsx! {
                                        PreloadRow {
                                            key: "m-{item.id}",
                                            item: *item,
                                            now,
                                            on_open,
                                            on_delete: on_delete_preload,
                                        }
                                    },
                                    DownloadGroup::Series { tmdb_id, items } => {
                                        let is_open = expanded_ids.contains(&tmdb_id);
                                        rsx! {
                                            SeriesPreloadCard {
                                                key: "s-{tmdb_id}",
                                                tmdb_id,
                                                items,
                                                now,
                                                expanded: is_open,
                                                on_toggle: toggle_series,
                                                on_open,
                                                on_delete: on_delete_preload,
                                                on_delete_episode: on_delete_preload_episode,
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            section { class: "section",
                div { class: "sec-head",
                    h2 { "Раздачи и плеер по умолчанию" }
                }
                div { class: if prefs_open() { "acc-card open" } else { "acc-card" },
                    onclick: move |_| toggle_prefs(),
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
                            toggle_prefs();
                        },
                        if prefs_open() { "Свернуть" } else { "Настроить" }
                        Icon { svg: icons::CHEV_D, class: chev_cls }
                    }
                }
                if prefs_open() {
                    div { class: "panel acc-body",
                        p { class: "muted acc-note", "Эти фильтры сразу применяются при открытии списка раздач. Выбранный язык озвучки сохраняется всегда; если по остальным ограничениям ничего не нашлось, ослабляются только они." }
                        PrefRow {
                            name: "Качество на ПК",
                            any: "Авто (1080p)",
                            options: ["2160p", "1080p", "720p", "SD", "Спрашивать"].map(String::from).to_vec(),
                            selected: device_quality_label(p.desktop_quality.as_deref()),
                            on_select: {
                                let p = p.clone();
                                move |v: Option<String>| {
                                    on_prefs.call(UserPrefs {
                                        desktop_quality: device_quality_value(v.as_deref()),
                                        ..p.clone()
                                    })
                                }
                            },
                        }
                        PrefRow {
                            name: "Качество на телефоне",
                            any: "Авто (720p / SD)",
                            options: ["720p", "SD", "1080p", "2160p", "Спрашивать"].map(String::from).to_vec(),
                            selected: device_quality_label(p.mobile_quality.as_deref()),
                            on_select: {
                                let p = p.clone();
                                move |v: Option<String>| {
                                    on_prefs.call(UserPrefs {
                                        mobile_quality: device_quality_value(v.as_deref()),
                                        ..p.clone()
                                    })
                                }
                            },
                        }
                        PrefRow { name: "Фильтр качества", any: "Любое", options: QUALITIES.map(String::from).to_vec(), selected: p.quality.clone(),
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
                                let href = title_href(kind, id);
                                rsx! {
                                    a {
                                        key: "{i}-{id}",
                                        class: "hist-row",
                                        href: "{href}",
                                        onclick: move |e: Event<MouseData>| {
                                            if is_plain_primary_click(&e) {
                                                e.prevent_default();
                                                on_open.call((kind, id));
                                            }
                                        },
                                        onauxclick: handle_aux_click,
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
                                onclick: move |_| bump_hist_limit(),
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

fn retention_tag(
    state: PreloadState,
    delete_after: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<(String, &'static str)> {
    match (state, delete_after) {
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
    }
}

enum DownloadGroup {
    Movie(Box<PreloadItem>),
    Series { tmdb_id: i64, items: Vec<PreloadItem> },
}

fn group_profile_downloads(preloads: Vec<PreloadItem>) -> Vec<DownloadGroup> {
    let mut out: Vec<DownloadGroup> = Vec::new();
    let mut tv_idx: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for item in preloads {
        if item.kind == MediaKind::Tv {
            if let Some(&pos) = tv_idx.get(&item.tmdb_id) {
                if let DownloadGroup::Series { ref mut items, .. } = out[pos] {
                    items.push(item);
                }
            } else {
                tv_idx.insert(item.tmdb_id, out.len());
                out.push(DownloadGroup::Series {
                    tmdb_id: item.tmdb_id,
                    items: vec![item],
                });
            }
        } else {
            out.push(DownloadGroup::Movie(Box::new(item)));
        }
    }
    out
}

#[component]
fn PreloadRow(
    item: PreloadItem,
    now: DateTime<Utc>,
    on_open: EventHandler<(MediaKind, i64)>,
    on_delete: EventHandler<i64>,
) -> Element {
    let mut confirm_del = use_signal(|| false);
    let pct = (item.progress * 100.0).clamp(0.0, 100.0).round();
    let status_text = item.status_text();
    let delete_tag = retention_tag(item.state, item.delete_after, now);
    let size = item.size_bytes.map(|b| crate::human_size(b.max(0) as u64));
    let id = item.id;
    let (kind, tmdb_id) = (item.kind, item.tmdb_id);
    let cls = match item.state {
        PreloadState::Ready => "tag ok",
        PreloadState::Queued | PreloadState::Downloading => "tag acc",
        PreloadState::Failed => "tag err",
        _ => "tag",
    };
    let badge_lbl = if item.kind == MediaKind::Tv && item.season > 0 {
        format!("S{:02}", item.season)
    } else if item.kind == MediaKind::Tv {
        "TV".to_owned()
    } else {
        item.quality_short().unwrap_or("HD").to_owned()
    };
    let primary_title = match (item.media_title.as_deref(), item.kind, item.season) {
        (Some(name), MediaKind::Tv, s) if s > 0 => format!("{name} · Сезон {s}"),
        (Some(name), _, _) => name.to_owned(),
        (None, _, _) => item.title.clone().unwrap_or_else(|| item.infohash.clone()),
    };
    let release_sub = item
        .media_title
        .as_deref()
        .and_then(|name| item.title.as_deref().filter(|raw| *raw != name))
        .map(str::to_owned);
    let q_tag = item.quality_short();
    let voice_tag = item.voice.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let href = title_href(kind, tmdb_id);
    let in_progress = matches!(item.state, PreloadState::Queued | PreloadState::Downloading);
    let wait_bar = in_progress && item.progress <= 0.001;
    let confirm = confirm_del();
    rsx! {
        div { class: "dl-row",
            a {
                class: "dl-main",
                href: "{href}",
                onclick: move |e: Event<MouseData>| {
                    if is_plain_primary_click(&e) {
                        e.prevent_default();
                        on_open.call((kind, tmdb_id));
                    }
                },
                onauxclick: handle_aux_click,
                div { class: "dl-poster", style: "background: {fallback_bg(tmdb_id)}",
                    if let Some(p) = &item.poster_path {
                        img { src: image_url(p, ImageSize::Poster), alt: "{primary_title}", loading: "lazy" }
                        span { class: "dl-poster-tag over", "{badge_lbl}" }
                    } else {
                        span { class: "dl-poster-tag", "{badge_lbl}" }
                    }
                }
                div { class: "dl-body",
                    div { class: "dl-title", "{primary_title}" }
                    if let Some(sub) = release_sub {
                        div { class: "dl-sub", title: "{sub}", "{sub}" }
                    }
                    div { class: "dl-tags",
                        span { class: "{cls}", "{status_text}" }
                        if let Some(q) = q_tag { span { class: "tag", "{q}" } }
                        if let Some(v) = voice_tag { span { class: "tag", "{v}" } }
                        if let Some(s) = size { span { class: "tag", "{s}" } }
                        if let Some(who) = item.requested_by.as_deref() {
                            span { class: "tag", title: "Добавил: {who}",
                                Icon { svg: icons::USERS, class: "ic xs" }
                                "{who}"
                            }
                        }
                        if let Some((txt, tcls)) = delete_tag {
                            span { class: "{tcls}",
                                Icon { svg: icons::CLOCK, class: "ic xs" }
                                "{txt}"
                            }
                        }
                    }
                    if in_progress {
                        div { class: if wait_bar { "dl-bar wait" } else { "dl-bar" },
                            div { style: "width: {pct:.0}%" }
                        }
                    }
                }
            }
            if item.can_delete {
                button {
                    class: if confirm { "round-act danger-confirm" } else { "round-act danger" },
                    title: if confirm { "Нажмите ещё раз, чтобы удалить загрузку" } else { "Удалить загрузку" },
                    "aria-label": if confirm { "Подтвердить удаление" } else { "Удалить загрузку" },
                    onblur: move |_| confirm_del.set(false),
                    onclick: move |e| {
                        e.stop_propagation();
                        if *confirm_del.peek() {
                            confirm_del.set(false);
                            on_delete.call(id);
                        } else {
                            confirm_del.set(true);
                        }
                    },
                    Icon { svg: icons::TRASH, class: "ic sm" }
                }
            }
        }
    }
}

#[derive(Clone, PartialEq)]
struct DrawerEpisode {
    preload_id: i64,
    can_delete: bool,
    dl_phase: PreloadPhase,
    speed_bps: u64,
    quality: Option<String>,
    voice: Option<String>,
    ep: PreloadEpisodeItem,
}

#[component]
fn SeriesPreloadCard(
    tmdb_id: i64,
    items: Vec<PreloadItem>,
    now: DateTime<Utc>,
    expanded: bool,
    on_toggle: EventHandler<i64>,
    on_open: EventHandler<(MediaKind, i64)>,
    on_delete: EventHandler<i64>,
    #[props(default)] on_delete_episode: Option<EventHandler<(i64, i32, i32)>>,
) -> Element {
    let mut confirm_del = use_signal(|| false);
    let primary_title = items
        .iter()
        .find_map(|p| p.media_title.as_deref())
        .map(str::to_owned)
        .or_else(|| items.iter().find_map(|p| p.title.clone()))
        .unwrap_or_else(|| format!("Сериал #{tmdb_id}"));
    let poster_path = items.iter().find_map(|p| p.poster_path.clone());
    let release_sub = if items.len() == 1 {
        items[0]
            .media_title
            .as_deref()
            .and_then(|name| items[0].title.as_deref().filter(|raw| *raw != name))
            .map(str::to_owned)
    } else {
        Some(format!(
            "{} {}",
            items.len(),
            plural(items.len(), "раздача", "раздачи", "раздач")
        ))
    };

    let mut seasons_map: BTreeMap<i32, Vec<DrawerEpisode>> = BTreeMap::new();
    let mut no_ep_items: Vec<PreloadItem> = Vec::new();
    let mut ready_ep_keys: Vec<(i32, i32)> = Vec::new();
    let mut all_ep_keys: Vec<(i32, i32)> = Vec::new();

    for item in &items {
        let non_deleted: Vec<PreloadEpisodeItem> = item
            .episodes
            .iter()
            .filter(|e| e.state != PreloadState::Deleted)
            .cloned()
            .collect();
        if non_deleted.is_empty() {
            no_ep_items.push(item.clone());
            continue;
        }
        let q = item.quality_short().map(str::to_owned);
        let v = item
            .voice
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        for ep in non_deleted {
            let key = (ep.season, ep.episode);
            if !all_ep_keys.contains(&key) {
                all_ep_keys.push(key);
            }
            if ep.state == PreloadState::Ready && !ready_ep_keys.contains(&key) {
                ready_ep_keys.push(key);
            }
            seasons_map.entry(ep.season).or_default().push(DrawerEpisode {
                preload_id: item.id,
                can_delete: item.can_delete,
                dl_phase: item.dl_phase,
                speed_bps: item.speed_bps,
                quality: q.clone(),
                voice: v.clone(),
                ep,
            });
        }
    }
    for rows in seasons_map.values_mut() {
        rows.sort_by_key(|r| (r.ep.episode, r.preload_id));
    }

    let badge_lbl = if seasons_map.len() == 1 {
        seasons_map
            .keys()
            .next()
            .filter(|&&s| s > 0)
            .map(|s| format!("S{s:02}"))
            .unwrap_or_else(|| "TV".to_owned())
    } else if items.len() == 1 && items[0].season > 0 {
        format!("S{:02}", items[0].season)
    } else {
        "TV".to_owned()
    };

    let ep_summary_str = format_episode_ranges(&all_ep_keys);
    let ep_summary = (!ep_summary_str.is_empty()).then_some(ep_summary_str);

    let any_active = items
        .iter()
        .any(|p| matches!(p.state, PreloadState::Queued | PreloadState::Downloading));
    let any_ready = items.iter().any(|p| p.state == PreloadState::Ready) || !ready_ep_keys.is_empty();
    let total_speed: u64 = items
        .iter()
        .filter(|p| matches!(p.state, PreloadState::Queued | PreloadState::Downloading))
        .map(|p| p.speed_bps)
        .sum();
    let agg_progress: f32 = {
        let total_w: f64 = items.iter().map(|p| p.size_bytes.unwrap_or(1).max(1) as f64).sum();
        let done_w: f64 = items
            .iter()
            .map(|p| {
                let w = p.size_bytes.unwrap_or(1).max(1) as f64;
                let pr = if p.state == PreloadState::Ready {
                    1.0
                } else {
                    f64::from(p.progress.clamp(0.0, 1.0))
                };
                w * pr
            })
            .sum();
        if total_w > 0.0 {
            (done_w / total_w) as f32
        } else {
            0.0
        }
    };
    let (status_cls, status_text) = if any_active {
        let pct = (agg_progress * 100.0).clamp(0.0, 100.0).round() as u32;
        let lbl = if total_speed > 0 && agg_progress > 0.0 {
            format!("Качается {pct}% · {}", format_speed(total_speed))
        } else if agg_progress > 0.0 {
            format!("Качается {pct}%")
        } else {
            "В очереди".to_owned()
        };
        ("tag acc", lbl)
    } else if any_ready {
        let lbl = if !ready_ep_keys.is_empty() {
            let n = ready_ep_keys.len();
            format!("На сервере · {n} {}", plural(n, "серия", "серии", "серий"))
        } else {
            "Готово".to_owned()
        };
        ("tag ok", lbl)
    } else {
        ("tag err", "Ошибка".to_owned())
    };

    let mut qualities: Vec<String> = Vec::new();
    let mut voices: Vec<String> = Vec::new();
    let mut requesters: Vec<String> = Vec::new();
    let mut known_bytes: i64 = 0;
    for p in &items {
        if let Some(q) = p.quality_short() {
            let qs = q.to_owned();
            if !qualities.contains(&qs) {
                qualities.push(qs);
            }
        }
        if let Some(v) = p.voice.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let vs = v.to_owned();
            if !voices.contains(&vs) {
                voices.push(vs);
            }
        }
        if let Some(who) = p.requested_by.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            for part in who.split(", ").map(str::trim).filter(|s| !s.is_empty()) {
                let ps = part.to_owned();
                if !requesters.contains(&ps) {
                    requesters.push(ps);
                }
            }
        }
        if let Some(b) = p.size_bytes.filter(|&b| b > 0) {
            known_bytes += b;
        }
    }
    let delete_tag = if !any_active && any_ready {
        let min_del = items.iter().filter_map(|p| p.delete_after).min();
        retention_tag(PreloadState::Ready, min_del, now)
    } else {
        None
    };

    let href = title_href(MediaKind::Tv, tmdb_id);
    let pct = (agg_progress * 100.0).clamp(0.0, 100.0).round();
    let wait_bar = any_active && agg_progress <= 0.001;
    let total_eps = all_ep_keys.len();
    let toggle_lbl = if total_eps > 0 {
        format!("Серии · {total_eps}")
    } else {
        "Серии".to_owned()
    };
    let single_deletable_id = (items.len() == 1 && items[0].can_delete).then(|| items[0].id);
    let show_ep_release = items.len() > 1;
    let confirm = confirm_del();
    let card_cls = if expanded {
        "dl-series-card open"
    } else {
        "dl-series-card"
    };
    let bar_cls = if wait_bar { "dl-bar wait" } else { "dl-bar" };
    let toggle_cls = if expanded {
        "chip sm active dl-ep-toggle"
    } else {
        "chip sm dl-ep-toggle"
    };
    let toggle_title = if expanded {
        "Скрыть серии"
    } else {
        "Показать скачанные серии"
    };
    let toggle_chev_cls = if expanded { "ic xs rot" } else { "ic xs" };
    let del_cls = if confirm {
        "round-act danger-confirm"
    } else {
        "round-act danger"
    };
    let del_title = if confirm {
        "Нажмите ещё раз, чтобы удалить все серии этого релиза"
    } else {
        "Удалить загрузку"
    };
    let del_aria = if confirm {
        "Подтвердить удаление"
    } else {
        "Удалить загрузку"
    };
    let req_label = (!requesters.is_empty()).then(|| requesters.join(", "));
    let can_delete_season = items.len() > 1;

    rsx! {
        div { class: "{card_cls}",
            div { class: "dl-series-head",
                a {
                    class: "dl-main",
                    href: "{href}",
                    onclick: move |e: Event<MouseData>| {
                        if is_plain_primary_click(&e) {
                            e.prevent_default();
                            on_open.call((MediaKind::Tv, tmdb_id));
                        }
                    },
                    onauxclick: handle_aux_click,
                    div { class: "dl-poster", style: "background: {fallback_bg(tmdb_id)}",
                        if let Some(p) = &poster_path {
                            img { src: image_url(p, ImageSize::Poster), alt: "{primary_title}", loading: "lazy" }
                            span { class: "dl-poster-tag over", "{badge_lbl}" }
                        } else {
                            span { class: "dl-poster-tag", "{badge_lbl}" }
                        }
                    }
                    div { class: "dl-body",
                        div { class: "dl-title", "{primary_title}" }
                        if let Some(sub) = release_sub {
                            div { class: "dl-sub", title: "{sub}", "{sub}" }
                        }
                        div { class: "dl-tags",
                            span { class: "{status_cls}", "{status_text}" }
                            if let Some(sum) = ep_summary {
                                span { class: "tag srv", title: "Скачанные серии", "{sum}" }
                            }
                            for q in qualities {
                                span { class: "tag", "{q}" }
                            }
                            for v in voices {
                                span { class: "tag", "{v}" }
                            }
                            if known_bytes > 0 {
                                span { class: "tag", "{crate::human_size(known_bytes as u64)}" }
                            }
                            if let Some(who) = req_label {
                                span { class: "tag", title: "Добавил: {who}",
                                    Icon { svg: icons::USERS, class: "ic xs" }
                                    "{who}"
                                }
                            }
                            if let Some((txt, tcls)) = delete_tag {
                                span { class: "{tcls}",
                                    Icon { svg: icons::CLOCK, class: "ic xs" }
                                    "{txt}"
                                }
                            }
                        }
                        if any_active {
                            div { class: "{bar_cls}",
                                div { style: "width: {pct:.0}%" }
                            }
                        }
                    }
                }
                div { class: "dl-series-acts",
                    button {
                        class: "{toggle_cls}",
                        title: "{toggle_title}",
                        onclick: move |e| {
                            e.stop_propagation();
                            on_toggle.call(tmdb_id);
                        },
                        span { "{toggle_lbl}" }
                        Icon { svg: icons::CHEV_D, class: toggle_chev_cls }
                    }
                    if let Some(pid) = single_deletable_id {
                        button {
                            class: "{del_cls}",
                            title: "{del_title}",
                            "aria-label": "{del_aria}",
                            onblur: move |_| confirm_del.set(false),
                            onclick: move |e| {
                                e.stop_propagation();
                                if *confirm_del.peek() {
                                    confirm_del.set(false);
                                    on_delete.call(pid);
                                } else {
                                    confirm_del.set(true);
                                }
                            },
                            Icon { svg: icons::TRASH, class: "ic sm" }
                        }
                    }
                }
            }
            if expanded {
                SeriesPreloadDrawer {
                    tmdb_id,
                    seasons_map,
                    no_ep_items,
                    show_ep_release,
                    can_delete_season,
                    now,
                    on_open,
                    on_delete,
                    on_delete_episode,
                }
            }
        }
    }
}

#[component]
fn SeriesPreloadDrawer(
    tmdb_id: i64,
    seasons_map: BTreeMap<i32, Vec<DrawerEpisode>>,
    no_ep_items: Vec<PreloadItem>,
    show_ep_release: bool,
    can_delete_season: bool,
    now: DateTime<Utc>,
    on_open: EventHandler<(MediaKind, i64)>,
    on_delete: EventHandler<i64>,
    #[props(default)] on_delete_episode: Option<EventHandler<(i64, i32, i32)>>,
) -> Element {
    rsx! {
        div { class: "dl-ep-drawer",
            if seasons_map.is_empty() && no_ep_items.is_empty() {
                div { class: "dl-ep-empty", "Нет данных о сериях." }
            }
            for (s_num, rows) in seasons_map {
                {
                    let ready_cnt = rows.iter().filter(|r| r.ep.state == PreloadState::Ready).count();
                    let count_lbl = if ready_cnt == rows.len() {
                        format!("{} {}", rows.len(), plural(rows.len(), "серия", "серии", "серий"))
                    } else {
                        format!("{ready_cnt} из {} готово", rows.len())
                    };
                    let single_s_pid = {
                        let first = rows.first().filter(|r| r.can_delete).map(|r| r.preload_id);
                        first.filter(|&pid| can_delete_season && rows.iter().all(|r| r.preload_id == pid))
                    };
                    let season_lbl = if s_num > 0 {
                        format!("Сезон {s_num}")
                    } else {
                        "Серии".to_owned()
                    };
                    rsx! {
                        div { key: "ds-{s_num}",
                            div { class: "dl-ep-shead",
                                span { class: "dl-ep-stitle", "{season_lbl}" }
                                span { class: "muted", "{count_lbl}" }
                                if let Some(pid) = single_s_pid {
                                    SeriesSeasonDeleteBtn { preload_id: pid, on_delete }
                                }
                            }
                            div { class: "dl-ep-rows",
                                for row in rows {
                                    SeriesEpisodeRow {
                                        key: "{row.preload_id}-{row.ep.season}-{row.ep.episode}",
                                        tmdb_id,
                                        row,
                                        show_release: show_ep_release,
                                        now,
                                        on_open,
                                        on_delete_episode,
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for rel_item in no_ep_items {
                SeriesPendingReleaseRow {
                    key: "nop-{rel_item.id}",
                    item: rel_item,
                    on_delete,
                }
            }
        }
    }
}

#[component]
fn SeriesSeasonDeleteBtn(preload_id: i64, on_delete: EventHandler<i64>) -> Element {
    let mut confirm = use_signal(|| false);
    rsx! {
        button {
            class: if confirm() { "link warn" } else { "link" },
            title: "Удалить этот сезон с сервера",
            onblur: move |_| confirm.set(false),
            onclick: move |e| {
                e.stop_propagation();
                if *confirm.peek() {
                    confirm.set(false);
                    on_delete.call(preload_id);
                } else {
                    confirm.set(true);
                }
            },
            if confirm() { "Точно удалить сезон?" } else { "Удалить сезон" }
        }
    }
}

#[component]
fn SeriesPendingReleaseRow(item: PreloadItem, on_delete: EventHandler<i64>) -> Element {
    let mut confirm = use_signal(|| false);
    let label = if item.season > 0 {
        format!("Сезон {}", item.season)
    } else {
        item.title.clone().unwrap_or_else(|| "Раздача".to_owned())
    };
    let status = item.status_text();
    let pid = item.id;
    rsx! {
        div { class: "dl-ep-rel-note",
            span { "{label} · {status} — список серий появится после получения метаданных" }
            if item.can_delete {
                button {
                    class: if confirm() { "round-act xs danger-confirm" } else { "round-act xs danger" },
                    title: if confirm() { "Нажмите ещё раз, чтобы удалить" } else { "Удалить раздачу" },
                    onblur: move |_| confirm.set(false),
                    onclick: move |e| {
                        e.stop_propagation();
                        if *confirm.peek() {
                            confirm.set(false);
                            on_delete.call(pid);
                        } else {
                            confirm.set(true);
                        }
                    },
                    Icon { svg: icons::TRASH, class: "ic xs" }
                }
            }
        }
    }
}

#[component]
fn SeriesEpisodeRow(
    tmdb_id: i64,
    row: DrawerEpisode,
    show_release: bool,
    now: DateTime<Utc>,
    on_open: EventHandler<(MediaKind, i64)>,
    #[props(default)] on_delete_episode: Option<EventHandler<(i64, i32, i32)>>,
) -> Element {
    let mut confirm_del = use_signal(|| false);
    let ep = &row.ep;
    let code = episode_code(ep.season, ep.episode);
    let title_txt = match ep.episode_name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => format!("{}. {n}", ep.episode),
        None => format!("Серия {}", ep.episode),
    };
    let st_cls = match ep.state {
        PreloadState::Ready => "tag ok",
        PreloadState::Queued | PreloadState::Downloading => "tag acc",
        PreloadState::Failed => "tag err",
        PreloadState::Deleted => "tag",
    };
    let st_lbl = if ep.state == PreloadState::Ready {
        "На сервере".to_owned()
    } else {
        ep.status_text(row.dl_phase, row.speed_bps)
    };
    let sz_txt = ep.size_bytes.filter(|&b| b > 0).map(|b| crate::human_size(b as u64));
    let href = title_href(MediaKind::Tv, tmdb_id);
    let pid = row.preload_id;
    let (s_num, ep_num) = (ep.season, ep.episode);
    let confirm = confirm_del();
    let del_days = ep.delete_after.map(|at| (at - now).num_hours().max(0) as f64 / 24.0);
    rsx! {
        div { class: "dl-ep-row",
            a {
                class: "dl-ep-main",
                href: "{href}",
                onclick: move |e: Event<MouseData>| {
                    if is_plain_primary_click(&e) {
                        e.prevent_default();
                        on_open.call((MediaKind::Tv, tmdb_id));
                    }
                },
                onauxclick: handle_aux_click,
                span { class: "dl-ep-code", "{code}" }
                span { class: "dl-ep-name", title: "{title_txt}", "{title_txt}" }
            }
            div { class: "dl-ep-meta",
                if show_release {
                    if let Some(ref q) = row.quality {
                        span { class: "tag", "{q}" }
                    }
                    if let Some(ref v) = row.voice {
                        span { class: "tag", "{v}" }
                    }
                }
                if let Some(sz) = sz_txt {
                    span { class: "dl-ep-sz", "{sz}" }
                }
                span { class: "{st_cls}", "{st_lbl}" }
                if let Some(days) = del_days {
                    {
                        let del_lbl = if days < 1.0 {
                            "удалится сегодня".to_owned()
                        } else {
                            format!("удалится через {} дн.", days.ceil())
                        };
                        rsx! { span { class: "tag warn", "{del_lbl}" } }
                    }
                }
            }
            if let (true, Some(cb)) = (row.can_delete, on_delete_episode) {
                button {
                    class: if confirm { "round-act xs danger-confirm" } else { "round-act xs danger" },
                    title: if confirm { "Нажмите ещё раз, чтобы удалить серию" } else { "Удалить серию с сервера" },
                    "aria-label": if confirm { "Подтвердить удаление серии" } else { "Удалить серию с сервера" },
                    onblur: move |_| confirm_del.set(false),
                    onclick: move |e| {
                        e.stop_propagation();
                        if *confirm_del.peek() {
                            confirm_del.set(false);
                            cb.call((pid, s_num, ep_num));
                        } else {
                            confirm_del.set(true);
                        }
                    },
                    Icon { svg: icons::TRASH, class: "ic xs" }
                }
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

    #[test]
    fn groups_tv_preloads_by_series_and_keeps_movies_separate() {
        let base = |id, kind, tmdb_id, season| PreloadItem {
            id,
            kind,
            tmdb_id,
            season,
            infohash: format!("{id:040}"),
            title: None,
            state: PreloadState::Ready,
            progress: 1.0,
            size_bytes: None,
            requested_at: Utc::now(),
            ready_at: Some(Utc::now()),
            delete_after: None,
            poster_path: None,
            media_title: None,
            quality: None,
            voice: None,
            speed_bps: 0,
            dl_phase: PreloadPhase::Ready,
            whole_torrent: kind == MediaKind::Movie,
            episodes: Vec::new(),
            requested_by: None,
            can_delete: true,
        };
        let grouped = group_profile_downloads(vec![
            base(1, MediaKind::Tv, 250703, 1),
            base(2, MediaKind::Movie, 603, 0),
            base(3, MediaKind::Tv, 250703, 2),
        ]);
        assert_eq!(grouped.len(), 2);
        match &grouped[0] {
            DownloadGroup::Series { tmdb_id, items } => {
                assert_eq!(*tmdb_id, 250703);
                assert_eq!(items.len(), 2);
            }
            _ => panic!("expected series group first"),
        }
        assert!(matches!(grouped[1], DownloadGroup::Movie(ref m) if m.tmdb_id == 603));
    }
}
