//! Kino client. One codebase for the website and the Telegram Mini App;
//! desktop and mobile builds come later (see README).
//!
//! The screens live in `kino-ui`; this crate fetches data and handles clicks.

mod api;
mod native;
mod platform;
mod player;
mod sync;
mod telegram;
mod updater;

use player::{PlayStart, Player};

use std::collections::{HashMap, VecDeque};

use dioxus::prelude::*;
use kino_core::{
    image_url, normalize_quality_key, AuthResponse, CalendarEntry, ContinueItem, HistoryEntry, HomeFeed, ImageSize,
    MarkWatched, MediaKind, PersonDetails, PreloadItem, PreloadRequest, PreloadState, Progress, ReleaseChoice,
    ReleaseChoiceUpdate, ResolveReleaseRequest, ResolveReleaseResponse, RewatchMode, SearchResults, Season,
    TitleDetails, TitleExtras, TitleState, TitleStatus, TitleSummary, UserPrefs, UserPublicProfile, UserSocialLibrary,
    VideoScale, WidgetAuthRequest,
};
use kino_ui::{
    icons, play_target, AppUpdateUi, HomeView, Icon, LoginView, MyFilters, MyKindFilter, MySection, MyView, PersonView,
    PickerPanel, PlayTarget, ProfileView, ReleaseFilter, ReleaseOption, ReleasePicker, ReleaseSort, SearchView,
    ShelfView, Shell, Tab, Theme, TitleExtrasView, TitleView, UpdateBanner, FAVICON_URI, STYLE,
};

const FONTS_URL: &str = "https://fonts.googleapis.com/css2?family=Golos+Text:wght@400;500;600;700&family=Playfair+Display:ital,wght@0,500;0,600;0,700;1,500&display=swap";
const THEME_KEY: &str = "kino.theme";
const MY_FILTERS_KEY: &str = "kino.my_filters";

fn main() {
    #[cfg(any(feature = "mobile", feature = "desktop"))]
    {
        // The WebView's own page: let the picture go under the notch and system bars
        // (the screens add the safe-area padding themselves), and apply saved theme early.
        const HEAD: &str = r#"<script>
            var m = document.querySelector('meta[name=viewport]');
            if (m) { m.content = 'width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no, viewport-fit=cover'; }
            try {
                var t = localStorage.getItem('kino.theme');
                if (t === 'dark' || t === 'light') { document.documentElement.dataset.theme = t; }
            } catch (e) {}
        </script>"#;
        #[cfg(all(feature = "desktop", not(feature = "mobile")))]
        let config = desktop_config(HEAD);
        #[cfg(feature = "mobile")]
        let config = dioxus::mobile::Config::new()
            .with_background_color((0, 0, 0, 0))
            .with_custom_head(HEAD.to_owned());
        dioxus::LaunchBuilder::new().with_cfg(config).launch(App);
    }

    #[cfg(not(any(feature = "mobile", feature = "desktop")))]
    dioxus::launch(App);
}

#[cfg(all(feature = "desktop", not(feature = "mobile")))]
fn desktop_config(head: &str) -> dioxus::desktop::Config {
    use dioxus::desktop::{tao::window::Icon, Config, LogicalSize, WindowBuilder};

    let icon = Icon::from_rgba(include_bytes!("../../desktop/assets/icon-64.rgba").to_vec(), 64, 64).ok();
    let window = WindowBuilder::new()
        .with_title("Орловский кинопоказ")
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(LogicalSize::new(480.0, 600.0))
        .with_window_icon(icon);
    let mut config = Config::new()
        .with_window(window)
        .with_menu(None)
        .with_custom_head(head.to_owned());
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        config = config.with_data_directory(
            std::path::PathBuf::from(local_app_data)
                .join("Orel Kino")
                .join("webview"),
        );
    }
    config
}

#[derive(Clone, PartialEq)]
enum Session {
    Checking,
    LoggedOut {
        error: Option<String>,
    },
    LoggedIn {
        token: String,
        name: String,
        avatar_url: Option<String>,
    },
}

/// Tries every way to be logged in: Mini App data, Login Widget redirect, saved token.
async fn establish() -> Session {
    platform::init().await;
    let invite = telegram::invite();
    let attempt: Option<Result<AuthResponse, api::Failure>> = if let Some(init) = telegram::init_data() {
        Some(api::login_webapp(init, invite).await)
    } else if let Some(fields) = telegram::widget_fields() {
        let r = api::login_widget(WidgetAuthRequest { fields, invite }).await;
        telegram::clean_url();
        Some(r)
    } else if let Some(code) = telegram::login_token() {
        // The bot's `/login` link: the code was confirmed in the chat already.
        let r = match api::poll_login_link(&code).await {
            Ok(kino_core::LoginLinkStatus::Done { auth }) => Ok(auth),
            Ok(_) => Err(api::Failure {
                code: "expired".into(),
                message: "Ссылка для входа устарела. Отправьте боту /login ещё раз.".into(),
                available_episodes: Vec::new(),
            }),
            Err(f) => Err(f),
        };
        telegram::clean_url();
        Some(r)
    } else {
        None
    };

    match attempt {
        Some(Ok(auth)) => {
            api::save_token(&auth.token);
            return Session::LoggedIn {
                token: auth.token,
                name: auth.me.first_name.unwrap_or_default(),
                avatar_url: auth.me.avatar_url,
            };
        }
        Some(Err(f)) if f.code == "not_invited" => return Session::LoggedOut { error: Some(f.message) },
        Some(Err(f)) => {
            return Session::LoggedOut {
                error: Some(format!("Не удалось войти: {}", f.message)),
            }
        }
        None => {}
    }

    if let Some(token) = api::saved_token().await {
        match api::me(&token).await {
            Ok(me) => {
                return Session::LoggedIn {
                    token,
                    name: me.first_name.unwrap_or_default(),
                    avatar_url: me.avatar_url,
                }
            }
            Err(f) if f.code == "unauthorized" => api::forget_token(),
            Err(f) => return Session::LoggedOut { error: Some(f.message) },
        }
    }
    Session::LoggedOut { error: None }
}

#[component]
fn App() -> Element {
    let mut session = use_signal(|| Session::Checking);
    let mut theme = use_signal(|| {
        if telegram::is_telegram() {
            Theme::Auto
        } else {
            Theme::Dark
        }
    });
    use_effect(move || {
        spawn(async move {
            platform::init().await;
            telegram::init();
            let in_tg = telegram::is_telegram();
            let saved = platform::storage_get(THEME_KEY).await;
            let initial = Theme::parse(saved.as_deref(), in_tg);
            theme.set(initial);
            platform::apply_theme(initial);
            session.set(establish().await);
        });
    });

    let on_theme = move |next: Theme| {
        telegram::haptic(telegram::Haptic::Select);
        theme.set(next);
        if next == Theme::Auto {
            platform::storage_remove(THEME_KEY);
        } else {
            platform::storage_set(THEME_KEY, next.as_str());
        }
        platform::apply_theme(next);
    };

    rsx! {
        document::Link { rel: "icon", r#type: "image/svg+xml", href: FAVICON_URI }
        document::Link { rel: "preconnect", href: "https://fonts.googleapis.com" }
        document::Link { rel: "preconnect", href: "https://fonts.gstatic.com", crossorigin: "anonymous" }
        document::Link { rel: "stylesheet", href: FONTS_URL, media: "print", onload: "this.media='all'" }
        style { "{STYLE}" }
        match session() {
            Session::Checking => rsx! { p { class: "muted", style: "padding: 24px", "Загрузка…" } },
            Session::LoggedOut { error } => rsx! { Login { error, on_login: move |s| session.set(s) } },
            Session::LoggedIn { token, name, avatar_url } => rsx! {
                Main {
                    token,
                    name,
                    avatar_url,
                    theme: theme(),
                    on_theme,
                    on_logout: move |_| {
                        api::forget_token();
                        clear_home_snapshot();
                        session.set(Session::LoggedOut { error: None });
                    },
                }
            },
        }
    }
}

/// Where logging in through the Telegram app stands.
#[derive(Clone, PartialEq)]
enum LinkLogin {
    Idle,
    /// The bot is open in Telegram, waiting for the button press there.
    Waiting,
    Failed(String),
}

/// In the apps a link must be handed to the system; a page opens it by itself.
fn open_link(e: &Event<MouseData>, url: &str) {
    if !platform::is_safe_external_url(url) {
        e.prevent_default();
        return;
    }
    if !platform::is_web() {
        e.prevent_default();
        platform::open_external(url);
    }
}

#[component]
fn Login(error: Option<String>, on_login: EventHandler<Session>) -> Element {
    let mut state = use_signal(|| LinkLogin::Idle);
    // Made in advance so the button is a plain link: browsers block pop-ups
    // opened after a network round trip.
    let mut invite_input = use_signal(|| telegram::invite().unwrap_or_default());
    let mut invite = use_signal(telegram::invite);
    let mut invite_gen = use_signal(|| 0u64);
    let mut link = use_resource(move || api::start_login_link(invite()));
    let mut edit_server = use_signal(|| false);
    let mut server_input = use_signal(platform::server_base);
    let mut server_err = use_signal(|| None::<String>);
    let mut wait = move |token: String| {
        state.set(LinkLogin::Waiting);
        spawn(async move {
            // The link lives 10 minutes; ask every 2 seconds.
            for _ in 0..300 {
                platform::sleep(2_000).await;
                if *state.peek() != LinkLogin::Waiting {
                    return;
                }
                match api::poll_login_link(&token).await {
                    Ok(kino_core::LoginLinkStatus::Pending) => {}
                    Ok(kino_core::LoginLinkStatus::Done { auth }) => {
                        api::save_token(&auth.token);
                        on_login.call(Session::LoggedIn {
                            token: auth.token,
                            name: auth.me.first_name.unwrap_or_default(),
                            avatar_url: auth.me.avatar_url,
                        });
                        return;
                    }
                    Ok(kino_core::LoginLinkStatus::Expired) => break,
                    Err(f) if f.code == "not_invited" => {
                        link.restart();
                        return state.set(LinkLogin::Failed(f.message));
                    }
                    // A dropped request; try again on the next tick.
                    Err(_) => {}
                }
            }
            link.restart();
            state.set(LinkLogin::Failed("Время на вход вышло. Нажмите кнопку ещё раз.".into()));
        });
    };
    let link_error = match &*link.read() {
        Some(Err(f)) => Some(f.message.clone()),
        Some(Ok(l)) if !platform::is_safe_external_url(&l.url) => {
            Some("Сервер вернул некорректную ссылку для входа.".into())
        }
        _ => None,
    };
    let link_failed = link_error.is_some();
    let shown_error = match state() {
        LinkLogin::Failed(e) => Some(e),
        _ => link_error.or(error),
    };
    let ready = match &*link.read() {
        Some(Ok(l)) if platform::is_safe_external_url(&l.url) => Some(l.clone()),
        _ => None,
    };
    let server_host = platform::server_base()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_owned();
    rsx! {
        LoginView { invite: telegram::invite(), error: shown_error,
            if !platform::is_web() && state() != LinkLogin::Waiting {
                input {
                    class: "input-field",
                    placeholder: "Код приглашения (если есть)",
                    value: "{invite_input()}",
                    oninput: move |e| {
                        let raw = e.value();
                        invite_input.set(raw.clone());
                        let next_gen = *invite_gen.peek() + 1;
                        invite_gen.set(next_gen);
                        let trimmed = Some(raw.trim().to_owned()).filter(|v| !v.is_empty());
                        spawn(async move {
                            platform::sleep(400).await;
                            if *invite_gen.peek() == next_gen && *invite.peek() != trimmed {
                                invite.set(trimmed);
                            }
                        });
                    },
                }
            }
            match (state(), ready) {
                (LinkLogin::Waiting, Some(l)) => rsx! {
                    p { class: "login-wait-txt", "Откройте Telegram и нажмите «Войти на сайт» в сообщении бота. Эта страница откроется сама." }
                    a {
                        class: "btn glass",
                        href: "{l.url}",
                        target: "_blank",
                        rel: "noopener noreferrer",
                        onclick: move |e: Event<MouseData>| open_link(&e, &l.url),
                        Icon { svg: icons::TG, class: "ic sm" }
                        "Открыть Telegram ещё раз"
                    }
                    p { class: "login-cancel",
                        a { class: "link", href: "#", onclick: move |e: Event<MouseData>| { e.prevent_default(); state.set(LinkLogin::Idle) }, "Отмена" }
                    }
                },
                (_, Some(l)) => rsx! {
                    a { class: "btn primary", href: "{l.url}", target: "_blank", rel: "noopener noreferrer",
                        onclick: move |e: Event<MouseData>| {
                            open_link(&e, &l.url);
                            wait(l.token.clone());
                        },
                        Icon { svg: icons::TG, class: "ic sm" }
                        "Войти через Telegram"
                    }
                },
                (_, None) if link_failed => rsx! {
                    button {
                        class: "btn primary",
                        onclick: move |_| {
                            state.set(LinkLogin::Idle);
                            link.restart();
                        },
                        Icon { svg: icons::REFRESH, class: "ic sm" }
                        "Повторить"
                    }
                },
                (_, None) => rsx! { p { class: "muted", "Готовим вход…" } },
            }
            if !platform::is_web() {
                div { class: "server-row",
                    button {
                        class: "server-toggle",
                        onclick: move |_| {
                            server_input.set(platform::server_base());
                            server_err.set(None);
                            edit_server.toggle();
                        },
                        "Адрес сервера: {server_host} · Сменить"
                    }
                    if edit_server() {
                        div { class: "server-editor",
                            input {
                                class: "input-field",
                                placeholder: "https://kino.example.ru",
                                value: "{server_input()}",
                                oninput: move |e| {
                                    server_err.set(None);
                                    server_input.set(e.value());
                                },
                            }
                            button {
                                class: "btn glass sm",
                                style: "width: auto",
                                onclick: move |_| {
                                    let raw = server_input().trim().trim_end_matches('/').to_owned();
                                    if raw.is_empty() {
                                        server_err.set(Some("Введите адрес сервера.".into()));
                                        return;
                                    }
                                    let url = if raw.starts_with("http://") || raw.starts_with("https://") {
                                        raw
                                    } else {
                                        format!("https://{raw}")
                                    };
                                    let host = url
                                        .trim_start_matches("https://")
                                        .trim_start_matches("http://")
                                        .split('/')
                                        .next()
                                        .unwrap_or("");
                                    let valid = !host.is_empty()
                                        && !url.chars().any(char::is_whitespace)
                                        && (host.contains('.') || host.starts_with("localhost") || host.starts_with('['));
                                    if valid {
                                        server_err.set(None);
                                        platform::save_server(&url);
                                        platform::set_server_base(&url);
                                        edit_server.set(false);
                                        state.set(LinkLogin::Idle);
                                        link.restart();
                                    } else {
                                        server_err.set(Some(
                                            "Некорректный адрес сервера. Пример: https://kino.example.ru".into(),
                                        ));
                                    }
                                },
                                "Сохранить"
                            }
                        }
                        if let Some(err) = server_err() {
                            p { class: "err-text", style: "margin-top:8px;", "{err}" }
                        }
                    }
                }
            }
        }
    }
}

/// What the main area shows.
#[derive(Clone, PartialEq)]
enum View {
    Tab(Tab),
    /// A title page; `autoplay` starts playback at once (optionally targeting a season and release).
    Title {
        kind: MediaKind,
        id: i64,
        autoplay: bool,
        season: Option<i32>,
        release: Option<String>,
    },
    /// An actor's page.
    Person(i64),
    /// Full grid of a home shelf ("Смотреть все").
    Shelf {
        id: String,
        title: String,
        subtitle: Option<String>,
    },
}

#[derive(Clone, PartialEq)]
struct TrailEntry {
    view: View,
    scroll_y: f64,
}

#[derive(Clone, Copy, PartialEq)]
struct ScrollRequest {
    seq: u64,
    y: f64,
}

const MAX_CACHED_ENTRIES: usize = 20;
#[cfg(target_arch = "wasm32")]
const HOME_SNAPSHOT_KEY: &str = "kino.home_snapshot.v1";

#[derive(Clone, Default)]
struct HomeCache {
    feed: Option<HomeFeed>,
    continue_items: Vec<ContinueItem>,
    row_scrolls: HashMap<String, f64>,
    cats_scroll: f64,
}

#[derive(Clone)]
struct ShelfCache {
    title: String,
    subtitle: Option<String>,
    items: Vec<TitleSummary>,
    page: u32,
    has_more: bool,
}

#[derive(Clone)]
struct SearchCache {
    query: String,
    results: Option<SearchResults>,
    kind: MyKindFilter,
}

#[derive(Clone, Default)]
struct MineCache {
    items: Option<Vec<TitleState>>,
    users: Option<Vec<UserPublicProfile>>,
    calendar: Option<Vec<CalendarEntry>>,
    selected_user_id: Option<i64>,
    peer_libraries: HashMap<i64, UserSocialLibrary>,
    peer_status_filter: Option<TitleStatus>,
    cal_month: Option<(i32, u32)>,
    cal_day: Option<String>,
}

#[derive(Clone, Default)]
struct ProfileCache {
    history: Option<Vec<HistoryEntry>>,
    preloads: Option<Vec<PreloadItem>>,
    prefs: Option<UserPrefs>,
    prefs_open: bool,
    hist_limit: Option<usize>,
    expanded_series: Vec<i64>,
}

#[derive(Clone)]
struct TitleCache {
    details: TitleDetails,
    progress: Vec<Progress>,
    state: TitleState,
    choices: Vec<ReleaseChoice>,
    extras: Option<TitleExtras>,
    chosen_season: Option<i32>,
    seasons: HashMap<i32, Season>,
}

#[derive(Clone, Default)]
struct AppCache {
    home: Option<HomeCache>,
    shelves: HashMap<String, ShelfCache>,
    shelf_order: VecDeque<String>,
    search: Option<SearchCache>,
    mine: Option<MineCache>,
    profile: Option<ProfileCache>,
    persons: HashMap<i64, PersonDetails>,
    person_order: VecDeque<i64>,
    titles: HashMap<(MediaKind, i64), TitleCache>,
    title_order: VecDeque<(MediaKind, i64)>,
}

fn push_bounded<K: Eq + std::hash::Hash + Clone, V>(map: &mut HashMap<K, V>, order: &mut VecDeque<K>, key: K, val: V) {
    if !map.contains_key(&key) {
        if order.len() >= MAX_CACHED_ENTRIES {
            if let Some(old) = order.pop_front() {
                map.remove(&old);
            }
        }
        order.push_back(key.clone());
    }
    map.insert(key, val);
}

impl AppCache {
    fn insert_shelf(&mut self, id: String, shelf: ShelfCache) {
        push_bounded(&mut self.shelves, &mut self.shelf_order, id, shelf);
    }

    fn insert_person(&mut self, id: i64, person: PersonDetails) {
        push_bounded(&mut self.persons, &mut self.person_order, id, person);
    }

    fn upsert_title(&mut self, key: (MediaKind, i64), entry: TitleCache) {
        push_bounded(&mut self.titles, &mut self.title_order, key, entry);
    }

    fn patch_title(
        &mut self,
        kind: MediaKind,
        tmdb_id: i64,
        user_status: Option<TitleStatus>,
        user_rating: Option<Option<f32>>,
        preloaded: Option<bool>,
    ) {
        let patch_summary = |s: &mut TitleSummary| {
            if s.kind == kind && s.tmdb_id == tmdb_id {
                s.user_status = user_status;
                if let Some(r) = user_rating {
                    s.user_rating = r;
                }
                if let Some(pre) = preloaded {
                    s.preloaded = pre;
                }
            }
        };
        if let Some(home) = &mut self.home {
            if let Some(feed) = &mut home.feed {
                for row in &mut feed.rows {
                    for item in &mut row.items {
                        patch_summary(item);
                    }
                }
            }
            for cont in &mut home.continue_items {
                if let Some(t) = &mut cont.title {
                    patch_summary(t);
                }
            }
        }
        for shelf in self.shelves.values_mut() {
            for item in &mut shelf.items {
                patch_summary(item);
            }
        }
        if let Some(search) = &mut self.search {
            if let Some(res) = &mut search.results {
                for item in &mut res.items {
                    patch_summary(item);
                }
            }
        }
        for person in self.persons.values_mut() {
            for item in &mut person.credits {
                patch_summary(item);
            }
        }
        for entry in self.titles.values_mut() {
            patch_summary(&mut entry.details.summary);
            if let Some(ex) = &mut entry.extras {
                for item in &mut ex.collection {
                    patch_summary(item);
                }
                for item in &mut ex.recommendations {
                    patch_summary(item);
                }
                for item in &mut ex.similar {
                    patch_summary(item);
                }
            }
        }
        if let Some(mine) = &mut self.mine {
            if let Some(items) = &mut mine.items {
                for st in items {
                    if st.kind == kind && st.tmdb_id == tmdb_id {
                        st.status = user_status;
                        if let Some(r) = user_rating {
                            st.effective_rating = r;
                        }
                    }
                    if let Some(t) = &mut st.title {
                        patch_summary(t);
                    }
                }
            }
            for lib in mine.peer_libraries.values_mut() {
                for st in &mut lib.items {
                    if let Some(t) = &mut st.title {
                        patch_summary(t);
                    }
                }
            }
        }
        if let Some(profile) = &mut self.profile {
            if let Some(history) = &mut profile.history {
                for h in history {
                    if let Some(t) = &mut h.title {
                        patch_summary(t);
                    }
                }
            }
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct HomeSessionSnapshot {
    category: String,
    scroll_y: f64,
    cats_scroll: f64,
    row_scrolls: HashMap<String, f64>,
    feed: Option<HomeFeed>,
    continue_items: Vec<ContinueItem>,
}

#[cfg(target_arch = "wasm32")]
fn save_home_snapshot(category: &str, scroll_y: f64, home: &HomeCache) {
    let snap = HomeSessionSnapshot {
        category: category.to_owned(),
        scroll_y,
        cats_scroll: home.cats_scroll,
        row_scrolls: home.row_scrolls.clone(),
        feed: home.feed.clone(),
        continue_items: home.continue_items.clone(),
    };
    if let Ok(json) = serde_json::to_string(&snap) {
        if let Some(storage) = web_sys::window().and_then(|w| w.session_storage().ok().flatten()) {
            let _ = storage.set_item(HOME_SNAPSHOT_KEY, &json);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn save_home_snapshot(_category: &str, _scroll_y: f64, _home: &HomeCache) {}

#[cfg(target_arch = "wasm32")]
fn load_home_snapshot() -> Option<HomeSessionSnapshot> {
    let storage = web_sys::window()?.session_storage().ok().flatten()?;
    let raw = storage.get_item(HOME_SNAPSHOT_KEY).ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn load_home_snapshot() -> Option<HomeSessionSnapshot> {
    None
}

#[cfg(target_arch = "wasm32")]
fn clear_home_snapshot() {
    if let Some(storage) = web_sys::window().and_then(|w| w.session_storage().ok().flatten()) {
        let _ = storage.remove_item(HOME_SNAPSHOT_KEY);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn clear_home_snapshot() {}

#[cfg(target_arch = "wasm32")]
fn is_back_forward_nav() -> bool {
    let Some(w) = web_sys::window() else { return false };
    let Ok(perf) = js_sys::Reflect::get(&w, &wasm_bindgen::JsValue::from_str("performance")) else {
        return false;
    };
    let Ok(func) = js_sys::Reflect::get(&perf, &wasm_bindgen::JsValue::from_str("getEntriesByType")) else {
        return false;
    };
    let Ok(f) = wasm_bindgen::JsCast::dyn_into::<js_sys::Function>(func) else {
        return false;
    };
    let Ok(entries) = f.call1(&perf, &wasm_bindgen::JsValue::from_str("navigation")) else {
        return false;
    };
    let arr: js_sys::Array = entries.into();
    if arr.length() == 0 {
        return false;
    }
    let first = arr.get(0);
    js_sys::Reflect::get(&first, &wasm_bindgen::JsValue::from_str("type"))
        .ok()
        .and_then(|v| v.as_string())
        .is_some_and(|t| t == "back_forward")
}

#[cfg(not(target_arch = "wasm32"))]
fn is_back_forward_nav() -> bool {
    false
}

#[cfg(target_arch = "wasm32")]
fn current_scroll_y(_fallback: f64) -> f64 {
    web_sys::window().and_then(|w| w.scroll_y().ok()).unwrap_or(0.0)
}

#[cfg(not(target_arch = "wasm32"))]
fn current_scroll_y(fallback: f64) -> f64 {
    fallback
}

fn view_url(view: &View, my_section: MySection, query: &str) -> String {
    match view {
        View::Tab(Tab::Home) => "/".to_owned(),
        View::Tab(Tab::Search) => {
            let trimmed = query.trim();
            if trimmed.is_empty() {
                "/?tab=search".to_owned()
            } else {
                format!("/?tab=search&q={}", urlencoding::encode(trimmed))
            }
        }
        View::Tab(Tab::Mine) => match my_section {
            MySection::Lists => "/?tab=my".to_owned(),
            MySection::Users => "/?tab=my&sec=users".to_owned(),
            MySection::Calendar => "/?tab=my&sec=calendar".to_owned(),
        },
        View::Tab(Tab::Profile) => "/?tab=profile".to_owned(),
        View::Title { kind, id, .. } => format!("/?open={}_{id}", kind.as_str()),
        View::Person(id) => format!("/?person={id}"),
        View::Shelf { id, .. } => format!("/?shelf={}", urlencoding::encode(id)),
    }
}

fn normalize_prev_view(v: View) -> View {
    match v {
        View::Title { kind, id, .. } => View::Title {
            kind,
            id,
            autoplay: false,
            season: None,
            release: None,
        },
        other => other,
    }
}

fn schedule_scroll(mut scroll_req: Signal<ScrollRequest>, y: f64) {
    let next = scroll_req.peek().seq + 1;
    scroll_req.set(ScrollRequest { seq: next, y });
}

#[allow(clippy::too_many_arguments)]
fn push_view(
    mut view: Signal<View>,
    mut trail: Signal<Vec<TrailEntry>>,
    home_category: Signal<String>,
    cache: Signal<AppCache>,
    scroll_req: Signal<ScrollRequest>,
    live_scroll_y: Signal<f64>,
    next_view: View,
    url: Option<&str>,
    push_hist: bool,
) {
    let sy = current_scroll_y(*live_scroll_y.peek());
    let prev = normalize_prev_view(view.peek().clone());
    if matches!(prev, View::Tab(Tab::Home)) {
        if let Some(home) = cache.peek().home.as_ref() {
            save_home_snapshot(&home_category.peek(), sy, home);
        }
    }
    trail.with_mut(|t| {
        t.push(TrailEntry {
            view: prev,
            scroll_y: sy,
        })
    });
    view.set(next_view);
    schedule_scroll(scroll_req, 0.0);
    if push_hist {
        telegram::push_history(url);
    }
}

fn step_back(
    mut view: Signal<View>,
    mut trail: Signal<Vec<TrailEntry>>,
    mut query: Signal<String>,
    scroll_req: Signal<ScrollRequest>,
    from_pop: bool,
) {
    if let Some(prev) = trail.with_mut(|t| t.pop()) {
        if matches!(prev.view, View::Tab(Tab::Home)) && trail.peek().is_empty() {
            query.set(String::new());
        }
        view.set(prev.view);
        schedule_scroll(scroll_req, prev.scroll_y);
        if !from_pop {
            telegram::pop_history();
        }
    }
}

#[component]
fn Main(
    token: String,
    name: String,
    avatar_url: Option<String>,
    theme: Theme,
    on_theme: EventHandler<Theme>,
    on_logout: EventHandler<()>,
) -> Element {
    let initial_route = use_hook(telegram::initial_route);
    let initial_my_sec = match &initial_route {
        telegram::InitialRoute::Mine(Some(s)) if s == "users" => MySection::Users,
        telegram::InitialRoute::Mine(Some(s)) if s == "calendar" => MySection::Calendar,
        _ => MySection::Lists,
    };
    let initial_query = match &initial_route {
        telegram::InitialRoute::Search(q) => q.clone(),
        _ => String::new(),
    };
    let initial_view = match &initial_route {
        telegram::InitialRoute::Home => View::Tab(Tab::Home),
        telegram::InitialRoute::Search(_) => View::Tab(Tab::Search),
        telegram::InitialRoute::Mine(_) => View::Tab(Tab::Mine),
        telegram::InitialRoute::Profile => View::Tab(Tab::Profile),
        telegram::InitialRoute::Title(t) => View::Title {
            kind: t.kind,
            id: t.tmdb_id,
            autoplay: t.autoplay,
            season: t.season,
            release: t.release.clone(),
        },
        telegram::InitialRoute::Person(id) => View::Person(*id),
        telegram::InitialRoute::Shelf(id) => View::Shelf {
            id: id.clone(),
            title: String::new(),
            subtitle: None,
        },
    };
    let has_initial_trail = !matches!(initial_route, telegram::InitialRoute::Home);
    let restored_snap = use_hook(|| {
        let allow = has_initial_trail || is_back_forward_nav();
        if !allow {
            clear_home_snapshot();
            return None;
        }
        load_home_snapshot()
    });
    let init_category = restored_snap
        .as_ref()
        .map(|s| s.category.clone())
        .unwrap_or_else(|| "for_you".to_owned());
    let init_home_scroll = restored_snap.as_ref().map(|s| s.scroll_y).unwrap_or(0.0);
    let mut cache = use_signal(|| {
        let mut c = AppCache::default();
        if let Some(snap) = &restored_snap {
            c.home = Some(HomeCache {
                feed: snap.feed.clone(),
                continue_items: snap.continue_items.clone(),
                row_scrolls: snap.row_scrolls.clone(),
                cats_scroll: snap.cats_scroll,
            });
        }
        c
    });
    let mut view = use_signal(|| initial_view.clone());
    let mut trail = use_signal(|| {
        if has_initial_trail {
            vec![TrailEntry {
                view: View::Tab(Tab::Home),
                scroll_y: init_home_scroll,
            }]
        } else {
            Vec::new()
        }
    });
    let mut query = use_signal(|| initial_query.clone());
    let mut my_section = use_signal(|| initial_my_sec);
    use_hook(|| {
        let init_url = view_url(&initial_view, initial_my_sec, &initial_query);
        telegram::init_history(has_initial_trail, (init_url != "/").then_some(init_url.as_str()));
    });
    let mut home_category = use_signal(|| init_category);
    let mut home_epoch = use_signal(|| 0u64);
    let mut my_filters = use_signal(MyFilters::default);
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut live_scroll_y = use_signal(|| 0.0f64);
    let scroll_req = use_signal(|| ScrollRequest {
        seq: u64::from(!has_initial_trail && init_home_scroll > 0.0),
        y: if !has_initial_trail { init_home_scroll } else { 0.0 },
    });
    let mut applied_scroll_seq = use_signal(|| 0u64);
    use_effect(move || {
        let req = scroll_req();
        if req.seq == 0 || *applied_scroll_seq.peek() == req.seq {
            return;
        }
        applied_scroll_seq.set(req.seq);
        let y = req.y.max(0.0);
        let js = format!(
            "(function(y) {{ \
               window.scrollTo({{ top: y, left: 0, behavior: 'instant' }}); \
               if (y > 0 && Math.abs((window.scrollY || 0) - y) > 2) {{ \
                 requestAnimationFrame(function() {{ \
                   window.scrollTo({{ top: y, left: 0, behavior: 'instant' }}); \
                   if (Math.abs((window.scrollY || 0) - y) > 2) {{ \
                     requestAnimationFrame(function() {{ \
                       window.scrollTo({{ top: y, left: 0, behavior: 'instant' }}); \
                     }}); \
                   }} \
                 }}); \
               }} \
             }})({y});"
        );
        let _ = dioxus::document::eval(&js);
    });
    #[cfg(not(target_arch = "wasm32"))]
    use_hook(move || {
        spawn(async move {
            let mut bridge = dioxus::document::eval(
                "if (!window.__kinoScrollTrack) {
                   window.__kinoScrollTrack = true;
                   var lastT = 0, lastY = -1, timer = null;
                   var send = function() {
                     var y = window.scrollY || 0;
                     if (y !== lastY) { lastY = y; dioxus.send(y); }
                   };
                   window.addEventListener('scroll', function() {
                     var now = Date.now();
                     if (now - lastT > 80) { lastT = now; send(); }
                     clearTimeout(timer);
                     timer = setTimeout(send, 120);
                   }, { passive: true });
                 }",
            );
            while let Ok(y) = bridge.recv::<f64>().await {
                live_scroll_y.set(y);
            }
        });
    });
    // Restore saved "Мои" filters on startup.
    use_effect(move || {
        spawn(async move {
            if let Some(saved) = platform::storage_get(MY_FILTERS_KEY).await {
                my_filters.set(MyFilters::decode(&saved));
            }
        });
    });
    // Global Ctrl+K / Cmd+K focuses the search box (disabled while playing video).
    use_hook(|| {
        let _ = dioxus::document::eval(
            "if (!window.__kinoSearchShortcut) {
               window.__kinoSearchShortcut = true;
               window.addEventListener('keydown', function(e) {
                 if (document.documentElement.classList.contains('kino-playing')) return;
                 if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
                   var el = document.getElementById('kino-search-input');
                   if (el) { e.preventDefault(); el.focus(); el.select(); }
                 }
               });
             }",
        );
    });
    // Lightweight active-download check for the topbar badge / mobile avatar dot (auto-polls while active).
    let t_dl = token.clone();
    let mut active_preloads = use_resource(move || {
        let t = t_dl.clone();
        async move { api::preloads(&t).await.ok() }
    });
    let has_active_dl = active_preloads
        .read()
        .as_ref()
        .and_then(Option::as_ref)
        .is_some_and(|list| {
            list.iter()
                .any(|p| matches!(p.state, PreloadState::Queued | PreloadState::Downloading))
        });
    let mut top_poll_gen = use_signal(|| 0u64);
    use_effect(use_reactive!(|has_active_dl| {
        let gen = *top_poll_gen.peek() + 1;
        top_poll_gen.set(gen);
        if has_active_dl {
            spawn(async move {
                loop {
                    platform::sleep(4_000).await;
                    if *top_poll_gen.peek() != gen {
                        break;
                    }
                    active_preloads.restart();
                }
            });
        }
    }));
    use_drop(move || {
        let next = *top_poll_gen.peek() + 1;
        top_poll_gen.set(next);
    });
    let dl_percent: Option<u8> = active_preloads
        .read()
        .as_ref()
        .and_then(Option::as_ref)
        .and_then(|list| {
            list.iter()
                .find(|p| p.state == PreloadState::Downloading)
                .or_else(|| list.iter().find(|p| p.state == PreloadState::Queued))
        })
        .map(|p| (p.progress * 100.0).clamp(0.0, 100.0).round() as u8);

    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let tab = match view() {
        View::Tab(t) => Some(t),
        View::Title { .. } | View::Person(_) | View::Shelf { .. } => None,
    };
    let open_title = move |kind: MediaKind, id: i64, play: bool| {
        let url = format!("/?open={}_{id}", kind.as_str());
        push_view(
            view,
            trail,
            home_category,
            cache,
            scroll_req,
            live_scroll_y,
            View::Title {
                kind,
                id,
                autoplay: play,
                season: None,
                release: None,
            },
            Some(&url),
            true,
        );
        if !telegram::is_telegram() {
            let _ = api::reload_for_web_update(play);
        }
    };
    let open = move |t: TitleSummary| open_title(t.kind, t.tmdb_id, false);
    let open_person = move |id: i64| {
        telegram::haptic(telegram::Haptic::Tap);
        let url = format!("/?person={id}");
        push_view(
            view,
            trail,
            home_category,
            cache,
            scroll_req,
            live_scroll_y,
            View::Person(id),
            Some(&url),
            true,
        );
        if !telegram::is_telegram() {
            let _ = api::reload_for_web_update(false);
        }
    };
    let open_shelf = move |(id, title, subtitle): (String, String, Option<String>)| {
        telegram::haptic(telegram::Haptic::Tap);
        cache.with_mut(|c| {
            c.shelves.remove(&id);
        });
        let url = format!("/?shelf={}", urlencoding::encode(&id));
        push_view(
            view,
            trail,
            home_category,
            cache,
            scroll_req,
            live_scroll_y,
            View::Shelf { id, title, subtitle },
            Some(&url),
            true,
        );
        if !telegram::is_telegram() {
            let _ = api::reload_for_web_update(false);
        }
    };
    let go_back = move |_: ()| {
        telegram::haptic(telegram::Haptic::Tap);
        step_back(view, trail, query, scroll_req, false);
    };
    let mut update_state = use_signal(|| None::<updater::UpdateState>);
    let mut update_dismissed = use_signal(|| false);
    use_hook(|| {
        if updater::is_supported() {
            spawn(async move {
                platform::sleep(2_000).await;
                if update_state.peek().is_none() {
                    if let Ok(Some(manifest)) = updater::check_for_update().await {
                        update_state.set(Some(updater::UpdateState::Available(manifest)));
                    }
                }
            });
        }
    });
    let on_check_update = Callback::new(move |_: ()| {
        if !updater::is_supported() {
            return;
        }
        if matches!(
            *update_state.peek(),
            Some(
                updater::UpdateState::Checking
                    | updater::UpdateState::Downloading { .. }
                    | updater::UpdateState::Installing(_)
            )
        ) {
            return;
        }
        update_state.set(Some(updater::UpdateState::Checking));
        spawn(async move {
            match updater::check_for_update().await {
                Ok(Some(manifest)) => {
                    update_dismissed.set(false);
                    update_state.set(Some(updater::UpdateState::Available(manifest)));
                }
                Ok(None) => update_state.set(Some(updater::UpdateState::UpToDate)),
                Err(message) => update_state.set(Some(updater::UpdateState::Error {
                    manifest: None,
                    message,
                })),
            }
        });
    });
    let on_install_update = Callback::new(move |_: ()| {
        let manifest = match update_state.peek().clone() {
            Some(updater::UpdateState::Available(m)) | Some(updater::UpdateState::Error { manifest: Some(m), .. }) => m,
            _ => return,
        };
        update_dismissed.set(false);
        update_state.set(Some(updater::UpdateState::Downloading {
            manifest: manifest.clone(),
            percent: 0,
        }));
        spawn(async move {
            let m_prog = manifest.clone();
            let res = updater::download_and_install(manifest.clone(), move |step| {
                let next = match step {
                    updater::DownloadStep::Progress(pct) => updater::UpdateState::Downloading {
                        manifest: m_prog.clone(),
                        percent: pct,
                    },
                    updater::DownloadStep::Installing => updater::UpdateState::Installing(m_prog.clone()),
                };
                update_state.set(Some(next));
            })
            .await;
            match res {
                Ok(()) => update_state.set(Some(updater::UpdateState::Available(manifest))),
                Err(message) => update_state.set(Some(updater::UpdateState::Error {
                    manifest: Some(manifest),
                    message,
                })),
            }
        });
    });
    let update_banner = if !updater::is_supported() || update_dismissed() {
        None
    } else {
        match update_state() {
            Some(updater::UpdateState::Available(m)) => Some(UpdateBanner {
                version: m.version,
                downloading_pct: None,
                installing: false,
                error: None,
            }),
            Some(updater::UpdateState::Downloading { manifest, percent }) => Some(UpdateBanner {
                version: manifest.version,
                downloading_pct: Some(percent),
                installing: false,
                error: None,
            }),
            Some(updater::UpdateState::Installing(m)) => Some(UpdateBanner {
                version: m.version,
                downloading_pct: None,
                installing: true,
                error: None,
            }),
            Some(updater::UpdateState::Error {
                manifest: Some(m),
                message,
            }) => Some(UpdateBanner {
                version: m.version,
                downloading_pct: None,
                installing: false,
                error: Some(message),
            }),
            _ => None,
        }
    };
    let profile_update_ui = if updater::is_supported() {
        match update_state() {
            Some(updater::UpdateState::Checking) => Some(AppUpdateUi::Checking),
            Some(updater::UpdateState::Available(m)) => Some(AppUpdateUi::Available(m.version)),
            Some(updater::UpdateState::Downloading { percent, .. }) => Some(AppUpdateUi::Downloading(percent)),
            Some(updater::UpdateState::Installing(_)) => Some(AppUpdateUi::Installing),
            Some(updater::UpdateState::Error { manifest, message }) => Some(AppUpdateUi::Error {
                can_retry_install: manifest.is_some(),
                message,
            }),
            Some(updater::UpdateState::UpToDate) => Some(AppUpdateUi::UpToDate),
            None => None,
        }
    } else {
        None
    };
    let can_back = !trail().is_empty();
    rsx! {
        if can_back {
            native::BackButton { on_back: go_back }
        }
        Shell {
            tab,
            query: query(),
            user_initial: initial,
            avatar_url: avatar_url.clone(),
            dl_percent,
            update_banner,
            on_install_update: Some(on_install_update),
            on_dismiss_update: Some(Callback::new(move |_| update_dismissed.set(true))),
            on_tab: move |t| {
                telegram::haptic(telegram::Haptic::Tap);
                if t != Tab::Search {
                    query.set(String::new());
                }
                if t == Tab::Home {
                    home_category.set("for_you".to_owned());
                    cache.with_mut(|c| c.home = None);
                    clear_home_snapshot();
                    let next = *home_epoch.peek() + 1;
                    home_epoch.set(next);
                    trail.set(Vec::new());
                    view.set(View::Tab(Tab::Home));
                    schedule_scroll(scroll_req, 0.0);
                    if !telegram::is_telegram() {
                        telegram::reset_history();
                    }
                    let _ = api::reload_for_web_update(false);
                    return;
                }
                if view() == View::Tab(t) {
                    if !telegram::is_telegram() && api::reload_for_web_update(false) {
                        return;
                    }
                    if t == Tab::Search {
                        let _ = dioxus::document::eval(
                            "var el = document.getElementById('kino-search-input'); if (el) el.focus();",
                        );
                    } else {
                        schedule_scroll(scroll_req, 0.0);
                    }
                    return;
                }
                if telegram::is_telegram() {
                    trail.set(Vec::new());
                    view.set(View::Tab(t));
                    schedule_scroll(scroll_req, 0.0);
                } else {
                    let next = View::Tab(t);
                    let url = view_url(&next, *my_section.peek(), &query.peek());
                    push_view(
                        view,
                        trail,
                        home_category,
                        cache,
                        scroll_req,
                        live_scroll_y,
                        next,
                        Some(&url),
                        true,
                    );
                    if api::reload_for_web_update(false) {
                        return;
                    }
                }
                if t == Tab::Profile {
                    active_preloads.restart();
                }
                if t == Tab::Search {
                    spawn(async move {
                        platform::sleep(60).await;
                        let _ = dioxus::document::eval(
                            "var el = document.getElementById('kino-search-input'); if (el) el.focus();",
                        );
                    });
                }
            },
            on_query: move |q: String| {
                let empty = q.trim().is_empty();
                if empty {
                    // On phones Search is a dedicated tab: clearing the box keeps you on Search.
                    // On desktop, clearing the topbar search box returns to the previous screen.
                    if matches!(view(), View::Tab(Tab::Search)) && !telegram::is_mobile() {
                        if trail.peek().is_empty() {
                            view.set(View::Tab(Tab::Home));
                            schedule_scroll(scroll_req, 0.0);
                        } else {
                            step_back(view, trail, query, scroll_req, false);
                        }
                    }
                } else if !matches!(view(), View::Tab(Tab::Search)) {
                    if !telegram::is_telegram() {
                        let url = view_url(&View::Tab(Tab::Search), *my_section.peek(), &q);
                        push_view(
                            view,
                            trail,
                            home_category,
                            cache,
                            scroll_req,
                            live_scroll_y,
                            View::Tab(Tab::Search),
                            Some(&url),
                            true,
                        );
                    } else {
                        trail.set(Vec::new());
                        view.set(View::Tab(Tab::Search));
                        schedule_scroll(scroll_req, 0.0);
                    }
                } else {
                    telegram::replace_history(&view_url(&View::Tab(Tab::Search), *my_section.peek(), &q));
                }
                query.set(q);
            },
            match view() {
                View::Tab(Tab::Home) => rsx! {
                    Home {
                        key: "home-{home_epoch()}",
                        token: token.clone(),
                        active_category: home_category,
                        cache,
                        on_open: open,
                        on_open_title: move |(k, id)| open_title(k, id, false),
                        on_play: move |(k, id)| open_title(k, id, true),
                        on_open_shelf: open_shelf,
                    }
                },
                View::Tab(Tab::Search) => rsx! { Search { token: token.clone(), query, cache, on_open: open } },
                View::Tab(Tab::Mine) => rsx! {
                    Mine {
                        token: token.clone(),
                        section: my_section,
                        filters: my_filters,
                        cache,
                        on_section: move |s: MySection| {
                            my_section.set(s);
                            telegram::replace_history(&view_url(&View::Tab(Tab::Mine), s, &query.peek()));
                        },
                        on_filters: move |f: MyFilters| {
                            telegram::haptic(telegram::Haptic::Select);
                            my_filters.set(f);
                            platform::storage_set(MY_FILTERS_KEY, &f.encode());
                        },
                        on_open: open,
                        on_open_id: move |(k, id)| open_title(k, id, false),
                    }
                },
                View::Tab(Tab::Profile) => rsx! {
                    Profile {
                        token: token.clone(),
                        name: name.clone(),
                        avatar_url: avatar_url.clone(),
                        theme,
                        cache,
                        app_version: updater::is_supported().then(|| updater::APP_VERSION.to_owned()),
                        update_state: profile_update_ui.clone(),
                        on_check_update: Some(on_check_update),
                        on_install_update: Some(on_install_update),
                        on_theme,
                        on_preloads_changed: move |_| active_preloads.restart(),
                        on_open: move |(k, id)| open_title(k, id, false),
                        on_logout,
                    }
                },
                View::Title {
                    kind,
                    id,
                    autoplay,
                    season,
                    release,
                } => {
                    let s_key = season.unwrap_or(0);
                    let r_key = release.as_deref().unwrap_or("");
                    rsx! {
                        Title {
                            key: "{kind.as_str()}{id}{autoplay}{s_key}{r_key}",
                            token: token.clone(),
                            kind,
                            id,
                            autoplay,
                            auto_season: season,
                            auto_release: release,
                            cache,
                            on_preloads_changed: move |_| active_preloads.restart(),
                            on_open: open,
                            on_person: open_person,
                            // Closing a player started directly from Home / deep-link returns to the previous screen.
                            on_exit: move |from_pop: bool| {
                                step_back(view, trail, query, scroll_req, from_pop);
                            },
                        }
                    }
                },
                View::Person(id) => rsx! { Person { key: "person{id}", token: token.clone(), id, cache, on_open: open } },
                View::Shelf { id, title, subtitle } => rsx! {
                    Shelf {
                        key: "shelf-{id}",
                        token: token.clone(),
                        id,
                        title,
                        subtitle,
                        cache,
                        on_open: open,
                    }
                },
            }
        }
    }
}

/// Fires once when it appears: lets a page start playback after it has loaded.
#[component]
fn AutoPlay(on_start: EventHandler<()>) -> Element {
    use_effect(move || on_start.call(()));
    rsx! {}
}

fn error_text(message: &str) -> Element {
    rsx! { p { class: "error section", "{message}" } }
}

fn loading() -> Element {
    rsx! { p { class: "muted section", "Загрузка…" } }
}

#[component]
fn Person(token: String, id: i64, cache: Signal<AppCache>, on_open: EventHandler<TitleSummary>) -> Element {
    let cached = use_hook(|| cache.peek().persons.get(&id).cloned());
    let mut person_state = use_signal(|| cached);
    let data = use_resource(move || {
        let t = token.clone();
        async move { api::person(&t, id).await }
    });
    use_effect(move || {
        if let Some(Ok(p)) = &*data.read() {
            person_state.set(Some(p.clone()));
            cache.with_mut(|c| c.insert_person(id, p.clone()));
        }
    });
    if let Some(person) = person_state() {
        return rsx! { PersonView { person, on_open } };
    }
    let state = data.read().clone();
    match state {
        None => loading(),
        Some(Err(e)) => error_text(&e.message),
        Some(Ok(person)) => rsx! { PersonView { person, on_open } },
    }
}

const HOME_LAZY_BATCH: usize = 4;
const MAX_ROW_AUTO_PAGES: u32 = 4;

#[component]
fn Home(
    token: String,
    active_category: Signal<String>,
    cache: Signal<AppCache>,
    on_open: EventHandler<TitleSummary>,
    on_open_title: EventHandler<(MediaKind, i64)>,
    on_play: EventHandler<(MediaKind, i64)>,
    on_open_shelf: EventHandler<(String, String, Option<String>)>,
) -> Element {
    let cached_home = use_hook(|| cache.peek().home.clone());
    let has_cached_feed = cached_home.as_ref().and_then(|c| c.feed.as_ref()).is_some();
    let initial_cont = cached_home
        .as_ref()
        .map(|c| c.continue_items.clone())
        .unwrap_or_default();
    let initial_row_scrolls = use_hook(|| cached_home.as_ref().map(|c| c.row_scrolls.clone()).unwrap_or_default());
    let initial_cats_scroll = cached_home.as_ref().map(|c| c.cats_scroll).unwrap_or(0.0);
    let cat_already_has_rows = cached_home.as_ref().and_then(|c| c.feed.as_ref()).is_some_and(|f| {
        let cat = active_category.peek();
        *cat == "for_you"
            || f.rows
                .iter()
                .any(|r| r.loaded && r.categories.iter().any(|c| c == &*cat))
    });

    let mut cont_items_state = use_signal(|| initial_cont);
    let mut feed_state = use_signal(|| cached_home.as_ref().and_then(|c| c.feed.clone()));
    let mut need_feed_fetch = use_signal(|| !has_cached_feed);

    let token_cont = token.clone();
    let mut cont_res = use_resource(move || {
        let t = token_cont.clone();
        async move { api::continue_watching(&t).await }
    });
    let token_feed = token.clone();
    let mut feed_res = use_resource(move || {
        let t = token_feed.clone();
        let should_fetch = need_feed_fetch();
        async move {
            if !should_fetch {
                return Ok(None);
            }
            api::home(&t).await.map(Some)
        }
    });

    let mut loading_shelves = use_signal(Vec::<String>::new);
    let mut loading_row_pages = use_signal(Vec::<String>::new);
    let mut loading_more_rows = use_signal(|| false);

    use_effect(move || {
        if let Some(Ok(items)) = &*cont_res.read() {
            cont_items_state.set(items.clone());
            cache.with_mut(|ca| {
                ca.home.get_or_insert_with(HomeCache::default).continue_items = items.clone();
            });
        }
    });

    use_effect(move || {
        if let Some(Ok(Some(f))) = &*feed_res.read() {
            need_feed_fetch.set(false);
            feed_state.set(Some(f.clone()));
            loading_shelves.set(Vec::new());
            loading_row_pages.set(Vec::new());
            loading_more_rows.set(false);
            cache.with_mut(|ca| {
                ca.home.get_or_insert_with(HomeCache::default).feed = Some(f.clone());
            });
        }
    });

    let mut lazy_backoff_until_ms = use_signal(|| 0i64);
    let mut init_cat_loaded = use_signal(|| cat_already_has_rows);

    let load_batch = {
        let token = token.clone();
        move |limit: usize, cat: String| {
            let batch_ids: Vec<String> = {
                let Some(feed) = feed_state.peek().as_ref().cloned() else {
                    return;
                };
                let busy = loading_shelves.peek();
                feed.rows
                    .iter()
                    .filter(|r| !r.loaded && (cat == "for_you" || r.categories.iter().any(|c| c == &cat)))
                    .map(|r| r.id.clone())
                    .filter(|id| !busy.contains(id))
                    .take(limit)
                    .collect()
            };
            if batch_ids.is_empty() {
                return;
            }
            loading_shelves.with_mut(|v| {
                for id in &batch_ids {
                    if !v.contains(id) {
                        v.push(id.clone());
                    }
                }
            });
            loading_more_rows.set(true);
            let t = token.clone();
            spawn(async move {
                match api::home_shelves(&t, &batch_ids).await {
                    Ok(loaded) => {
                        lazy_backoff_until_ms.set(0);
                        feed_state.with_mut(|maybe| {
                            let Some(feed) = maybe.as_mut() else { return };
                            for id in &batch_ids {
                                if let Some(pos) = feed.rows.iter().position(|r| &r.id == id) {
                                    if let Some(fresh) = loaded.iter().find(|r| &r.id == id) {
                                        feed.rows[pos] = fresh.clone();
                                    } else {
                                        feed.rows[pos].loaded = true;
                                        feed.rows[pos].has_more = false;
                                    }
                                }
                            }
                            feed.rows.retain(|r| !r.loaded || !r.items.is_empty());
                        });
                        cache.with_mut(|ca| {
                            ca.home.get_or_insert_with(HomeCache::default).feed = feed_state.peek().clone();
                        });
                    }
                    Err(_) => {
                        // Cool down for 10s after a transient failure so scroll observers don't storm the server.
                        lazy_backoff_until_ms.set(platform::now_ms() + 10_000);
                    }
                }
                loading_shelves.with_mut(|v| v.retain(|id| !batch_ids.contains(id)));
                loading_more_rows.set(false);
            });
        }
    };

    // If restored category isn't "for_you", load its hidden shelves once after feed arrives.
    {
        let mut load_batch = load_batch.clone();
        use_effect(move || {
            let cat = active_category();
            let has_feed = feed_state.read().is_some();
            if has_feed && cat != "for_you" && !*init_cat_loaded.peek() {
                init_cat_loaded.set(true);
                load_batch(6, cat);
            }
        });
    }

    // Auto-load next deferred shelf batch when scrolling near the bottom sentinel.
    {
        let mut load_batch = load_batch.clone();
        use_hook(move || {
            struct HomeScrollGuard;
            impl Drop for HomeScrollGuard {
                fn drop(&mut self) {
                    let _ = dioxus::document::eval(
                        "if (window.__kinoHomeStop) { try { window.__kinoHomeStop(); } catch (e) {} window.__kinoHomeStop = null; }",
                    );
                }
            }
            spawn(async move {
                let mut bridge = dioxus::document::eval(
                    "if (window.__kinoHomeStop) { try { window.__kinoHomeStop(); } catch (e) {} }
                     var obs = null, mo = null, lastSent = 0, observedEl = null;
                     var emit = function() {
                       var now = Date.now();
                       if (now - lastSent > 500) {
                         lastSent = now;
                         dioxus.send(true);
                       }
                     };
                     var attach = function() {
                       var el = document.getElementById('home-lazy-sentinel');
                       if (el === observedEl) return;
                       if (obs && observedEl) obs.unobserve(observedEl);
                       observedEl = el;
                       if (obs && el) obs.observe(el);
                     };
                     if (typeof IntersectionObserver !== 'undefined') {
                       obs = new IntersectionObserver(function(entries) {
                         if (entries.some(function(e) { return e.isIntersecting; })) emit();
                       }, { rootMargin: '350px 0px' });
                     }
                     if (typeof MutationObserver !== 'undefined') {
                       mo = new MutationObserver(attach);
                       mo.observe(document.body, { childList: true, subtree: true });
                     }
                     attach();
                     window.__kinoHomeStop = function() {
                       if (obs) obs.disconnect();
                       if (mo) mo.disconnect();
                     };",
                );
                while let Ok(true) = bridge.recv::<bool>().await {
                    if !*loading_more_rows.peek() && platform::now_ms() >= *lazy_backoff_until_ms.peek() {
                        load_batch(HOME_LAZY_BATCH, active_category.peek().clone());
                    }
                }
            });
            std::rc::Rc::new(HomeScrollGuard)
        });
    }

    let load_more_items = {
        let token = token.clone();
        move |shelf_id: String| {
            let next_page = {
                let Some(feed) = feed_state.peek().as_ref().cloned() else {
                    return;
                };
                let Some(row) = feed.rows.iter().find(|r| r.id == shelf_id) else {
                    return;
                };
                if !row.loaded || !row.has_more || row.page >= MAX_ROW_AUTO_PAGES {
                    return;
                }
                if loading_row_pages.peek().contains(&shelf_id) {
                    return;
                }
                row.page + 1
            };
            loading_row_pages.with_mut(|v| v.push(shelf_id.clone()));
            let t = token.clone();
            let sid = shelf_id.clone();
            spawn(async move {
                if let Ok(page) = api::shelf_page(&t, &sid, next_page).await {
                    feed_state.with_mut(|maybe| {
                        let Some(feed) = maybe.as_mut() else { return };
                        let Some(row) = feed.rows.iter_mut().find(|r| r.id == sid) else {
                            return;
                        };
                        for item in page.items {
                            if !row
                                .items
                                .iter()
                                .any(|e| e.kind == item.kind && e.tmdb_id == item.tmdb_id)
                            {
                                row.items.push(item);
                            }
                        }
                        row.page = page.page;
                        row.has_more = page.has_more;
                    });
                    cache.with_mut(|ca| {
                        ca.home.get_or_insert_with(HomeCache::default).feed = feed_state.peek().clone();
                    });
                }
                loading_row_pages.with_mut(|v| v.retain(|id| id != &sid));
            });
        }
    };

    let cont_items = cont_items_state();
    let feed_err = match &*feed_res.read() {
        Some(Err(e)) => Some(e.message.clone()),
        _ => None,
    };
    let cont_err = match &*cont_res.read() {
        Some(Err(e)) => Some(e.message.clone()),
        _ => None,
    };
    let feed_for_view = match (feed_state(), &feed_err) {
        (Some(f), _) => Some(f),
        (None, Some(_)) if cont_items.is_empty() => return error_text(feed_err.as_deref().unwrap_or_default()),
        (None, Some(_)) => Some(HomeFeed {
            rows: Vec::new(),
            categories: Vec::new(),
            demo: false,
        }),
        (None, None) => None,
    };
    let t_drop = token.clone();
    let mut lb_cat = load_batch.clone();
    let mut lb_more = load_batch;
    rsx! {
        div { id: "home-active-marker", style: "display:none" }
        HomeView {
            feed: feed_for_view,
            continue_items: cont_items,
            active_category: active_category(),
            loading_shelves: loading_shelves(),
            loading_row_pages: loading_row_pages(),
            loading_more_rows: loading_more_rows(),
            on_open,
            on_open_title,
            // Plays at once from where the user stopped, without the title page in between.
            on_play: move |c: kino_core::ContinueItem| on_play.call((c.last.kind, c.last.tmdb_id)),
            on_drop: move |c: kino_core::ContinueItem| {
                let t = t_drop.clone();
                cache.with_mut(|ca| {
                    ca.patch_title(c.last.kind, c.last.tmdb_id, Some(TitleStatus::Dropped), None, None);
                });
                spawn(async move {
                    if api::set_status(&t, c.last.kind, c.last.tmdb_id, Some(TitleStatus::Dropped))
                        .await
                        .is_ok()
                    {
                        cont_res.restart();
                        need_feed_fetch.set(true);
                        feed_res.restart();
                    }
                });
            },
            on_category: move |cat: String| {
                telegram::haptic(telegram::Haptic::Select);
                active_category.set(cat.clone());
                lb_cat(6, cat);
            },
            on_load_more_items: load_more_items,
            on_open_shelf,
            on_load_more_rows: move |_| {
                if !*loading_more_rows.peek() {
                    lb_more(HOME_LAZY_BATCH, active_category.peek().clone());
                }
            },
            row_scrolls: initial_row_scrolls.clone(),
            on_row_scroll: move |(row_id, left): (String, f64)| {
                cache.with_mut(|ca| {
                    ca.home
                        .get_or_insert_with(HomeCache::default)
                        .row_scrolls
                        .insert(row_id, left);
                });
            },
            initial_cats_scroll,
            on_cats_scroll: move |left: f64| {
                cache.with_mut(|ca| {
                    ca.home.get_or_insert_with(HomeCache::default).cats_scroll = left;
                });
            },
        }
        if let Some(err) = feed_err {
            {error_text(&err)}
        }
        if let Some(err) = cont_err {
            {error_text(&err)}
        }
    }
}

#[component]
fn Shelf(
    token: String,
    id: String,
    title: String,
    subtitle: Option<String>,
    cache: Signal<AppCache>,
    on_open: EventHandler<TitleSummary>,
) -> Element {
    let cached = use_hook(|| cache.peek().shelves.get(&id).cloned());
    let has_cached = cached.is_some();
    let mut items = use_signal(|| cached.as_ref().map(|c| c.items.clone()).unwrap_or_default());
    let mut page = use_signal(|| cached.as_ref().map(|c| c.page).unwrap_or(0));
    let mut has_more = use_signal(|| cached.as_ref().map(|c| c.has_more).unwrap_or(true));
    let mut loading = use_signal(|| !has_cached);
    let mut err = use_signal(|| None::<String>);
    let mut shelf_title = use_signal(|| {
        cached
            .as_ref()
            .filter(|c| !c.title.is_empty())
            .map(|c| c.title.clone())
            .unwrap_or(title)
    });
    let mut shelf_sub = use_signal(|| cached.as_ref().and_then(|c| c.subtitle.clone()).or(subtitle));

    let mut fetch_page = {
        let token = token.clone();
        let id = id.clone();
        move |target_page: u32| {
            loading.set(true);
            err.set(None);
            let t = token.clone();
            let sid = id.clone();
            spawn(async move {
                match api::shelf_page(&t, &sid, target_page).await {
                    Ok(res) => {
                        if !res.title.is_empty() {
                            shelf_title.set(res.title);
                        }
                        if res.subtitle.is_some() {
                            shelf_sub.set(res.subtitle);
                        }
                        items.with_mut(|list| {
                            for item in res.items {
                                if !list.iter().any(|e| e.kind == item.kind && e.tmdb_id == item.tmdb_id) {
                                    list.push(item);
                                }
                            }
                        });
                        page.set(res.page);
                        has_more.set(res.has_more);
                        let snapshot = ShelfCache {
                            title: shelf_title.peek().clone(),
                            subtitle: shelf_sub.peek().clone(),
                            items: items.peek().clone(),
                            page: res.page,
                            has_more: res.has_more,
                        };
                        cache.with_mut(|ca| ca.insert_shelf(sid, snapshot));
                    }
                    Err(e) => err.set(Some(e.message)),
                }
                loading.set(false);
            });
        }
    };

    {
        let mut fp = fetch_page.clone();
        use_hook(move || {
            if !has_cached {
                fp(1);
            }
        });
    }

    if let Some(message) = err() {
        if items().is_empty() {
            return error_text(&message);
        }
    }

    rsx! {
        ShelfView {
            title: shelf_title(),
            subtitle: shelf_sub(),
            items: items(),
            has_more: has_more(),
            loading: loading(),
            on_open,
            on_load_more: move |_| {
                if !*loading.peek() && *has_more.peek() {
                    fetch_page(*page.peek() + 1);
                }
            },
        }
    }
}

#[component]
fn Search(
    token: String,
    query: Signal<String>,
    cache: Signal<AppCache>,
    on_open: EventHandler<TitleSummary>,
) -> Element {
    let initial_q = use_hook(|| query.peek().clone());
    let cached = use_hook(|| {
        cache
            .peek()
            .search
            .as_ref()
            .filter(|c| c.query == initial_q.trim())
            .cloned()
    });
    let initial_kind = cached.as_ref().map(|c| c.kind).unwrap_or_default();
    let mut shown_state = use_signal(|| cached.as_ref().and_then(|c| c.results.clone()));
    let mut debounced = use_signal(|| initial_q);
    let mut gen = use_signal(|| 0u64);
    use_effect(move || {
        let q = query();
        let next = *gen.peek() + 1;
        gen.set(next);
        if q.trim().is_empty() {
            debounced.set(q);
            shown_state.set(None);
            cache.with_mut(|c| c.search = None);
            return;
        }
        spawn(async move {
            platform::sleep(300).await;
            if *gen.peek() == next {
                debounced.set(q);
            }
        });
    });
    let results = use_resource(move || {
        let t = token.clone();
        let q = debounced();
        async move {
            if q.trim().is_empty() {
                return Ok(None);
            }
            api::search(&t, &q).await.map(Some)
        }
    });
    use_effect(move || {
        if let Some(Ok(Some(res))) = &*results.read() {
            shown_state.set(Some(res.clone()));
            let q_trimmed = debounced.peek().trim().to_owned();
            cache.with_mut(|c| {
                let kind = c
                    .search
                    .as_ref()
                    .filter(|prev| prev.query == q_trimmed)
                    .map(|prev| prev.kind)
                    .unwrap_or(initial_kind);
                c.search = Some(SearchCache {
                    query: q_trimmed,
                    results: Some(res.clone()),
                    kind,
                });
            });
        }
    });
    let shown = shown_state();
    let err = match &*results.read() {
        Some(Err(e)) if shown.is_none() => Some(e.message.clone()),
        _ => None,
    };
    rsx! {
        SearchView {
            results: shown,
            on_open,
            initial_kind,
            on_kind_change: move |next_kind| {
                let q_trimmed = query.peek().trim().to_owned();
                cache.with_mut(|c| {
                    if let Some(s) = c.search.as_mut() {
                        s.kind = next_kind;
                    } else if !q_trimmed.is_empty() {
                        c.search = Some(SearchCache {
                            query: q_trimmed,
                            results: shown_state.peek().clone(),
                            kind: next_kind,
                        });
                    }
                });
            },
        }
        if let Some(e) = err { {error_text(&e)} }
    }
}

#[component]
fn Mine(
    token: String,
    section: Signal<MySection>,
    filters: Signal<MyFilters>,
    cache: Signal<AppCache>,
    on_section: EventHandler<MySection>,
    on_filters: EventHandler<MyFilters>,
    on_open: EventHandler<TitleSummary>,
    on_open_id: EventHandler<(MediaKind, i64)>,
) -> Element {
    let cached = use_hook(|| cache.peek().mine.clone().unwrap_or_default());
    let mut selected_user_id = use_signal(|| cached.selected_user_id);
    let mut items_state = use_signal(|| cached.items.clone());
    let mut users_state = use_signal(|| cached.users.clone().unwrap_or_default());
    let mut calendar_state = use_signal(|| cached.calendar.clone().unwrap_or_default());
    let mut peer_libs = use_signal(|| cached.peer_libraries.clone());

    let t_items = token.clone();
    let items_res = use_resource(move || {
        let t = t_items.clone();
        async move { api::my(&t).await }
    });
    let t_users = token.clone();
    let users_res = use_resource(move || {
        let t = t_users.clone();
        async move { api::users(&t).await.unwrap_or_default() }
    });
    let t_cal = token.clone();
    let calendar_res = use_resource(move || {
        let t = t_cal.clone();
        async move { api::calendar(&t).await.unwrap_or_default() }
    });
    let t_peer = token.clone();
    let peer_res = use_resource(move || {
        let t = t_peer.clone();
        let uid = selected_user_id().or_else(|| users_state().first().map(|u| u.id));
        async move {
            match uid {
                Some(id) => Some(api::user_titles(&t, id).await),
                None => None,
            }
        }
    });

    use_effect(move || {
        if let Some(Ok(list)) = &*items_res.read() {
            items_state.set(Some(list.clone()));
            cache.with_mut(|c| c.mine.get_or_insert_with(MineCache::default).items = Some(list.clone()));
        }
    });
    use_effect(move || {
        if let Some(list) = &*users_res.read() {
            users_state.set(list.clone());
            cache.with_mut(|c| c.mine.get_or_insert_with(MineCache::default).users = Some(list.clone()));
        }
    });
    use_effect(move || {
        if let Some(list) = &*calendar_res.read() {
            calendar_state.set(list.clone());
            cache.with_mut(|c| c.mine.get_or_insert_with(MineCache::default).calendar = Some(list.clone()));
        }
    });
    use_effect(move || {
        if let Some(Some(Ok(lib))) = &*peer_res.read() {
            let uid = lib.profile.id;
            peer_libs.with_mut(|m| {
                m.insert(uid, lib.clone());
            });
            cache.with_mut(|c| {
                c.mine
                    .get_or_insert_with(MineCache::default)
                    .peer_libraries
                    .insert(uid, lib.clone());
            });
        }
    });

    let items_err = match &*items_res.read() {
        Some(Err(e)) => Some(e.message.clone()),
        _ => None,
    };
    let Some(items) = items_state() else {
        return match items_err {
            Some(msg) => error_text(&msg),
            None => loading(),
        };
    };
    let users = users_state();
    let active_uid = selected_user_id().or_else(|| users.first().map(|u| u.id));
    let peer_library = active_uid.and_then(|uid| peer_libs().get(&uid).cloned());
    let peer_error = match &*peer_res.read() {
        Some(Some(Err(e))) if peer_library.is_none() => Some(e.message.clone()),
        _ => None,
    };
    let peer_loading = active_uid.is_some() && peer_library.is_none() && peer_error.is_none() && !users.is_empty();
    let calendar = calendar_state();
    rsx! {
        MyView {
            items,
            filters: filters(),
            section: section(),
            users,
            selected_user_id: active_uid,
            peer_library,
            peer_loading,
            peer_error,
            calendar,
            today_ymd: today_ymd(),
            on_section,
            on_select_user: move |id| {
                selected_user_id.set(Some(id));
                cache.with_mut(|c| {
                    let m = c.mine.get_or_insert_with(MineCache::default);
                    m.selected_user_id = Some(id);
                    m.peer_status_filter = None;
                });
            },
            initial_peer_status: cached.peer_status_filter,
            on_peer_status: move |st| {
                cache.with_mut(|c| {
                    c.mine.get_or_insert_with(MineCache::default).peer_status_filter = st;
                });
            },
            initial_cal_month: cached.cal_month,
            on_cal_month: move |ym| {
                cache.with_mut(|c| {
                    c.mine.get_or_insert_with(MineCache::default).cal_month = ym;
                });
            },
            initial_cal_day: cached.cal_day.clone(),
            on_cal_day: move |day| {
                cache.with_mut(|c| {
                    c.mine.get_or_insert_with(MineCache::default).cal_day = day;
                });
            },
            on_open_id: Some(on_open_id),
            on_filters,
            on_open,
        }
    }
}

async fn join2<A, B, FA, FB>(fa: FA, fb: FB) -> (A, B)
where
    FA: std::future::Future<Output = A>,
    FB: std::future::Future<Output = B>,
{
    let mut fa = std::pin::pin!(fa);
    let mut fb = std::pin::pin!(fb);
    let (mut ra, mut rb) = (None, None);
    std::future::poll_fn(|cx| {
        if ra.is_none() {
            if let std::task::Poll::Ready(v) = fa.as_mut().poll(cx) {
                ra = Some(v);
            }
        }
        if rb.is_none() {
            if let std::task::Poll::Ready(v) = fb.as_mut().poll(cx) {
                rb = Some(v);
            }
        }
        if ra.is_some() && rb.is_some() {
            std::task::Poll::Ready((ra.take().unwrap(), rb.take().unwrap()))
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}

async fn join3<A, B, C, FA, FB, FC>(fa: FA, fb: FB, fc: FC) -> (A, B, C)
where
    FA: std::future::Future<Output = A>,
    FB: std::future::Future<Output = B>,
    FC: std::future::Future<Output = C>,
{
    let mut fa = std::pin::pin!(fa);
    let mut fb = std::pin::pin!(fb);
    let mut fc = std::pin::pin!(fc);
    let (mut ra, mut rb, mut rc) = (None, None, None);
    std::future::poll_fn(|cx| {
        if ra.is_none() {
            if let std::task::Poll::Ready(v) = fa.as_mut().poll(cx) {
                ra = Some(v);
            }
        }
        if rb.is_none() {
            if let std::task::Poll::Ready(v) = fb.as_mut().poll(cx) {
                rb = Some(v);
            }
        }
        if rc.is_none() {
            if let std::task::Poll::Ready(v) = fc.as_mut().poll(cx) {
                rc = Some(v);
            }
        }
        if ra.is_some() && rb.is_some() && rc.is_some() {
            std::task::Poll::Ready((ra.take().unwrap(), rb.take().unwrap(), rc.take().unwrap()))
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}

async fn join4<A, B, C, D, FA, FB, FC, FD>(fa: FA, fb: FB, fc: FC, fd: FD) -> (A, B, C, D)
where
    FA: std::future::Future<Output = A>,
    FB: std::future::Future<Output = B>,
    FC: std::future::Future<Output = C>,
    FD: std::future::Future<Output = D>,
{
    let mut fa = std::pin::pin!(fa);
    let mut fb = std::pin::pin!(fb);
    let mut fc = std::pin::pin!(fc);
    let mut fd = std::pin::pin!(fd);
    let (mut ra, mut rb, mut rc, mut rd) = (None, None, None, None);
    std::future::poll_fn(|cx| {
        if ra.is_none() {
            if let std::task::Poll::Ready(v) = fa.as_mut().poll(cx) {
                ra = Some(v);
            }
        }
        if rb.is_none() {
            if let std::task::Poll::Ready(v) = fb.as_mut().poll(cx) {
                rb = Some(v);
            }
        }
        if rc.is_none() {
            if let std::task::Poll::Ready(v) = fc.as_mut().poll(cx) {
                rc = Some(v);
            }
        }
        if rd.is_none() {
            if let std::task::Poll::Ready(v) = fd.as_mut().poll(cx) {
                rd = Some(v);
            }
        }
        if ra.is_some() && rb.is_some() && rc.is_some() && rd.is_some() {
            std::task::Poll::Ready((
                ra.take().unwrap(),
                rb.take().unwrap(),
                rc.take().unwrap(),
                rd.take().unwrap(),
            ))
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}

#[component]
fn Profile(
    token: String,
    name: String,
    avatar_url: Option<String>,
    theme: Theme,
    cache: Signal<AppCache>,
    #[props(default)] app_version: Option<String>,
    #[props(default)] update_state: Option<AppUpdateUi>,
    #[props(default)] on_check_update: Option<EventHandler<()>>,
    #[props(default)] on_install_update: Option<EventHandler<()>>,
    on_theme: EventHandler<Theme>,
    on_preloads_changed: EventHandler<()>,
    on_open: EventHandler<(MediaKind, i64)>,
    on_logout: EventHandler<()>,
) -> Element {
    let cached = use_hook(|| cache.peek().profile.clone().unwrap_or_default());
    let mut history_state = use_signal(|| cached.history.clone());
    let mut preloads_state = use_signal(|| cached.preloads.clone());
    let mut prefs_state = use_signal(|| cached.prefs.clone());
    let t1 = token.clone();
    let mut data = use_resource(move || {
        let t = t1.clone();
        async move {
            let (history, preloads, prefs) = join3(api::history(&t), api::preloads(&t), api::prefs(&t)).await;
            Ok::<_, api::Failure>((history?, preloads?, prefs?))
        }
    });
    use_effect(move || {
        if let Some(Ok((h, pl, pr))) = &*data.read() {
            history_state.set(Some(h.clone()));
            preloads_state.set(Some(pl.clone()));
            prefs_state.set(Some(pr.clone()));
            cache.with_mut(|c| {
                let p = c.profile.get_or_insert_with(ProfileCache::default);
                p.history = Some(h.clone());
                p.preloads = Some(pl.clone());
                p.prefs = Some(pr.clone());
            });
        }
    });
    let mut prefs_now = use_signal(|| None::<UserPrefs>);
    let mut dl_error = use_signal(|| None::<String>);
    let state = data.read().clone();
    let active_preloads_list = preloads_state();
    let has_active_dl = active_preloads_list.as_ref().is_some_and(|pl| {
        pl.iter()
            .any(|p| matches!(p.state, PreloadState::Queued | PreloadState::Downloading))
    });
    let mut poll_gen = use_signal(|| 0u64);
    use_effect(use_reactive!(|has_active_dl| {
        let gen = *poll_gen.peek() + 1;
        poll_gen.set(gen);
        if has_active_dl {
            spawn(async move {
                loop {
                    platform::sleep(3_000).await;
                    if *poll_gen.peek() != gen {
                        break;
                    }
                    data.restart();
                    on_preloads_changed.call(());
                }
            });
        }
    }));
    use_drop(move || {
        let next = *poll_gen.peek() + 1;
        poll_gen.set(next);
    });
    let (Some(history), Some(preloads), Some(prefs)) = (history_state(), active_preloads_list, prefs_state()) else {
        return match state {
            Some(Err(e)) => error_text(&e.message),
            _ => loading(),
        };
    };
    let (now, utc_offset_min) = platform::now();
    let t2 = token.clone();
    let t3 = token.clone();
    rsx! {
        ProfileView {
            name,
            avatar_url,
            history,
            preloads,
            prefs: prefs_now().unwrap_or(prefs),
            theme,
            in_telegram: telegram::is_telegram(),
            app_version,
            update_state,
            on_check_update,
            on_install_update,
            now,
            utc_offset_min,
            on_theme,
            on_prefs: move |p: UserPrefs| {
                // The studio is no longer chosen here, so an old one must not linger unseen.
                let p = UserPrefs { voice: None, ..p };
                prefs_now.set(Some(p.clone()));
                prefs_state.set(Some(p.clone()));
                cache.with_mut(|c| {
                    c.profile.get_or_insert_with(ProfileCache::default).prefs = Some(p.clone());
                });
                let t = token.clone();
                spawn(async move {
                    let _ = api::save_prefs(&t, &p).await;
                });
            },
            on_delete_preload: move |pid: i64| {
                dl_error.set(None);
                let t = t2.clone();
                spawn(async move {
                    match api::delete_preload(&t, pid).await {
                        Ok(()) => {
                            data.restart();
                            on_preloads_changed.call(());
                        }
                        Err(e) => {
                            dl_error.set(Some(format!("Не удалось удалить загрузку: {}", e.message)));
                        }
                    }
                });
            },
            on_delete_preload_episode: {
                let t_ep = t3.clone();
                move |(pid, s_num, ep_num): (i64, i32, i32)| {
                    dl_error.set(None);
                    let t = t_ep.clone();
                    spawn(async move {
                        match api::delete_preload_episode(&t, pid, s_num, ep_num).await {
                            Ok(()) => {
                                data.restart();
                                on_preloads_changed.call(());
                            }
                            Err(e) => {
                                dl_error.set(Some(format!("Не удалось удалить серию: {}", e.message)));
                            }
                        }
                    });
                }
            },
            dl_error: dl_error(),
            on_refresh: move |_| {
                dl_error.set(None);
                data.restart();
                on_preloads_changed.call(());
            },
            initial_prefs_open: cached.prefs_open,
            on_prefs_open: move |open| {
                cache.with_mut(|c| {
                    c.profile.get_or_insert_with(ProfileCache::default).prefs_open = open;
                });
            },
            initial_hist_limit: cached.hist_limit,
            on_hist_limit: move |limit| {
                cache.with_mut(|c| {
                    c.profile.get_or_insert_with(ProfileCache::default).hist_limit = Some(limit);
                });
            },
            initial_expanded_series: cached.expanded_series.clone(),
            on_toggle_series: move |tid: i64| {
                cache.with_mut(|c| {
                    let list = &mut c.profile.get_or_insert_with(ProfileCache::default).expanded_series;
                    if let Some(pos) = list.iter().position(|&x| x == tid) {
                        list.remove(pos);
                    } else {
                        list.push(tid);
                    }
                });
            },
            on_open,
            on_logout,
        }
    }
}

/// Short label of a remembered release for the title page.
fn choice_label(c: &ReleaseChoiceUpdate) -> String {
    match (&c.voice, &c.quality) {
        (Some(v), Some(q)) => format!("{v} · {q}"),
        _ => c.title.clone().unwrap_or_else(|| "выбрана".into()),
    }
}

fn today_ymd() -> String {
    let (now, offset_min) = platform::now();
    let tz = chrono::FixedOffset::east_opt(offset_min * 60)
        .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC"));
    now.with_timezone(&tz).format("%Y-%m-%d").to_string()
}

fn scoped_choice(
    choices: &[ReleaseChoiceUpdate],
    kind: MediaKind,
    season: i32,
    episode: i32,
    want_q: Option<&str>,
) -> Option<ReleaseChoiceUpdate> {
    let s = if kind == MediaKind::Tv { season } else { 0 };
    let e = if kind == MediaKind::Tv { episode } else { 0 };
    let matches_q = |c: &&ReleaseChoiceUpdate| -> bool {
        let Some(q) = want_q else { return true };
        let k = c
            .quality_key
            .as_deref()
            .or(c.quality.as_deref())
            .map(normalize_quality_key)
            .unwrap_or("");
        k == q
    };
    choices
        .iter()
        .filter(matches_q)
        .find(|c| c.season == s && c.episode == e)
        .or_else(|| {
            choices
                .iter()
                .filter(matches_q)
                .find(|c| c.season == s && c.episode == 0)
        })
        .or_else(|| {
            choices
                .iter()
                .filter(matches_q)
                .find(|c| c.season == 0 && c.episode == 0)
        })
        .cloned()
}

fn auto_play_target_for(
    kind: MediaKind,
    details: &TitleDetails,
    progress: &[Progress],
    auto_season: Option<i32>,
    default_target: &PlayTarget,
) -> PlayTarget {
    let Some(s) = auto_season.filter(|n| kind == MediaKind::Tv && *n > 0) else {
        return default_target.clone();
    };
    if default_target.season == s {
        return default_target.clone();
    }
    if let Some(last) = progress.iter().filter(|p| p.season == s).max_by_key(|p| p.updated_at) {
        if !last.finished {
            return PlayTarget {
                season: s,
                episode: last.episode,
                position_sec: last.position_sec,
            };
        }
        let next_ep = details
            .seasons
            .iter()
            .find(|sec| sec.number == s)
            .filter(|sec| last.episode < sec.episode_count)
            .map(|_| last.episode + 1)
            .filter(|ep| !matches!(details.last_episode, Some((ls, le)) if ls == s && *ep > le))
            .unwrap_or(1);
        return PlayTarget {
            season: s,
            episode: next_ep,
            position_sec: 0.0,
        };
    }
    PlayTarget {
        season: s,
        episode: 1,
        position_sec: 0.0,
    }
}

fn auto_release_choice_for(
    kind: MediaKind,
    tmdb_id: i64,
    season: i32,
    hash: &str,
    preloads: &[PreloadItem],
    manual_choices: &[ReleaseChoiceUpdate],
    peer_choices: &[ReleaseChoiceUpdate],
) -> ReleaseChoiceUpdate {
    let season_key = if kind == MediaKind::Tv { season } else { 0 };
    if let Some(p) = preloads
        .iter()
        .filter(|p| p.kind == kind && p.tmdb_id == tmdb_id && p.state != PreloadState::Deleted)
        .filter(|p| p.infohash.eq_ignore_ascii_case(hash))
        .max_by_key(|p| i32::from(p.season == season_key))
    {
        return ReleaseChoiceUpdate {
            kind,
            tmdb_id,
            season: if kind == MediaKind::Tv { p.season } else { 0 },
            episode: 0,
            infohash: p.infohash.clone(),
            title: p.title.clone().filter(|s| !s.is_empty()),
            quality: p.quality.clone(),
            quality_key: p.quality.as_deref().map(|q| normalize_quality_key(q).to_owned()),
            voice: p.voice.clone(),
        };
    }
    if let Some(c) = manual_choices
        .iter()
        .chain(peer_choices.iter())
        .find(|c| c.infohash.eq_ignore_ascii_case(hash))
    {
        return c.clone();
    }
    ReleaseChoiceUpdate {
        kind,
        tmdb_id,
        season: season_key,
        episode: 0,
        infohash: hash.to_owned(),
        title: None,
        quality: None,
        quality_key: None,
        voice: None,
    }
}

#[component]
fn Title(
    token: String,
    kind: MediaKind,
    id: i64,
    autoplay: bool,
    #[props(default)] auto_season: Option<i32>,
    #[props(default)] auto_release: Option<String>,
    cache: Signal<AppCache>,
    on_preloads_changed: EventHandler<()>,
    on_open: EventHandler<TitleSummary>,
    on_person: EventHandler<i64>,
    on_exit: EventHandler<bool>,
) -> Element {
    let cached_title = use_hook(|| cache.peek().titles.get(&(kind, id)).cloned());
    let mut data_state = use_signal(|| {
        cached_title.as_ref().map(|c| {
            (
                c.details.clone(),
                c.progress.clone(),
                c.state.clone(),
                c.choices.clone(),
            )
        })
    });
    let mut extras_state = use_signal(|| cached_title.as_ref().and_then(|c| c.extras.clone()));
    let mut chosen_season = use_signal(|| auto_season.or_else(|| cached_title.as_ref().and_then(|c| c.chosen_season)));
    let mut auto_preloads = use_signal(|| None::<Vec<PreloadItem>>);
    let mut title_preloads = use_signal(Vec::<kino_core::PreloadItem>::new);
    let mut season_state = use_signal(|| {
        cached_title.as_ref().and_then(|c| {
            let last = c.progress.iter().max_by_key(|p| p.updated_at).map(|p| p.season);
            let default_n = last.or_else(|| c.details.seasons.iter().map(|s| s.number).find(|n| *n > 0));
            c.chosen_season.or(default_n).and_then(|n| c.seasons.get(&n).cloned())
        })
    });
    let has_auto_release = autoplay && auto_release.is_some();
    let t1 = token.clone();
    let mut data = use_resource(move || {
        let t = t1.clone();
        async move {
            let fetch_title = async {
                let (details, progress, st, choices) = join4(
                    api::details(&t, kind, id),
                    api::title_progress(&t, kind, id),
                    api::title_state(&t, kind, id),
                    api::release_choices(&t, kind, id),
                )
                .await;
                Ok::<_, api::Failure>((details?, progress?, st?, choices?))
            };
            if has_auto_release {
                let (title_res, pre_res) = join2(fetch_title, api::preloads(&t)).await;
                let pre_items = pre_res.ok().map(|items| {
                    items
                        .into_iter()
                        .filter(|p| p.kind == kind && p.tmdb_id == id && p.state != PreloadState::Deleted)
                        .collect::<Vec<_>>()
                });
                Ok::<_, api::Failure>((title_res?, pre_items))
            } else {
                Ok::<_, api::Failure>((fetch_title.await?, None))
            }
        }
    });
    // Cast and suggestions come apart from the title: if TMDB is slow or fails, the page still works.
    let t_extras = token.clone();
    let extras_res = use_resource(move || {
        let t = t_extras.clone();
        async move { api::extras(&t, kind, id).await.ok() }
    });
    let t2 = token.clone();
    let season_res = use_resource(move || {
        let t = t2.clone();
        let picked = chosen_season();
        // Default: the season of the last watched episode, else the first one.
        let default = match data_state() {
            Some((d, p, _, _)) if kind == MediaKind::Tv => {
                let last = p.iter().max_by_key(|p| p.updated_at).map(|p| p.season);
                last.or_else(|| d.seasons.iter().map(|s| s.number).find(|n| *n > 0))
            }
            _ => None,
        };
        async move {
            match picked.or(default) {
                Some(n) => api::season(&t, id, n).await.ok(),
                None => None,
            }
        }
    });
    use_effect(move || {
        if let Some(Ok((v, pre_opt))) = &*data.read() {
            data_state.set(Some(v.clone()));
            if let Some(pre) = pre_opt {
                auto_preloads.set(Some(pre.clone()));
                if *title_preloads.peek() != *pre {
                    title_preloads.set(pre.clone());
                }
            } else if has_auto_release && auto_preloads.peek().is_none() {
                auto_preloads.set(Some(Vec::new()));
            }
            let user_r = v.2.effective_rating.or_else(|| v.2.rating.map(f32::from));
            let is_pre = v.0.summary.preloaded || v.2.preloaded_ready_at.is_some();
            cache.with_mut(|c| {
                let prev = c.titles.get(&(kind, id)).cloned();
                c.upsert_title(
                    (kind, id),
                    TitleCache {
                        details: v.0.clone(),
                        progress: v.1.clone(),
                        state: v.2.clone(),
                        choices: v.3.clone(),
                        extras: extras_state
                            .peek()
                            .clone()
                            .or_else(|| prev.as_ref().and_then(|p| p.extras.clone())),
                        chosen_season: *chosen_season.peek(),
                        seasons: prev.map(|p| p.seasons).unwrap_or_default(),
                    },
                );
                c.patch_title(kind, id, v.2.status, Some(user_r), Some(is_pre));
            });
        }
    });
    use_effect(move || {
        if let Some(Some(ex)) = &*extras_res.read() {
            extras_state.set(Some(ex.clone()));
            cache.with_mut(|c| {
                if let Some(entry) = c.titles.get_mut(&(kind, id)) {
                    entry.extras = Some(ex.clone());
                }
            });
        }
    });
    use_effect(move || {
        if let Some(Some(s)) = &*season_res.read() {
            season_state.set(Some(s.clone()));
            cache.with_mut(|c| {
                if let Some(entry) = c.titles.get_mut(&(kind, id)) {
                    entry.seasons.insert(s.number, s.clone());
                }
            });
        }
    });
    // Optimistic edits; cleared when the data reloads.
    let mut status_now = use_signal(|| None::<Option<TitleStatus>>);
    let mut rating_now = use_signal(|| None::<Option<i16>>);
    let mut scale_now = use_signal(|| None::<VideoScale>);
    let mut rate_seq = use_signal(|| 0u32);
    let mut notice = use_signal(|| None::<String>);
    // Choices made in this visit, newest first; they override the loaded ones.
    let mut picked_now = use_signal(Vec::<ReleaseChoiceUpdate>::new);
    let mut picker = use_signal(|| None::<PlayTarget>);
    let mut picker_ep_scoped = use_signal(|| false);
    let mut picker_download_only = use_signal(|| false);
    let mut picker_note_override = use_signal(|| None::<String>);
    let mut downloading_ep = use_signal(|| None::<(i32, i32)>);
    let mut picker_preloaded = use_signal(|| None::<(i32, kino_core::ReleaseList)>);
    let mut resolving = use_signal(|| false);
    let mut resolve_gen = use_signal(|| 0u32);
    let mut hash_rebinds = use_signal(Vec::<(String, String)>::new);
    let mut title_poll_running = use_signal(|| false);
    let mut title_poll_gen = use_signal(|| 0u32);
    let refresh_title_preloads = {
        let tok = token.clone();
        move || {
            let t = tok.clone();
            spawn(async move {
                let Ok(items) = api::preloads(&t).await else {
                    return;
                };
                let filtered: Vec<kino_core::PreloadItem> = items
                    .into_iter()
                    .filter(|p| p.kind == kind && p.tmdb_id == id && p.state != kino_core::PreloadState::Deleted)
                    .collect();
                if *title_preloads.peek() != filtered {
                    title_preloads.set(filtered);
                }
            });
        }
    };
    use_hook({
        let refresh = refresh_title_preloads.clone();
        move || {
            if !has_auto_release {
                refresh();
            }
        }
    });
    let mut filter = use_signal(ReleaseFilter::default);
    // The user's default filters are applied when the picker opens; if they leave
    // nothing, the list is shown without them (once the releases have loaded).
    let mut defaults_pending = use_signal(|| false);
    let mut defaults_relaxed = use_signal(|| false);
    let mut prefs_applied = use_signal(|| false);
    let t_prefs = token.clone();
    let user_prefs = use_resource(move || {
        let t = t_prefs.clone();
        async move { api::prefs(&t).await.ok() }
    });
    let mut sort = use_signal(ReleaseSort::default);
    let mut panel = use_signal(|| None::<PickerPanel>);
    let mut playing = use_signal(|| None::<(PlayTarget, ReleaseChoiceUpdate, bool, bool, RewatchMode)>);
    let mut auto = use_signal(|| autoplay);
    let mut exit_on_close = use_signal(|| autoplay);
    let mut overlay_pushed = use_signal(|| false);
    let mut notice_pushed = use_signal(|| false);
    let mut mismatch_prompt = use_signal(|| {
        None::<(
            PlayTarget,
            RewatchMode,
            ReleaseChoiceUpdate,
            String,
            String,
            ResolveReleaseResponse,
        )>
    });
    let mut mismatch_pushed = use_signal(|| false);
    struct TitleKeyGuard;
    impl Drop for TitleKeyGuard {
        fn drop(&mut self) {
            let _ = dioxus::document::eval(
                "if (window.__kinoTitleKeysStop) { try { window.__kinoTitleKeysStop(); } catch (e) {} }",
            );
        }
    }
    use_hook(move || {
        spawn(async move {
            let mut bridge = dioxus::document::eval(
                "if (window.__kinoTitleKeysStop) { try { window.__kinoTitleKeysStop(); } catch (e) {} }
                 var handler = function(e) {
                   if (e.key === 'Escape') { dioxus.send(1); }
                 };
                 window.addEventListener('keydown', handler);
                 window.__kinoTitleKeysStop = function() {
                   window.removeEventListener('keydown', handler);
                   delete window.__kinoTitleKeysStop;
                 };",
            );
            while bridge.recv::<i32>().await.is_ok() {
                if playing.peek().is_some() {
                    continue;
                }
                if notice.peek().is_some() {
                    notice.set(None);
                    if *notice_pushed.peek() {
                        notice_pushed.set(false);
                        telegram::pop_history();
                    }
                } else if mismatch_prompt.peek().is_some() {
                    mismatch_prompt.set(None);
                    if *mismatch_pushed.peek() {
                        mismatch_pushed.set(false);
                        telegram::pop_history();
                    }
                } else if *resolving.peek() {
                    let next = *resolve_gen.peek() + 1;
                    resolve_gen.set(next);
                    resolving.set(false);
                } else if picker.peek().is_some() {
                    picker.set(None);
                    picker_preloaded.set(None);
                    picker_ep_scoped.set(false);
                    picker_download_only.set(false);
                    picker_note_override.set(None);
                    if *overlay_pushed.peek() {
                        overlay_pushed.set(false);
                        telegram::pop_history();
                    }
                }
            }
        });
        std::rc::Rc::new(TitleKeyGuard)
    });
    let t0 = token.clone();
    // Releases for the open picker, loaded from the indexer (or demo data).
    let releases = use_resource(move || {
        let t = t0.clone();
        let open = picker();
        async move {
            let p = open?;
            let season = if kind == MediaKind::Tv { p.season } else { 0 };
            if let Some((ps, list)) = picker_preloaded.peek().clone() {
                if ps == season {
                    return Some(Ok(list));
                }
            }
            Some(api::releases(&t, kind, id, season).await)
        }
    });

    // Once the releases (and user prefs) are in: relax secondary filters if they
    // leave nothing, while always preserving the user's chosen audio language.
    use_effect(move || {
        if picker.read().is_none() {
            return;
        }
        if !*prefs_applied.peek() {
            if let Some(Some(p)) = user_prefs.read().clone() {
                prefs_applied.set(true);
                let mut updated = filter.peek().clone().or(ReleaseFilter::from_prefs(&p));
                if cfg!(feature = "mobile") {
                    updated.direct_play = None;
                }
                filter.set(updated);
                sort.set(ReleaseSort::from_key(p.sort.as_deref()));
                defaults_pending.set(true);
                defaults_relaxed.set(false);
            }
        }
        let Some(Ok(list)) = releases.read().clone().flatten() else {
            return;
        };
        if !*defaults_pending.peek() {
            return;
        }
        defaults_pending.set(false);
        let f = filter.peek().clone();
        if list.releases.is_empty() || list.releases.iter().any(|r| f.matches(r)) {
            return;
        }
        let season = (kind == MediaKind::Tv)
            .then(|| picker.peek().as_ref().map(|t| t.season))
            .flatten();
        let episode = (kind == MediaKind::Tv).then_some(f.episode).flatten();
        if f.language.is_some() {
            if episode.is_some() {
                let lang_season_ep = ReleaseFilter {
                    language: f.language.clone(),
                    season,
                    episode,
                    ..ReleaseFilter::default()
                };
                if list.releases.iter().any(|r| lang_season_ep.matches(r)) {
                    filter.set(lang_season_ep);
                    defaults_relaxed.set(true);
                    return;
                }
            }
            let lang_season = ReleaseFilter {
                language: f.language.clone(),
                season,
                ..ReleaseFilter::default()
            };
            if list.releases.iter().any(|r| lang_season.matches(r)) {
                filter.set(lang_season);
                defaults_relaxed.set(true);
                return;
            }
            let just_lang = ReleaseFilter {
                language: f.language.clone(),
                ..ReleaseFilter::default()
            };
            if season.is_some() && list.releases.iter().any(|r| just_lang.matches(r)) {
                filter.set(just_lang);
                defaults_relaxed.set(true);
                return;
            }
            // Keep the chosen language (and requested episode) active even when 0 releases match.
            filter.set(ReleaseFilter {
                language: f.language.clone(),
                season,
                episode,
                ..ReleaseFilter::default()
            });
            return;
        }
        if episode.is_some() {
            let season_ep = ReleaseFilter {
                season,
                episode,
                ..ReleaseFilter::default()
            };
            if list.releases.iter().any(|r| season_ep.matches(r)) {
                filter.set(season_ep);
                defaults_relaxed.set(true);
                return;
            }
        }
        let season_only = ReleaseFilter {
            season,
            ..ReleaseFilter::default()
        };
        let relaxed = if list.releases.iter().any(|r| season_only.matches(r)) {
            season_only
        } else {
            ReleaseFilter::default()
        };
        filter.set(relaxed);
        defaults_relaxed.set(true);
    });

    let (details, progress, st, loaded, fresh_preloads) = if auto() && has_auto_release && auto_preloads().is_none() {
        match data.read().clone() {
            Some(Ok((v, pre_opt))) => (v.0, v.1, v.2, v.3, pre_opt),
            Some(Err(e)) => match data_state() {
                Some(v) => (v.0, v.1, v.2, v.3, None),
                None => return error_text(&e.message),
            },
            None => return loading(),
        }
    } else {
        match data_state() {
            Some(v) => (v.0, v.1, v.2, v.3, auto_preloads()),
            None => match data.read().clone() {
                None => return loading(),
                Some(Err(e)) => return error_text(&e.message),
                Some(Ok((v, pre_opt))) => (v.0, v.1, v.2, v.3, pre_opt),
            },
        }
    };
    let status = status_now().unwrap_or(st.status);
    let my_rating = rating_now().unwrap_or(st.rating);
    let mut manual_choices: Vec<ReleaseChoiceUpdate> = picked_now();
    for c in &loaded {
        if c.picked_by.is_none() && c.manual {
            manual_choices.push(c.release.clone());
        }
    }
    let peer_choices: Vec<ReleaseChoiceUpdate> = loaded
        .iter()
        .filter(|c| c.picked_by.is_some())
        .map(|c| c.release.clone())
        .collect();
    let season_key = move |t: &PlayTarget| if kind == MediaKind::Tv { t.season } else { 0 };
    let loaded_prefs_opt = user_prefs.read().clone().flatten();
    let is_mobile = telegram::is_mobile();
    let dev_pref_q = loaded_prefs_opt.as_ref().and_then(|p| {
        if is_mobile {
            p.mobile_quality.as_deref()
        } else {
            p.desktop_quality.as_deref()
        }
    });
    let ask_each_time = dev_pref_q == Some("ask");
    let display_choice_for = {
        let manual_choices = manual_choices.clone();
        let peer_choices = peer_choices.clone();
        let dev_pref_q = dev_pref_q.map(str::to_owned);
        move |s: i32, e: i32| -> Option<ReleaseChoiceUpdate> {
            if ask_each_time {
                return scoped_choice(&picked_now(), kind, s, e, None);
            }
            let want_fn = |list: &[ReleaseChoiceUpdate]| -> Option<ReleaseChoiceUpdate> {
                if is_mobile && matches!(dev_pref_q.as_deref(), None | Some("auto")) {
                    scoped_choice(list, kind, s, e, Some("720p"))
                        .or_else(|| scoped_choice(list, kind, s, e, Some("SD")))
                } else {
                    let want = dev_pref_q.as_deref().map(normalize_quality_key).unwrap_or("1080p");
                    scoped_choice(list, kind, s, e, Some(want))
                }
            };
            want_fn(&manual_choices)
                .or_else(|| want_fn(&peer_choices))
                .or_else(|| scoped_choice(&manual_choices, kind, s, e, None))
                .or_else(|| scoped_choice(&peer_choices, kind, s, e, None))
        }
    };
    let latest = manual_choices.first().or_else(|| peer_choices.first()).cloned();
    let (target, play_label) = play_target(&details, &progress);
    let default_target = target.clone();
    let manual_after_preload = st.preloaded_ready_at.is_some_and(|rt| {
        loaded.iter().any(|c| {
            c.picked_by.is_none()
                && c.manual
                && c.chosen_at > rt
                && (kind == MediaKind::Movie
                    || (c.release.season == target.season
                        && (c.release.episode == target.episode || c.release.episode == 0))
                    || (c.release.season == 0 && c.release.episode == 0))
        })
    });
    let target_preloaded = {
        let cur_preloads = title_preloads();
        if cur_preloads.is_empty() {
            details.summary.preloaded
        } else {
            cur_preloads
                .iter()
                .any(|p| p.is_ready_for(kind, target.season, target.episode))
        }
    };
    let release_label = if target_preloaded && picked_now().is_empty() && !manual_after_preload {
        Some("На сервере · готово".to_owned())
    } else {
        display_choice_for(target.season, target.episode).map(|c| choice_label(&c))
    };
    let cur_season_data = season_state();
    let season_release_label = cur_season_data.as_ref().and_then(|sd| {
        manual_choices
            .iter()
            .chain(peer_choices.iter())
            .find(|c| c.season == sd.number && c.episode == 0)
            .map(|c| format!("Раздача: {}", choice_label(c)))
    });
    let watched: Vec<String> = progress
        .iter()
        .filter_map(|p| p.release.as_ref().map(|r| r.infohash.clone()))
        .collect();

    let mut open_picker_with_scope = {
        let latest = latest.clone();
        let manual_choices = manual_choices.clone();
        let loaded_prefs_opt = loaded_prefs_opt.clone();
        move |t: PlayTarget, ep_scoped: bool, filter_ep: bool| {
            let base = match (scoped_choice(&manual_choices, kind, t.season, t.episode, None), &latest) {
                (None, Some(prev)) => ReleaseFilter::like(prev),
                _ => ReleaseFilter::default(),
            };
            prefs_applied.set(loaded_prefs_opt.is_some());
            let prefs_now = loaded_prefs_opt.clone().unwrap_or_default();
            let mut f = base.or(ReleaseFilter::from_prefs(&prefs_now));
            if cfg!(feature = "mobile") {
                f.direct_play = None;
            }
            let f = if kind == MediaKind::Tv {
                let f = f.for_season(t.season);
                if filter_ep && t.episode > 0 {
                    f.for_episode(t.episode)
                } else {
                    f
                }
            } else {
                f
            };
            picker_ep_scoped.set(ep_scoped);
            filter.set(f);
            sort.set(ReleaseSort::from_key(prefs_now.sort.as_deref()));
            defaults_pending.set(true);
            defaults_relaxed.set(false);
            panel.set(None);
            exit_on_close.set(false);
            if picker.peek().is_none() && !*overlay_pushed.peek() {
                overlay_pushed.set(true);
                telegram::push_history(None);
            }
            picker.set(Some(t));
        }
    };
    let open_picker = {
        let mut open_picker_with_scope = open_picker_with_scope.clone();
        move |t: PlayTarget| {
            picker_preloaded.set(None);
            picker_download_only.set(false);
            picker_note_override.set(None);
            let filter_ep = t.episode > 0;
            open_picker_with_scope(t, false, filter_ep);
        }
    };
    let open_season_picker = {
        let mut open_picker_with_scope = open_picker_with_scope.clone();
        move |t: PlayTarget| {
            picker_preloaded.set(None);
            picker_download_only.set(false);
            picker_note_override.set(None);
            open_picker_with_scope(t, false, false);
        }
    };
    let open_episode_picker = {
        let mut open_picker_with_scope = open_picker_with_scope.clone();
        move |t: PlayTarget| {
            picker_preloaded.set(None);
            picker_download_only.set(false);
            picker_note_override.set(None);
            let filter_ep = t.episode > 0;
            open_picker_with_scope(t, true, filter_ep);
        }
    };
    let open_episode_download_picker = {
        let mut open_picker_with_scope = open_picker_with_scope.clone();
        move |t: PlayTarget, note_msg: Option<String>| {
            picker_preloaded.set(None);
            picker_download_only.set(true);
            picker_note_override.set(note_msg);
            let filter_ep = t.episode > 0;
            open_picker_with_scope(t, true, filter_ep);
        }
    };
    let queue_episode_preload = {
        let t_dl = token.clone();
        let refresh = refresh_title_preloads.clone();
        let open_ep_dl = open_episode_download_picker.clone();
        move |t: PlayTarget, rel_hash: String, rel_title: Option<String>| {
            if downloading_ep.peek().is_some() {
                return;
            }
            let dl_key = (t.season, t.episode);
            downloading_ep.set(Some(dl_key));
            let req = PreloadRequest {
                kind,
                tmdb_id: id,
                season: if kind == MediaKind::Tv { t.season } else { 0 },
                episode: if kind == MediaKind::Tv { t.episode.max(0) } else { 0 },
                infohash: rel_hash.clone(),
                title: rel_title,
            };
            let tok = t_dl.clone();
            let refresh_cb = refresh.clone();
            let mut open_ep_dl_cb = open_ep_dl.clone();
            telegram::haptic(telegram::Haptic::Success);
            spawn(async move {
                match api::add_preload(&tok, &req).await {
                    Ok(item) => {
                        downloading_ep.set(None);
                        let resolved = item.infohash.clone();
                        if !resolved.eq_ignore_ascii_case(&rel_hash) {
                            hash_rebinds.with_mut(|v| {
                                v.retain(|(k, _)| k != &rel_hash);
                                v.push((rel_hash.clone(), resolved));
                            });
                        }
                        title_preloads.with_mut(|v| {
                            v.retain(|p| p.id != item.id && !p.infohash.eq_ignore_ascii_case(&item.infohash));
                            v.insert(0, item);
                        });
                        refresh_cb();
                        on_preloads_changed.call(());
                    }
                    Err(e) => {
                        downloading_ep.set(None);
                        if e.code == "missing_episode" && kind == MediaKind::Tv {
                            open_ep_dl_cb(t, Some(e.message));
                        } else {
                            if !*notice_pushed.peek() {
                                notice_pushed.set(true);
                                telegram::push_history(None);
                            }
                            notice.set(Some(format!("Не удалось поставить в загрузку: {}", e.message)));
                        }
                    }
                }
            });
        }
    };
    let close_picker = Callback::new(move |_: ()| {
        picker.set(None);
        picker_preloaded.set(None);
        picker_ep_scoped.set(false);
        picker_download_only.set(false);
        picker_note_override.set(None);
        if *overlay_pushed.peek() {
            overlay_pushed.set(false);
            telegram::pop_history();
        }
    });
    let close_notice = Callback::new(move |_: ()| {
        notice.set(None);
        if *notice_pushed.peek() {
            notice_pushed.set(false);
            telegram::pop_history();
        }
    });
    let close_mismatch = Callback::new(move |_: ()| {
        mismatch_prompt.set(None);
        if *mismatch_pushed.peek() {
            mismatch_pushed.set(false);
            telegram::pop_history();
        }
    });
    let open_picker_scope = Callback::new(move |(t, ep_scoped): (PlayTarget, bool)| {
        let filter_ep = t.episode > 0;
        open_picker_with_scope(t, ep_scoped, filter_ep);
    });
    let start_playback = Callback::new(
        move |(t, c, mode, explicit_rel): (PlayTarget, ReleaseChoiceUpdate, RewatchMode, bool)| {
            if !*exit_on_close.peek() && !*overlay_pushed.peek() {
                overlay_pushed.set(true);
                telegram::push_history(None);
            }
            let explicit_ep = t.season != default_target.season || t.episode != default_target.episode;
            playing.set(Some((t, c, explicit_ep, explicit_rel, mode)));
        },
    );
    let close_resolving = Callback::new(move |_: ()| {
        let next = *resolve_gen.peek() + 1;
        resolve_gen.set(next);
        resolving.set(false);
    });
    let t_poll = token.clone();
    use_effect(move || {
        let should_poll = playing().is_none()
            && (picker().is_some()
                || title_preloads().iter().any(|p| {
                    matches!(
                        p.state,
                        kino_core::PreloadState::Queued | kino_core::PreloadState::Downloading
                    )
                }));
        if !should_poll {
            if *title_poll_running.peek() {
                title_poll_running.set(false);
                let next = *title_poll_gen.peek() + 1;
                title_poll_gen.set(next);
            }
            return;
        }
        if *title_poll_running.peek() {
            return;
        }
        title_poll_running.set(true);
        let my_gen = *title_poll_gen.peek() + 1;
        title_poll_gen.set(my_gen);
        let tok = t_poll.clone();
        spawn(async move {
            loop {
                platform::sleep(4_000).await;
                if *title_poll_gen.peek() != my_gen || playing.peek().is_some() {
                    break;
                }
                let still_wanted = picker.peek().is_some()
                    || title_preloads.peek().iter().any(|p| {
                        matches!(
                            p.state,
                            kino_core::PreloadState::Queued | kino_core::PreloadState::Downloading
                        )
                    });
                if !still_wanted {
                    break;
                }
                let Ok(items) = api::preloads(&tok).await else {
                    continue;
                };
                if *title_poll_gen.peek() != my_gen {
                    break;
                }
                let filtered: Vec<kino_core::PreloadItem> = items
                    .into_iter()
                    .filter(|p| p.kind == kind && p.tmdb_id == id && p.state != kino_core::PreloadState::Deleted)
                    .collect();
                if *title_preloads.peek() != filtered {
                    title_preloads.set(filtered);
                    on_preloads_changed.call(());
                }
            }
            if *title_poll_gen.peek() == my_gen {
                title_poll_running.set(false);
            }
        });
    });
    let on_play_rewatch = Callback::new({
        let tok_res = token.clone();
        move |(t, mode): (PlayTarget, RewatchMode)| {
            telegram::haptic(telegram::Haptic::Tap);
            if !telegram::is_telegram() && mode == RewatchMode::All && api::reload_for_web_update(true) {
                return;
            }
            if *resolving.peek() {
                return;
            }
            let my_gen = *resolve_gen.peek() + 1;
            resolve_gen.set(my_gen);
            resolving.set(true);
            let tok = tok_res.clone();
            let req = ResolveReleaseRequest {
                season: if kind == MediaKind::Tv { t.season } else { 0 },
                episode: if kind == MediaKind::Tv { t.episode } else { 0 },
                is_mobile: telegram::is_mobile(),
            };
            spawn(async move {
                let res = api::resolve_release(&tok, kind, id, &req).await;
                if *resolve_gen.peek() != my_gen {
                    return;
                }
                resolving.set(false);
                match res {
                    Ok(ResolveReleaseResponse::Ready { choice, .. }) => {
                        start_playback.call((t, choice, mode, false));
                    }
                    Ok(ResolveReleaseResponse::PreloadQualityMismatch {
                        preloaded,
                        preloaded_quality,
                        target_quality,
                        fallback,
                    }) => {
                        if !auto() && !*mismatch_pushed.peek() {
                            mismatch_pushed.set(true);
                            telegram::push_history(None);
                        }
                        mismatch_prompt.set(Some((t, mode, preloaded, preloaded_quality, target_quality, *fallback)));
                    }
                    Ok(ResolveReleaseResponse::NeedsPicker { releases, .. }) => {
                        let sk = if kind == MediaKind::Tv { t.season } else { 0 };
                        picker_preloaded.set(Some((sk, releases)));
                        open_picker_scope.call((t, false));
                    }
                    Err(_) => {
                        picker_preloaded.set(None);
                        open_picker_scope.call((t, false));
                    }
                }
            });
        }
    });
    let on_play = Callback::new(move |t: PlayTarget| {
        on_play_rewatch.call((t, RewatchMode::All));
    });

    let heading = picker().map(|p| {
        let mut h = details.summary.title.clone();
        if kind == MediaKind::Tv {
            if p.episode > 0 {
                h.push_str(&format!(" · {}", kino_core::episode_code(p.season, p.episode)));
            } else if p.season > 0 {
                h.push_str(&format!(" · Сезон {}", p.season));
            }
        }
        if p.position_sec > 0.0 {
            h.push_str(&format!(" · продолжить с {}", kino_core::clock(p.position_sec)));
        }
        h
    });
    let picker_season = picker().map(|p| season_key(&p));
    let picker_episode_count = if kind == MediaKind::Tv {
        picker_season.and_then(|s| {
            details
                .seasons
                .iter()
                .find(|x| x.number == s)
                .map(|x| x.episode_count)
                .filter(|&c| c > 0)
        })
    } else {
        None
    };
    let chosen_hash = picker()
        .and_then(|p| scoped_choice(&manual_choices, kind, p.season, p.episode, None))
        .map(|c| c.infohash);
    let auto_target = auto_play_target_for(kind, &details, &progress, auto_season, &target);
    let auto_choice = auto_release.as_deref().map(|hash| {
        let fallback_list;
        let preloads = match fresh_preloads.as_deref() {
            Some(list) => list,
            None => {
                fallback_list = title_preloads.peek().clone();
                fallback_list.as_slice()
            }
        };
        auto_release_choice_for(
            kind,
            id,
            auto_target.season,
            hash,
            preloads,
            &manual_choices,
            &peer_choices,
        )
    });
    let summary = details.summary.clone();
    let t3 = token.clone();
    let main_label = format!("▶ {play_label}");
    let main_target = target.clone();
    rsx! {
        // Inside Telegram: the native big button plays, the native Back closes what is open.
        if !auto()
            && playing().is_none()
            && picker().is_none()
            && notice().is_none()
            && mismatch_prompt().is_none()
            && !resolving()
        {
            native::MainButton { text: main_label, on_click: move |_| on_play.call(main_target.clone()) }
        }
        if picker().is_some() {
            native::BackButton { on_back: move |_| close_picker.call(()) }
        }
        if resolving() {
            native::BackButton { on_back: move |_| close_resolving.call(()) }
        }
        if mismatch_prompt().is_some() {
            native::BackButton { on_back: move |_| close_mismatch.call(()) }
        }
        if notice().is_some() {
            native::BackButton { on_back: move |_| close_notice.call(()) }
        }
        if auto() {
            AutoPlay {
                on_start: {
                    let t = auto_target.clone();
                    let ac = auto_choice.clone();
                    move |_| {
                        if !telegram::is_telegram() && api::reload_for_web_update(true) {
                            return;
                        }
                        auto.set(false);
                        if let Some(choice) = ac.clone() {
                            start_playback.call((t.clone(), choice, RewatchMode::All, true));
                        } else {
                            on_play.call(t.clone());
                        }
                    }
                },
            }
        }
        TitleView {
            details: details.clone(),
            season: cur_season_data,
            progress: progress.clone(),
            status,
            my_rating,
            effective_rating: st.effective_rating,
            season_ratings: st.season_ratings.clone(),
            episode_ratings: st.episode_ratings.clone(),
            peers: st.peers.clone(),
            today_ymd: today_ymd(),
            release_label,
            season_release_label,
            title_downloads: title_preloads(),
            on_play,
            on_play_rewatch: Some(on_play_rewatch),
            on_change_release: open_picker.clone(),
            on_change_season_release: Some(Callback::new({
                let mut open_season_picker = open_season_picker.clone();
                let details_s = details.clone();
                let progress_s = progress.clone();
                let target_s = default_target.clone();
                move |s_num: i32| {
                    let season_target = auto_play_target_for(kind, &details_s, &progress_s, Some(s_num), &target_s);
                    open_season_picker(season_target);
                }
            })),
            on_change_episode_release: Some(Callback::new(open_episode_picker)),
            on_download_episode: Some(Callback::new({
                let mut queue_ep = queue_episode_preload.clone();
                let mut open_ep_dl = open_episode_download_picker.clone();
                let manual_choices = manual_choices.clone();
                let display_choice_for = display_choice_for.clone();
                let picked_now_sig = picked_now;
                move |t: PlayTarget| {
                    if kind != MediaKind::Tv || t.episode <= 0 {
                        return;
                    }
                    let explicit_pick = scoped_choice(&picked_now_sig.peek(), kind, t.season, t.episode, None);
                    let server_preload = title_preloads
                        .peek()
                        .iter()
                        .filter(|p| {
                            p.kind == MediaKind::Tv
                                && (p.season == 0 || p.season == t.season)
                                && matches!(
                                    p.state,
                                    kino_core::PreloadState::Queued
                                        | kino_core::PreloadState::Downloading
                                        | kino_core::PreloadState::Ready
                                )
                        })
                        .max_by_key(|p| (p.state == kino_core::PreloadState::Ready, !p.whole_torrent, p.requested_at))
                        .map(|p| (p.infohash.clone(), p.title.clone()));
                    let chosen_release = explicit_pick
                        .map(|c| (c.infohash, c.title))
                        .or(server_preload)
                        .or_else(|| {
                            scoped_choice(&manual_choices, kind, t.season, t.episode, None).map(|c| (c.infohash, c.title))
                        })
                        .or_else(|| display_choice_for(t.season, t.episode).map(|c| (c.infohash, c.title)));
                    if let Some((hash, title)) = chosen_release {
                        queue_ep(t, hash, title);
                    } else {
                        open_ep_dl(t, None);
                    }
                }
            })),
            on_cancel_episode_download: Some(Callback::new({
                let t_cancel = token.clone();
                let refresh = refresh_title_preloads.clone();
                move |(pid, s_num, ep_num): (i64, i32, i32)| {
                    let tok = t_cancel.clone();
                    let refresh_cb = refresh.clone();
                    telegram::haptic(telegram::Haptic::Tap);
                    spawn(async move {
                        match api::delete_preload_episode(&tok, pid, s_num, ep_num).await {
                            Ok(()) => {
                                title_preloads.with_mut(|v| {
                                    for p in v.iter_mut() {
                                        if p.id == pid {
                                            p.episodes.retain(|e| !(e.season == s_num && e.episode == ep_num));
                                        }
                                    }
                                    v.retain(|p| p.id != pid || (p.whole_torrent && !p.episodes.is_empty()));
                                });
                                refresh_cb();
                                on_preloads_changed.call(());
                            }
                            Err(e) => {
                                if !*notice_pushed.peek() {
                                    notice_pushed.set(true);
                                    telegram::push_history(None);
                                }
                                notice.set(Some(format!("Не удалось удалить серию: {}", e.message)));
                            }
                        }
                    });
                }
            })),
            downloading_episode: downloading_ep(),
            on_status: {
                let token = token.clone();
                move |s: Option<TitleStatus>| {
                    telegram::haptic(telegram::Haptic::Select);
                    status_now.set(Some(s));
                    let t = token.clone();
                    spawn(async move {
                        match api::set_status(&t, kind, id, s).await {
                            Ok(saved) => {
                                // Reflect automatic server adjustments (e.g. "Want" -> "WaitingRelease" on unreleased titles).
                                status_now.set(Some(saved.status));
                                let user_r = saved.effective_rating.or_else(|| saved.rating.map(f32::from));
                                cache.with_mut(|c| {
                                    c.patch_title(kind, id, saved.status, Some(user_r), None);
                                });
                                // A film marked "Просмотрено" also changes its progress.
                                data.restart();
                                status_now.set(None);
                            }
                            Err(e) => {
                                status_now.set(None);
                                if !*notice_pushed.peek() {
                                    notice_pushed.set(true);
                                    telegram::push_history(None);
                                }
                                notice.set(Some(format!("Не удалось сохранить статус: {}", e.message)));
                            }
                        }
                    });
                }
            },
            on_rate: {
                let token = token.clone();
                move |r: Option<i16>| {
                    telegram::haptic(telegram::Haptic::Select);
                    let seq = *rate_seq.peek() + 1;
                    rate_seq.set(seq);
                    rating_now.set(Some(r));
                    if r.is_some() {
                        status_now.set(Some(Some(TitleStatus::Watched)));
                        if kind == MediaKind::Tv && chosen_season.peek().is_none() {
                            if let Some(s) = season_state.peek().as_ref() {
                                chosen_season.set(Some(s.number));
                            }
                        }
                    }
                    let t = token.clone();
                    spawn(async move {
                        match api::set_rating(&t, kind, id, r).await {
                            Ok(updated) if *rate_seq.peek() == seq => {
                                rating_now.set(Some(updated.rating));
                                if r.is_some() && *status_now.peek() == Some(Some(TitleStatus::Watched)) {
                                    status_now.set(Some(updated.status));
                                }
                                let user_r = updated.effective_rating.or_else(|| updated.rating.map(f32::from));
                                cache.with_mut(|c| {
                                    c.patch_title(kind, id, updated.status, Some(user_r), None);
                                });
                                data.restart();
                            }
                            Err(_) if *rate_seq.peek() == seq => {
                                rating_now.set(None);
                                if r.is_some() && *status_now.peek() == Some(Some(TitleStatus::Watched)) {
                                    status_now.set(None);
                                }
                            }
                            _ => {}
                        }
                    });
                }
            },
            on_rate_season: Some(Callback::new({
                let token = token.clone();
                move |(s_num, r): (i32, Option<i16>)| {
                    telegram::haptic(telegram::Haptic::Select);
                    let t = token.clone();
                    spawn(async move {
                        if let Ok(updated) = api::set_season_rating(&t, id, s_num, r).await {
                            status_now.set(None);
                            let user_r = updated.effective_rating.or_else(|| updated.rating.map(f32::from));
                            cache.with_mut(|c| {
                                c.patch_title(kind, id, updated.status, Some(user_r), None);
                            });
                            data.restart();
                        }
                    });
                }
            })),
            on_rate_episode: Some(Callback::new({
                let token = token.clone();
                move |(s_num, ep_num, r): (i32, i32, Option<i16>)| {
                    telegram::haptic(telegram::Haptic::Select);
                    let t = token.clone();
                    spawn(async move {
                        if let Ok(updated) = api::set_episode_rating(&t, id, s_num, ep_num, r).await {
                            status_now.set(None);
                            let user_r = updated.effective_rating.or_else(|| updated.rating.map(f32::from));
                            cache.with_mut(|c| {
                                c.patch_title(kind, id, updated.status, Some(user_r), None);
                            });
                            data.restart();
                        }
                    });
                }
            })),
            on_mark: {
                let token = token.clone();
                move |(season, episodes, watched): (i32, Vec<i32>, bool)| {
                    telegram::haptic(telegram::Haptic::Select);
                    let t = token.clone();
                    let m = MarkWatched { kind, tmdb_id: id, season, episodes, watched };
                    spawn(async move {
                        if api::mark(&t, &m).await.is_ok() {
                            status_now.set(None);
                            data.restart();
                        }
                    });
                }
            },
            on_season: move |n| {
                chosen_season.set(Some(n));
                cache.with_mut(|c| {
                    if let Some(entry) = c.titles.get_mut(&(kind, id)) {
                        entry.chosen_season = Some(n);
                        if let Some(s) = entry.seasons.get(&n) {
                            season_state.set(Some(s.clone()));
                        }
                    }
                });
            },
            on_external: (!platform::is_web()).then(|| Callback::new(|url: String| platform::open_external(&url))),
        }
        if let Some(extras) = extras_state() {
            TitleExtrasView { extras, on_open, on_person }
        }
        if let (Some(heading), Some(target)) = (heading, picker()) {
            match releases.read().clone().flatten() {
                None => rsx! {
                    div { class: "sheet-bg", onclick: move |_| close_picker.call(()),
                        div { class: "sheet", onclick: move |e| e.stop_propagation(),
                            div { class: "sheet-head",
                                h3 { "{heading}" }
                                button {
                                    class: "sheet-close",
                                    title: "Закрыть",
                                    "aria-label": "Закрыть",
                                    onclick: move |_| close_picker.call(()),
                                    Icon { svg: icons::CLOSE, class: "ic sm" }
                                }
                            }
                            p { class: "muted", "Ищем раздачи на трекерах…" }
                        }
                    }
                },
                Some(Err(e)) => rsx! {
                    div { class: "sheet-bg", onclick: move |_| close_picker.call(()),
                        div { class: "sheet", onclick: move |e| e.stop_propagation(),
                            div { class: "sheet-head",
                                h3 { "{heading}" }
                                button {
                                    class: "sheet-close",
                                    title: "Закрыть",
                                    "aria-label": "Закрыть",
                                    onclick: move |_| close_picker.call(()),
                                    Icon { svg: icons::CLOSE, class: "ic sm" }
                                }
                            }
                            p { class: "error", "{e.message}" }
                            button { class: "btn glass", onclick: move |_| close_picker.call(()), "Закрыть" }
                        }
                    }
                },
                Some(Ok(mut list)) => {
                    let cur_preloads: Vec<kino_core::PreloadItem> = title_preloads()
                        .into_iter()
                        .filter(|p| {
                            kind == MediaKind::Movie || p.season == 0 || Some(p.season) == picker_season
                        })
                        .collect();
                    let scope_s = picker_season.unwrap_or(0);
                    let scope_ep = if kind == MediaKind::Tv && scope_s > 0 {
                        filter().episode.unwrap_or(0)
                    } else {
                        0
                    };
                    {
                        let rebinds = hash_rebinds.read();
                        for r in &mut list.releases {
                            if let Some((_, real)) = rebinds.iter().find(|(k, _)| k == &r.infohash) {
                                r.infohash = real.clone();
                            }
                            if cur_preloads
                                .iter()
                                .any(|p| p.infohash.eq_ignore_ascii_case(&r.infohash) && p.is_ready_for(kind, scope_s, scope_ep))
                            {
                                r.preloaded = true;
                            }
                        }
                    }
                    let picker_note = picker_note_override().or_else(|| {
                        defaults_relaxed().then(|| match filter().language {
                            Some(l) => {
                                format!("По строгим фильтрам ничего не нашлось — показаны все доступные в озвучке «{l}».")
                            }
                            None => "Ваши фильтры по умолчанию ничего не нашли, показаны все раздачи.".to_owned(),
                        })
                    });
                    rsx! { ReleasePicker {
                heading,
                releases: list.releases,
                chosen: chosen_hash,
                watched,
                filter: filter(),
                sort: sort(),
                panel: panel(),
                native_player: platform::video::supports_native(),
                episode_count: picker_episode_count,
                mock: !list.configured,
                note: picker_note,
                preloads: cur_preloads,
                picker_season: scope_s,
                download_only: picker_download_only(),
                on_filter: move |f| filter.set(f),
                on_sort: move |s| sort.set(s),
                on_panel: move |p| panel.set(p),
                on_pick: {
                    let mut queue_ep = queue_episode_preload.clone();
                    let t_pick_dl = token.clone();
                    let refresh_pick = refresh_title_preloads.clone();
                    move |r: ReleaseOption| {
                        if *picker_download_only.peek() {
                            let dl_target = PlayTarget {
                                season: scope_s,
                                episode: scope_ep,
                                position_sec: 0.0,
                            };
                            picker.set(None);
                            picker_preloaded.set(None);
                            picker_ep_scoped.set(false);
                            picker_download_only.set(false);
                            picker_note_override.set(None);
                            if *overlay_pushed.peek() {
                                overlay_pushed.set(false);
                                telegram::pop_history();
                            }
                            if kind == MediaKind::Tv && scope_ep > 0 {
                                queue_ep(dl_target, r.infohash, (!r.title.is_empty()).then_some(r.title));
                            } else {
                                let req = PreloadRequest {
                                    kind,
                                    tmdb_id: id,
                                    season: scope_s,
                                    episode: 0,
                                    infohash: r.infohash.clone(),
                                    title: (!r.title.is_empty()).then_some(r.title),
                                };
                                let t = t_pick_dl.clone();
                                let refresh_cb = refresh_pick.clone();
                                spawn(async move {
                                    match api::add_preload(&t, &req).await {
                                        Ok(item) => {
                                            title_preloads.with_mut(|v| {
                                                v.retain(|p| p.id != item.id && !p.infohash.eq_ignore_ascii_case(&item.infohash));
                                                v.insert(0, item);
                                            });
                                            refresh_cb();
                                            on_preloads_changed.call(());
                                        }
                                        Err(e) => notice.set(Some(format!("Не удалось поставить в загрузку: {}", e.message))),
                                    }
                                });
                            }
                            return;
                        }
                        let ep_scope = if kind == MediaKind::Tv && *picker_ep_scoped.peek() {
                            target.episode
                        } else {
                            0
                        };
                        let choice = r.to_scoped_choice(&summary, season_key(&target), ep_scope);
                        picked_now.with_mut(|v| {
                            v.retain(|c| !(c.season == choice.season && c.episode == choice.episode && c.quality_key == choice.quality_key));
                            v.insert(0, choice.clone());
                        });
                        picker.set(None);
                        picker_preloaded.set(None);
                        picker_ep_scoped.set(false);
                        picker_download_only.set(false);
                        picker_note_override.set(None);
                        let mut play_target = target.clone();
                        if kind == MediaKind::Tv && play_target.episode <= 0 {
                            play_target.episode = 1;
                        }
                        let explicit_ep =
                            play_target.season != default_target.season || play_target.episode != default_target.episode;
                        let qk = choice
                            .quality_key
                            .clone()
                            .unwrap_or_else(|| normalize_quality_key(&r.quality).to_owned());
                        playing.set(Some((play_target, choice.clone(), explicit_ep, true, RewatchMode::All)));
                        let t = t3.clone();
                        let req_q = api::SetDeviceQualityRequest {
                            kind,
                            tmdb_id: id,
                            season: choice.season,
                            episode: choice.episode,
                            is_mobile: telegram::is_mobile(),
                            quality_key: qk,
                        };
                        spawn(async move {
                            let _ = api::save_release_choice(&t, &choice).await;
                            let _ = api::set_release_quality(&t, &req_q).await;
                        });
                    }
                },
                on_preload: {
                    let t4 = token.clone();
                    let refresh_pre = refresh_title_preloads.clone();
                    let pre_season = picker_season.unwrap_or(0);
                    let pre_ep = if kind == MediaKind::Tv {
                        filter().episode.unwrap_or(0).max(0)
                    } else {
                        0
                    };
                    move |r: ReleaseOption| {
                        let orig_key = r.infohash.clone();
                        let req = PreloadRequest {
                            kind,
                            tmdb_id: id,
                            season: pre_season,
                            episode: pre_ep,
                            infohash: orig_key.clone(),
                            title: (!r.title.is_empty()).then(|| r.title.clone()),
                        };
                        let t = t4.clone();
                        let refresh_cb = refresh_pre.clone();
                        telegram::haptic(telegram::Haptic::Success);
                        spawn(async move {
                            match api::add_preload(&t, &req).await {
                                Ok(item) => {
                                    let resolved = item.infohash.clone();
                                    if !resolved.eq_ignore_ascii_case(&orig_key) {
                                        hash_rebinds.with_mut(|v| {
                                            v.retain(|(k, _)| k != &orig_key);
                                            v.push((orig_key.clone(), resolved.clone()));
                                        });
                                        picker_preloaded.with_mut(|pp| {
                                            if let Some((_, ref mut pl)) = pp.as_mut() {
                                                for rel in pl.releases.iter_mut() {
                                                    if rel.infohash == orig_key {
                                                        rel.infohash = resolved.clone();
                                                    }
                                                }
                                            }
                                        });
                                    }
                                    title_preloads.with_mut(|v| {
                                        v.retain(|p| {
                                            p.id != item.id && !p.infohash.eq_ignore_ascii_case(&item.infohash)
                                        });
                                        v.insert(0, item);
                                    });
                                    refresh_cb();
                                    on_preloads_changed.call(());
                                }
                                Err(e) => {
                                    if !*notice_pushed.peek() {
                                        notice_pushed.set(true);
                                        telegram::push_history(None);
                                    }
                                    notice.set(Some(format!("Не удалось поставить в загрузку: {}", e.message)));
                                }
                            }
                        });
                    }
                },
                on_cancel_preload: Some(Callback::new({
                    let t5 = token.clone();
                    let refresh_cancel = refresh_title_preloads.clone();
                    let cancel_season = picker_season.unwrap_or(0);
                    let cancel_ep = if kind == MediaKind::Tv {
                        filter().episode.unwrap_or(0).max(0)
                    } else {
                        0
                    };
                    move |preload_id: i64| {
                        let t = t5.clone();
                        let refresh_cb = refresh_cancel.clone();
                        let cancel_single = kind == MediaKind::Tv
                            && cancel_season > 0
                            && cancel_ep > 0
                            && title_preloads
                                .peek()
                                .iter()
                                .find(|p| p.id == preload_id)
                                .is_some_and(|p| !p.whole_torrent);
                        spawn(async move {
                            let res = if cancel_single {
                                api::delete_preload_episode(&t, preload_id, cancel_season, cancel_ep).await
                            } else {
                                api::delete_preload(&t, preload_id).await
                            };
                            match res {
                                Ok(()) => {
                                    title_preloads.with_mut(|v| {
                                        if cancel_single {
                                            for p in v.iter_mut() {
                                                if p.id == preload_id {
                                                    p.episodes.retain(|e| {
                                                        !(e.season == cancel_season && e.episode == cancel_ep)
                                                    });
                                                }
                                            }
                                            v.retain(|p| p.id != preload_id || !p.episodes.is_empty());
                                        } else {
                                            v.retain(|p| p.id != preload_id);
                                        }
                                    });
                                    refresh_cb();
                                    on_preloads_changed.call(());
                                }
                                Err(e) => {
                                    if !*notice_pushed.peek() {
                                        notice_pushed.set(true);
                                        telegram::push_history(None);
                                    }
                                    notice.set(Some(format!("Не удалось отменить загрузку: {}", e.message)));
                                }
                            }
                        });
                    }
                })),
                on_close: move |_| close_picker.call(()),
            } } },
            }
        }
        if resolving() {
            div { class: "sheet-bg", onclick: move |_| close_resolving.call(()),
                div { class: "sheet", onclick: move |e| e.stop_propagation(),
                    div { class: "sheet-head",
                        h3 { "Запуск…" }
                        button {
                            class: "sheet-close",
                            title: "Отмена",
                            "aria-label": "Отмена",
                            onclick: move |_| close_resolving.call(()),
                            Icon { svg: icons::CLOSE, class: "ic sm" }
                        }
                    }
                    p { class: "muted", "Подбираем подходящую раздачу…" }
                    div { style: "margin-top:12px;",
                        button { class: "btn ghost", onclick: move |_| close_resolving.call(()), "Отменить" }
                    }
                }
            }
        }
        if let Some(text) = notice() {
            div { class: "sheet-bg", onclick: move |_| close_notice.call(()),
                div { class: "sheet", onclick: move |e| e.stop_propagation(),
                    p { "{text}" }
                    button { class: "btn ghost", onclick: move |_| close_notice.call(()), "Хорошо" }
                }
            }
        }
        if let Some((t_m, mode_m, preloaded_m, pre_q, tgt_q, fallback_m)) = mismatch_prompt() {
            div { class: "sheet-bg", onclick: move |_| close_mismatch.call(()),
                div { class: "sheet", onclick: move |e| e.stop_propagation(),
                    div { class: "sheet-head",
                        h3 { "Есть скачанный файл ({pre_q})" }
                        button {
                            class: "sheet-close",
                            title: "Закрыть",
                            "aria-label": "Закрыть",
                            onclick: move |_| close_mismatch.call(()),
                            Icon { svg: icons::CLOSE, class: "ic sm" }
                        }
                    }
                    p { class: "muted",
                        "На сервере уже скачана раздача в качестве {pre_q}, а для этого устройства настроено {tgt_q}. Запустить скачанную без ожидания?"
                    }
                    div { style: "display:flex; gap:10px; flex-wrap:wrap; margin-top:14px;",
                        button {
                            class: "btn primary",
                            onclick: {
                                let t_m = t_m.clone();
                                let preloaded_m = preloaded_m.clone();
                                move |_| {
                                    close_mismatch.call(());
                                    start_playback.call((t_m.clone(), preloaded_m.clone(), mode_m, false));
                                }
                            },
                            "Смотреть скачанную ({pre_q})"
                        }
                        button {
                            class: "btn ghost",
                            onclick: {
                                let t_m = t_m.clone();
                                let fallback_m = fallback_m.clone();
                                move |_| {
                                    close_mismatch.call(());
                                    match fallback_m.clone() {
                                        ResolveReleaseResponse::Ready { choice, .. } => {
                                            start_playback.call((t_m.clone(), choice, mode_m, false));
                                        }
                                        ResolveReleaseResponse::NeedsPicker { releases, .. } => {
                                            let sk = if kind == MediaKind::Tv { t_m.season } else { 0 };
                                            picker_preloaded.set(Some((sk, releases)));
                                            open_picker_scope.call((t_m.clone(), false));
                                        }
                                        ResolveReleaseResponse::PreloadQualityMismatch { preloaded, .. } => {
                                            start_playback.call((t_m.clone(), preloaded, mode_m, false));
                                        }
                                    }
                                }
                            },
                            match &fallback_m {
                                ResolveReleaseResponse::Ready { .. } => format!("Смотреть {tgt_q} онлайн"),
                                _ => "Выбрать другую раздачу".to_owned(),
                            }
                        }
                    }
                }
            }
        }
        if let Some((t, rel, explicit_episode, explicit_release, rewatch_mode)) = playing() {
            Player {
                key: "{t.season}-{t.episode}-{rel.infohash}",
                token: token.clone(),
                start: PlayStart {
                    kind,
                    tmdb_id: id,
                    title: details.summary.title.clone(),
                    poster: details.summary.backdrop_path.as_deref().map(|p| image_url(p, ImageSize::Backdrop)),
                    season: t.season,
                    episode: t.episode,
                    choices: manual_choices.clone(),
                    release: rel,
                    explicit_episode,
                    explicit_release,
                    video_scale: scale_now().or(st.video_scale),
                    rewatch_mode,
                    episode_ratings: st.episode_ratings.clone(),
                },
                on_scale: {
                    let t = token.clone();
                    move |next: VideoScale| {
                        scale_now.set(Some(next));
                        let tok = t.clone();
                        spawn(async move {
                            let _ = api::set_scale(&tok, kind, id, Some(next)).await;
                        });
                    }
                },
                on_close: {
                    let refresh = refresh_title_preloads.clone();
                    move |from_pop: bool| {
                        playing.set(None);
                        if exit_on_close() {
                            exit_on_close.set(false);
                            on_exit.call(from_pop);
                        } else {
                            if *overlay_pushed.peek() {
                                overlay_pushed.set(false);
                                if !from_pop {
                                    telegram::pop_history();
                                }
                            }
                            if !telegram::is_telegram() && api::reload_for_web_update(false) {
                                return;
                            }
                            data.restart();
                            refresh();
                        }
                    }
                },
                on_title: {
                    let refresh = refresh_title_preloads.clone();
                    move |_| {
                        playing.set(None);
                        exit_on_close.set(false);
                        if *overlay_pushed.peek() {
                            overlay_pushed.set(false);
                            telegram::pop_history();
                        }
                        if !telegram::is_telegram() && api::reload_for_web_update(false) {
                            return;
                        }
                        data.restart();
                        refresh();
                    }
                },
                on_need_release: {
                    let mut open_picker = open_picker.clone();
                    move |(season, episode, position_sec): (i32, i32, f64)| {
                        playing.set(None);
                        open_picker(PlayTarget { season, episode, position_sec });
                    }
                },
                on_preloads_changed: Some(Callback::new({
                    let refresh = refresh_title_preloads.clone();
                    move |_| {
                        refresh();
                        on_preloads_changed.call(());
                    }
                })),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use kino_core::{PreloadPhase, SeasonSummary};

    fn sample_details() -> TitleDetails {
        TitleDetails {
            summary: TitleSummary {
                kind: MediaKind::Tv,
                tmdb_id: 1396,
                title: "Во все тяжкие".into(),
                original_title: None,
                year: Some(2008),
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
                user_rating: None,
                user_status: None,
                peers: vec![],
                preloaded: true,
            },
            genres: vec![],
            runtime_min: None,
            tagline: None,
            seasons: vec![
                SeasonSummary {
                    number: 1,
                    name: "Сезон 1".into(),
                    episode_count: 7,
                    year: Some(2008),
                },
                SeasonSummary {
                    number: 2,
                    name: "Сезон 2".into(),
                    episode_count: 13,
                    year: Some(2009),
                },
            ],
            imdb_id: None,
            last_episode: Some((2, 13)),
            series_status: None,
            release_date: None,
            next_episode: None,
            alternative_titles: vec![],
        }
    }

    fn make_progress(season: i32, episode: i32, pos: f64, finished: bool, sec_offset: i64) -> Progress {
        Progress {
            kind: MediaKind::Tv,
            tmdb_id: 1396,
            season,
            episode,
            position_sec: pos,
            duration_sec: 2800.0,
            finished,
            release: None,
            updated_at: DateTime::from_timestamp(1_700_000_000 + sec_offset, 0).unwrap(),
        }
    }

    #[test]
    fn auto_play_target_targets_downloaded_season() {
        let details = sample_details();
        let p_s1 = make_progress(1, 3, 120.0, false, 100);
        let default_t = PlayTarget {
            season: 1,
            episode: 3,
            position_sec: 120.0,
        };

        // Pack preload keeps user's default target.
        assert_eq!(
            auto_play_target_for(MediaKind::Tv, &details, std::slice::from_ref(&p_s1), None, &default_t),
            default_t
        );

        // Season 2 preload jumps straight to S02E01 when user only watched S01.
        assert_eq!(
            auto_play_target_for(MediaKind::Tv, &details, &[p_s1], Some(2), &default_t),
            PlayTarget {
                season: 2,
                episode: 1,
                position_sec: 0.0,
            }
        );

        // When user already finished S02E01 earlier, then watched S01 later, resumes S02E02.
        let p_s2_done = make_progress(2, 1, 2800.0, true, 50);
        let p_s1_newer = make_progress(1, 5, 300.0, false, 200);
        assert_eq!(
            auto_play_target_for(MediaKind::Tv, &details, &[p_s2_done, p_s1_newer], Some(2), &default_t),
            PlayTarget {
                season: 2,
                episode: 2,
                position_sec: 0.0,
            }
        );

        // When default target is already in season 2, preserves exact resume timestamp.
        let default_s2 = PlayTarget {
            season: 2,
            episode: 4,
            position_sec: 555.0,
        };
        assert_eq!(
            auto_play_target_for(MediaKind::Tv, &details, &[], Some(2), &default_s2),
            default_s2
        );
    }

    #[test]
    fn auto_release_choice_prefers_preloaded_item_metadata() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let preload = PreloadItem {
            id: 42,
            kind: MediaKind::Tv,
            tmdb_id: 1396,
            season: 2,
            infohash: hash.to_owned(),
            title: Some("Breaking Bad S02 2160p LostFilm".into()),
            state: PreloadState::Ready,
            progress: 1.0,
            size_bytes: Some(12_000_000_000),
            requested_at: Utc::now(),
            ready_at: Some(Utc::now()),
            delete_after: None,
            poster_path: None,
            media_title: None,
            quality: Some("4K".into()),
            voice: Some("LostFilm".into()),
            speed_bps: 0,
            dl_phase: PreloadPhase::default(),
            whole_torrent: true,
            episodes: Vec::new(),
            requested_by: None,
            can_delete: true,
        };

        let choice = auto_release_choice_for(MediaKind::Tv, 1396, 2, &hash.to_ascii_uppercase(), &[preload], &[], &[]);
        assert_eq!(choice.infohash, hash);
        assert_eq!(choice.season, 2);
        assert_eq!(choice.quality.as_deref(), Some("4K"));
        assert_eq!(choice.quality_key.as_deref(), Some("2160p"));
        assert_eq!(choice.voice.as_deref(), Some("LostFilm"));
        assert_eq!(choice.title.as_deref(), Some("Breaking Bad S02 2160p LostFilm"));

        // Falls back cleanly when preload metadata is absent.
        let minimal = auto_release_choice_for(MediaKind::Movie, 550, 0, hash, &[], &[], &[]);
        assert_eq!(minimal.infohash, hash);
        assert_eq!(minimal.season, 0);
        assert_eq!(minimal.quality, None);
    }
}
