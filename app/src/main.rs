//! Kino client. One codebase for the website and the Telegram Mini App;
//! desktop and mobile builds come later (see README).
//!
//! The screens live in `kino-ui`; this crate fetches data and handles clicks.

mod api;
mod native;
mod platform;
mod player;
mod telegram;

use player::{PlayStart, Player};

use dioxus::prelude::*;
use kino_core::{
    image_url, AuthResponse, HomeFeed, ImageSize, MarkWatched, MediaKind, PreloadRequest, PreloadState,
    ReleaseChoiceUpdate, TitleStatus, TitleSummary, UserPrefs, WidgetAuthRequest,
};
use kino_ui::{
    icons, play_target, HomeView, Icon, LoginView, MyFilters, MyView, PersonView, PickerPanel, PlayTarget, ProfileView,
    ReleaseFilter, ReleaseOption, ReleasePicker, ReleaseSort, SearchView, ShelfView, Shell, Tab, Theme,
    TitleExtrasView, TitleView, FAVICON_URI, STYLE,
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
            if (m) { m.content += ', viewport-fit=cover'; }
            try {
                var t = localStorage.getItem('kino.theme');
                if (t === 'dark' || t === 'light') { document.documentElement.dataset.theme = t; }
            } catch (e) {}
        </script>"#;
        #[cfg(all(feature = "desktop", not(feature = "mobile")))]
        use dioxus::desktop::Config;
        #[cfg(feature = "mobile")]
        use dioxus::mobile::Config;
        dioxus::LaunchBuilder::new()
            .with_cfg(Config::new().with_custom_head(HEAD.to_owned()))
            .launch(App);
    }

    #[cfg(not(any(feature = "mobile", feature = "desktop")))]
    dioxus::launch(App);
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
        telegram::init();
        spawn(async move {
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
    let mut invite = use_signal(telegram::invite);
    let mut link = use_resource(move || api::start_login_link(invite()));
    let mut edit_server = use_signal(|| false);
    let mut server_input = use_signal(platform::server_base);
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
        _ => None,
    };
    let shown_error = match state() {
        LinkLogin::Failed(e) => Some(e),
        _ => link_error.or(error),
    };
    let ready = match &*link.read() {
        Some(Ok(l)) => Some(l.clone()),
        _ => None,
    };
    let server_host = platform::server_base()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_owned();
    rsx! {
        LoginView { invite: telegram::invite(), error: shown_error,
            match (state(), ready) {
                (LinkLogin::Waiting, Some(l)) => rsx! {
                    p { class: "login-wait-txt", "Откройте Telegram и нажмите «Войти на сайт» в сообщении бота. Эта страница откроется сама." }
                    a {
                        class: "btn glass",
                        href: "{l.url}",
                        target: "_blank",
                        rel: "noopener",
                        onclick: move |e: Event<MouseData>| open_link(&e, &l.url),
                        Icon { svg: icons::TG, class: "ic sm" }
                        "Открыть Telegram ещё раз"
                    }
                    p { class: "login-cancel",
                        a { class: "link", href: "#", onclick: move |e: Event<MouseData>| { e.prevent_default(); state.set(LinkLogin::Idle) }, "Отмена" }
                    }
                },
                (_, Some(l)) => rsx! {
                    if !platform::is_web() {
                        input {
                            class: "input-field",
                            placeholder: "Код приглашения (если есть)",
                            value: "{invite().unwrap_or_default()}",
                            oninput: move |e| invite.set(Some(e.value().trim().to_owned()).filter(|v| !v.is_empty())),
                        }
                    }
                    a { class: "btn primary", href: "{l.url}", target: "_blank", rel: "noopener",
                        onclick: move |e: Event<MouseData>| {
                            open_link(&e, &l.url);
                            wait(l.token.clone());
                        },
                        Icon { svg: icons::TG, class: "ic sm" }
                        "Войти через Telegram"
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
                                oninput: move |e| server_input.set(e.value()),
                            }
                            button {
                                class: "btn glass sm",
                                style: "width: auto",
                                onclick: move |_| {
                                    let url = server_input().trim().trim_end_matches('/').to_owned();
                                    if url.starts_with("http://") || url.starts_with("https://") {
                                        platform::save_server(&url);
                                        platform::set_server_base(&url);
                                        edit_server.set(false);
                                        state.set(LinkLogin::Idle);
                                        link.restart();
                                    }
                                },
                                "Сохранить"
                            }
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
    /// A title page; the flag starts playback at once (from "Continue watching").
    Title(MediaKind, i64, bool),
    /// An actor's page.
    Person(i64),
    /// Full grid of a home shelf ("Смотреть все").
    Shelf {
        id: String,
        title: String,
        subtitle: Option<String>,
    },
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
    // A link to a title (from a bot message) opens it, with Home behind it.
    let link = use_hook(telegram::open_target);
    use_hook(|| {
        let initial_url = link.map(|(k, id)| format!("/?open={}_{id}", k.as_str()));
        telegram::init_history(link.is_some(), initial_url.as_deref());
    });
    let mut view = use_signal(|| match link {
        Some((kind, id)) => View::Title(kind, id, false),
        None => View::Tab(Tab::Home),
    });
    // Where "Back" goes from a title page.
    let mut trail = use_signal(|| link.map(|_| vec![View::Tab(Tab::Home)]).unwrap_or_default());
    let mut query = use_signal(String::new);
    let mut home_category = use_signal(|| "for_you".to_owned());
    let mut my_filters = use_signal(MyFilters::default);
    // Restore saved "Мои" filters on startup.
    use_effect(move || {
        spawn(async move {
            if let Some(saved) = platform::storage_get(MY_FILTERS_KEY).await {
                my_filters.set(MyFilters::decode(&saved));
            }
        });
    });
    // Global Ctrl+K / Cmd+K focuses the search box.
    use_hook(|| {
        let _ = dioxus::document::eval(
            "if (!window.__kinoSearchShortcut) {
               window.__kinoSearchShortcut = true;
               window.addEventListener('keydown', function(e) {
                 if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
                   var el = document.getElementById('kino-search-input');
                   if (el) { e.preventDefault(); el.focus(); el.select(); }
                 }
               });
             }",
        );
    });
    // Lightweight active-download check for the topbar badge / mobile avatar dot.
    let t_dl = token.clone();
    let active_preloads = use_resource(move || {
        let t = t_dl.clone();
        async move { api::preloads(&t).await.ok() }
    });
    let dl_percent: Option<u8> = active_preloads
        .read()
        .as_ref()
        .and_then(Option::as_ref)
        .and_then(|list| list.iter().find(|p| p.state == PreloadState::Downloading))
        .map(|p| (p.progress * 100.0).clamp(0.0, 100.0).round() as u8);

    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let tab = match view() {
        View::Tab(t) => Some(t),
        View::Title(..) | View::Person(_) | View::Shelf { .. } => None,
    };
    let mut open_title = move |kind: MediaKind, id: i64, play: bool| {
        // Coming back to a title must not start it again.
        trail.with_mut(|t| {
            t.push(match view() {
                View::Title(k, i, _) => View::Title(k, i, false),
                v => v,
            })
        });
        view.set(View::Title(kind, id, play));
        let url = format!("/?open={}_{id}", kind.as_str());
        telegram::push_history(Some(&url));
    };
    let open = move |t: TitleSummary| open_title(t.kind, t.tmdb_id, false);
    let open_person = move |id: i64| {
        telegram::haptic(telegram::Haptic::Tap);
        trail.with_mut(|t| {
            t.push(match view() {
                View::Title(k, i, _) => View::Title(k, i, false),
                v => v,
            })
        });
        view.set(View::Person(id));
        telegram::push_history(Some("/"));
    };
    let open_shelf = move |(id, title, subtitle): (String, String, Option<String>)| {
        telegram::haptic(telegram::Haptic::Tap);
        trail.with_mut(|t| {
            t.push(match view() {
                View::Title(k, i, _) => View::Title(k, i, false),
                v => v,
            })
        });
        view.set(View::Shelf { id, title, subtitle });
        telegram::push_history(Some("/"));
    };
    let go_back = move |_: ()| {
        telegram::haptic(telegram::Haptic::Tap);
        if let Some(prev) = trail.with_mut(|t| t.pop()) {
            if matches!(prev, View::Tab(Tab::Home)) && trail.peek().is_empty() {
                query.set(String::new());
            }
            view.set(prev);
            telegram::pop_history();
        }
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
            on_tab: move |t| {
                telegram::haptic(telegram::Haptic::Tap);
                if t != Tab::Search {
                    query.set(String::new());
                }
                if t == Tab::Home {
                    home_category.set("for_you".to_owned());
                }
                if view() == View::Tab(t) && (t != Tab::Home || trail.peek().is_empty()) {
                    if t == Tab::Search {
                        let _ = dioxus::document::eval(
                            "var el = document.getElementById('kino-search-input'); if (el) el.focus();",
                        );
                    }
                    return;
                }
                if t == Tab::Home || telegram::is_telegram() {
                    trail.set(Vec::new());
                    view.set(View::Tab(t));
                    if !telegram::is_telegram() {
                        telegram::reset_history();
                    }
                } else {
                    let prev = match view() {
                        View::Title(k, i, _) => View::Title(k, i, false),
                        v => v,
                    };
                    trail.with_mut(|tr| tr.push(prev));
                    view.set(View::Tab(t));
                    telegram::push_history(Some("/"));
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
                        if let Some(prev) = trail.with_mut(|tr| tr.pop()) {
                            view.set(prev);
                            telegram::pop_history();
                        } else {
                            view.set(View::Tab(Tab::Home));
                        }
                    }
                } else if !matches!(view(), View::Tab(Tab::Search)) {
                    if !telegram::is_telegram() {
                        let prev = match view() {
                            View::Title(k, i, _) => View::Title(k, i, false),
                            v => v,
                        };
                        trail.with_mut(|tr| tr.push(prev));
                        telegram::push_history(Some("/"));
                    } else {
                        trail.set(Vec::new());
                    }
                    view.set(View::Tab(Tab::Search));
                }
                query.set(q);
            },
            match view() {
                View::Tab(Tab::Home) => rsx! {
                    Home {
                        token: token.clone(),
                        active_category: home_category,
                        on_open: open,
                        on_open_title: move |(k, id)| open_title(k, id, false),
                        on_play: move |(k, id)| open_title(k, id, true),
                        on_open_shelf: open_shelf,
                    }
                },
                View::Tab(Tab::Search) => rsx! { Search { token: token.clone(), query, on_open: open } },
                View::Tab(Tab::Mine) => rsx! {
                    Mine {
                        token: token.clone(),
                        filters: my_filters,
                        on_filters: move |f: MyFilters| {
                            telegram::haptic(telegram::Haptic::Select);
                            my_filters.set(f);
                            platform::storage_set(MY_FILTERS_KEY, &f.encode());
                        },
                        on_open: open,
                    }
                },
                View::Tab(Tab::Profile) => rsx! {
                    Profile {
                        token: token.clone(),
                        name: name.clone(),
                        avatar_url: avatar_url.clone(),
                        theme,
                        on_theme,
                        on_open: move |(k, id)| open_title(k, id, false),
                        on_logout,
                    }
                },
                View::Title(kind, id, play) => rsx! {
                    Title {
                        key: "{kind.as_str()}{id}{play}",
                        token: token.clone(),
                        kind,
                        id,
                        autoplay: play,
                        on_open: open,
                        on_person: open_person,
                    }
                },
                View::Person(id) => rsx! { Person { key: "person{id}", token: token.clone(), id, on_open: open } },
                View::Shelf { id, title, subtitle } => rsx! {
                    Shelf {
                        key: "shelf-{id}",
                        token: token.clone(),
                        id,
                        title,
                        subtitle,
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
fn Person(token: String, id: i64, on_open: EventHandler<TitleSummary>) -> Element {
    let data = use_resource(move || {
        let t = token.clone();
        async move { api::person(&t, id).await }
    });
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
    on_open: EventHandler<TitleSummary>,
    on_open_title: EventHandler<(MediaKind, i64)>,
    on_play: EventHandler<(MediaKind, i64)>,
    on_open_shelf: EventHandler<(String, String, Option<String>)>,
) -> Element {
    let token_cont = token.clone();
    let mut cont_res = use_resource(move || {
        let t = token_cont.clone();
        async move { api::continue_watching(&t).await }
    });
    let token_feed = token.clone();
    let mut feed_res = use_resource(move || {
        let t = token_feed.clone();
        async move { api::home(&t).await }
    });

    let mut feed_state = use_signal(|| None::<HomeFeed>);
    let mut loading_shelves = use_signal(Vec::<String>::new);
    let mut loading_row_pages = use_signal(Vec::<String>::new);
    let mut loading_more_rows = use_signal(|| false);

    use_effect(move || {
        if let Some(Ok(f)) = &*feed_res.read() {
            feed_state.set(Some(f.clone()));
            loading_shelves.set(Vec::new());
            loading_row_pages.set(Vec::new());
            loading_more_rows.set(false);
        }
    });

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
                let res = api::home_shelves(&t, &batch_ids).await;
                if let Ok(loaded) = res {
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
                }
                loading_shelves.with_mut(|v| v.retain(|id| !batch_ids.contains(id)));
                loading_more_rows.set(false);
            });
        }
    };

    // If restored category isn't "for_you", load its hidden shelves right after feed arrives.
    {
        let mut load_batch = load_batch.clone();
        use_effect(move || {
            let cat = active_category();
            if feed_state().is_some() && cat != "for_you" {
                load_batch(6, cat);
            }
        });
    }

    // Auto-load next deferred shelf batch when scrolling near the bottom sentinel.
    {
        let mut load_batch = load_batch.clone();
        use_hook(move || {
            spawn(async move {
                let mut bridge = dioxus::document::eval(
                    "if (window.__kinoHomeStop) { try { window.__kinoHomeStop(); } catch (e) {} }
                     var obs = null, timer = null, lastSent = 0, observedEl = null;
                     var check = function() {
                       var el = document.getElementById('home-lazy-sentinel');
                       if (!el) return;
                       var r = el.getBoundingClientRect();
                       if (r.top <= window.innerHeight + 350) {
                         var now = Date.now();
                         if (now - lastSent > 350) {
                           lastSent = now;
                           dioxus.send(true);
                         }
                       }
                     };
                     if (typeof IntersectionObserver !== 'undefined') {
                       obs = new IntersectionObserver(function(entries) {
                         if (entries.some(function(e) { return e.isIntersecting; })) check();
                       }, { rootMargin: '350px 0px' });
                     }
                     timer = setInterval(function() {
                       if (!document.getElementById('home-active-marker')) {
                         window.__kinoHomeStop();
                         return;
                       }
                       var el = document.getElementById('home-lazy-sentinel');
                       if (el !== observedEl) {
                         if (obs && observedEl) obs.unobserve(observedEl);
                         observedEl = el;
                         if (obs && el) obs.observe(el);
                       }
                       check();
                     }, 450);
                     window.addEventListener('scroll', check, { passive: true });
                     window.__kinoHomeStop = function() {
                       if (obs) obs.disconnect();
                       if (timer) clearInterval(timer);
                       window.removeEventListener('scroll', check);
                     };",
                );
                while let Ok(true) = bridge.recv::<bool>().await {
                    if !*loading_more_rows.peek() {
                        load_batch(HOME_LAZY_BATCH, active_category.peek().clone());
                    }
                }
            });
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
                }
                loading_row_pages.with_mut(|v| v.retain(|id| id != &sid));
            });
        }
    };

    let cont_items = match &*cont_res.read() {
        Some(Ok(items)) => items.clone(),
        _ => Vec::new(),
    };
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
                spawn(async move {
                    if api::set_status(&t, c.last.kind, c.last.tmdb_id, Some(TitleStatus::Dropped))
                        .await
                        .is_ok()
                    {
                        cont_res.restart();
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
    on_open: EventHandler<TitleSummary>,
) -> Element {
    let mut items = use_signal(Vec::<TitleSummary>::new);
    let mut page = use_signal(|| 0u32);
    let mut has_more = use_signal(|| true);
    let mut loading = use_signal(|| true);
    let mut err = use_signal(|| None::<String>);
    let mut shelf_title = use_signal(|| title);
    let mut shelf_sub = use_signal(|| subtitle);

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
                    }
                    Err(e) => err.set(Some(e.message)),
                }
                loading.set(false);
            });
        }
    };

    {
        let mut fp = fetch_page.clone();
        use_hook(move || fp(1));
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
fn Search(token: String, query: Signal<String>, on_open: EventHandler<TitleSummary>) -> Element {
    let results = use_resource(move || {
        let t = token.clone();
        let q = query();
        async move {
            if q.trim().is_empty() {
                return Ok(None);
            }
            api::search(&t, &q).await.map(Some)
        }
    });
    let shown = match &*results.read() {
        Some(Ok(r)) => r.clone(),
        _ => None,
    };
    let err = match &*results.read() {
        Some(Err(e)) => Some(e.message.clone()),
        _ => None,
    };
    rsx! {
        SearchView { results: shown, on_open }
        if let Some(e) = err { {error_text(&e)} }
    }
}

#[component]
fn Mine(
    token: String,
    filters: Signal<MyFilters>,
    on_filters: EventHandler<MyFilters>,
    on_open: EventHandler<TitleSummary>,
) -> Element {
    let items = use_resource(move || {
        let t = token.clone();
        async move { api::my(&t).await }
    });
    let state = items.read().clone();
    match state {
        None => loading(),
        Some(Err(e)) => error_text(&e.message),
        Some(Ok(items)) => rsx! {
            MyView { items, filters: filters(), on_filters, on_open }
        },
    }
}

#[component]
fn Profile(
    token: String,
    name: String,
    avatar_url: Option<String>,
    theme: Theme,
    on_theme: EventHandler<Theme>,
    on_open: EventHandler<(MediaKind, i64)>,
    on_logout: EventHandler<()>,
) -> Element {
    let t1 = token.clone();
    let mut data = use_resource(move || {
        let t = t1.clone();
        async move {
            let history = api::history(&t).await?;
            let preloads = api::preloads(&t).await?;
            let prefs = api::prefs(&t).await?;
            Ok::<_, api::Failure>((history, preloads, prefs))
        }
    });
    let mut prefs_now = use_signal(|| None::<UserPrefs>);
    let state = data.read().clone();
    let (history, preloads, prefs) = match state {
        None => return loading(),
        Some(Err(e)) => return error_text(&e.message),
        Some(Ok(v)) => v,
    };
    let (now, utc_offset_min) = platform::now();
    let t2 = token.clone();
    rsx! {
        ProfileView {
            name,
            avatar_url,
            history,
            preloads,
            prefs: prefs_now().unwrap_or(prefs),
            theme,
            in_telegram: telegram::is_telegram(),
            now,
            utc_offset_min,
            on_theme,
            on_prefs: move |p: UserPrefs| {
                // The studio is no longer chosen here, so an old one must not linger unseen.
                let p = UserPrefs { voice: None, ..p };
                prefs_now.set(Some(p.clone()));
                let t = token.clone();
                spawn(async move {
                    let _ = api::save_prefs(&t, &p).await;
                });
            },
            on_delete_preload: move |pid: i64| {
                let t = t2.clone();
                spawn(async move {
                    if api::delete_preload(&t, pid).await.is_ok() {
                        data.restart();
                    }
                });
            },
            on_refresh: move |_| data.restart(),
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

#[component]
fn Title(
    token: String,
    kind: MediaKind,
    id: i64,
    autoplay: bool,
    on_open: EventHandler<TitleSummary>,
    on_person: EventHandler<i64>,
) -> Element {
    let t1 = token.clone();
    let mut data = use_resource(move || {
        let t = t1.clone();
        async move {
            let details = api::details(&t, kind, id).await?;
            let progress = api::title_progress(&t, kind, id).await?;
            let st = api::title_state(&t, kind, id).await?;
            let choices = api::release_choices(&t, kind, id).await?;
            Ok::<_, api::Failure>((details, progress, st, choices))
        }
    });
    // Cast and suggestions come apart from the title: if TMDB is slow or fails, the page still works.
    let t_extras = token.clone();
    let extras = use_resource(move || {
        let t = t_extras.clone();
        async move { api::extras(&t, kind, id).await.ok() }
    });
    let mut chosen_season = use_signal(|| None::<i32>);
    let t2 = token.clone();
    let season = use_resource(move || {
        let t = t2.clone();
        let picked = chosen_season();
        // Default: the season of the last watched episode, else the first one.
        let default = match &*data.read() {
            Some(Ok((d, p, _, _))) if kind == MediaKind::Tv => {
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
    // Optimistic edits; cleared when the data reloads.
    let mut status_now = use_signal(|| None::<Option<TitleStatus>>);
    let mut rating_now = use_signal(|| None::<Option<i16>>);
    let mut notice = use_signal(|| None::<String>);
    // Choices made in this visit, newest first; they override the loaded ones.
    let mut picked_now = use_signal(Vec::<ReleaseChoiceUpdate>::new);
    let mut picker = use_signal(|| None::<PlayTarget>);
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
    let mut playing = use_signal(|| None::<(PlayTarget, ReleaseChoiceUpdate)>);
    let mut auto = use_signal(|| autoplay);
    let mut overlay_pushed = use_signal(|| false);
    let mut notice_pushed = use_signal(|| false);
    let t0 = token.clone();
    // Releases for the open picker, loaded from the indexer (or demo data).
    let releases = use_resource(move || {
        let t = t0.clone();
        let open = picker();
        async move {
            let p = open?;
            let season = if kind == MediaKind::Tv { p.season } else { 0 };
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
                let updated = filter.peek().clone().or(ReleaseFilter::from_prefs(&p));
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
        if f.language.is_some() {
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
            // Keep the chosen language active even when 0 releases match it.
            filter.set(lang_season);
            return;
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

    let state = data.read().clone();
    let (details, progress, st, loaded) = match state {
        None => return loading(),
        Some(Err(e)) => return error_text(&e.message),
        Some(Ok(v)) => v,
    };
    let status = status_now().unwrap_or(st.status);
    let my_rating = rating_now().unwrap_or(st.rating);
    let mut choices: Vec<ReleaseChoiceUpdate> = picked_now();
    choices.extend(loaded.into_iter().map(|c| c.release));
    let season_key = move |t: &PlayTarget| if kind == MediaKind::Tv { t.season } else { 0 };
    let choice_for = {
        let choices = choices.clone();
        move |s: i32| choices.iter().find(|c| c.season == s).cloned()
    };
    let latest = choices.first().cloned();
    let (target, play_label) = play_target(&details, &progress);
    let release_label = choice_for(season_key(&target)).map(|c| choice_label(&c));
    let watched: Vec<String> = progress
        .iter()
        .filter_map(|p| p.release.as_ref().map(|r| r.infohash.clone()))
        .collect();

    let loaded_prefs_opt = user_prefs.read().clone().flatten();
    let open_picker = {
        let latest = latest.clone();
        let choice_for = choice_for.clone();
        move |t: PlayTarget| {
            // A new season starts pre-filtered to the quality and voice-over picked last time,
            // then the user's default filters, and a series always to the season of the
            // episode about to play.
            let base = match (choice_for(season_key(&t)), &latest) {
                (None, Some(prev)) => ReleaseFilter::like(prev),
                _ => ReleaseFilter::default(),
            };
            prefs_applied.set(loaded_prefs_opt.is_some());
            let prefs_now = loaded_prefs_opt.clone().unwrap_or_default();
            let f = base.or(ReleaseFilter::from_prefs(&prefs_now));
            let f = if kind == MediaKind::Tv {
                f.for_season(t.season)
            } else {
                f
            };
            filter.set(f);
            sort.set(ReleaseSort::from_key(prefs_now.sort.as_deref()));
            defaults_pending.set(true);
            defaults_relaxed.set(false);
            panel.set(None);
            if picker.peek().is_none() && !*overlay_pushed.peek() {
                overlay_pushed.set(true);
                telegram::push_history(None);
            }
            picker.set(Some(t));
        }
    };
    let close_picker = Callback::new(move |_: ()| {
        picker.set(None);
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
    let on_play = Callback::new({
        let choice_for = choice_for.clone();
        let mut open_picker = open_picker.clone();
        move |t: PlayTarget| {
            telegram::haptic(telegram::Haptic::Tap);
            match choice_for(season_key(&t)) {
                Some(c) => {
                    if !*overlay_pushed.peek() {
                        overlay_pushed.set(true);
                        telegram::push_history(None);
                    }
                    playing.set(Some((t, c)));
                }
                None => {
                    open_picker(t);
                }
            }
        }
    });

    let heading = picker().map(|p| {
        let mut h = details.summary.title.clone();
        if kind == MediaKind::Tv {
            h.push_str(&format!(" · {}", kino_core::episode_code(p.season, p.episode)));
        }
        if p.position_sec > 0.0 {
            h.push_str(&format!(" · продолжить с {}", kino_core::clock(p.position_sec)));
        }
        h
    });
    let picker_season = picker().map(|p| season_key(&p));
    let chosen_hash = picker_season.and_then(&choice_for).map(|c| c.infohash);
    let summary = details.summary.clone();
    let t3 = token.clone();
    let main_label = format!("▶ {play_label}");
    let main_target = target.clone();
    rsx! {
        // Inside Telegram: the native big button plays, the native Back closes what is open.
        if playing().is_none() && picker().is_none() && notice().is_none() {
            native::MainButton { text: main_label, on_click: move |_| on_play.call(main_target.clone()) }
        }
        if picker().is_some() {
            native::BackButton { on_back: move |_| close_picker.call(()) }
        }
        if notice().is_some() {
            native::BackButton { on_back: move |_| close_notice.call(()) }
        }
        if auto() {
            AutoPlay {
                on_start: {
                    let t = target.clone();
                    move |_| {
                        auto.set(false);
                        on_play.call(t.clone());
                    }
                },
            }
        }
        TitleView {
            details: details.clone(),
            season: season.read().clone().flatten(),
            progress,
            status,
            my_rating,
            release_label,
            on_play,
            on_change_release: open_picker.clone(),
            on_status: {
                let token = token.clone();
                move |s: Option<TitleStatus>| {
                    telegram::haptic(telegram::Haptic::Select);
                    status_now.set(Some(s));
                    let t = token.clone();
                    spawn(async move {
                        if api::set_status(&t, kind, id, s).await.is_ok() {
                            // A film marked "Просмотрено" also changes its progress.
                            data.restart();
                            status_now.set(None);
                        }
                    });
                }
            },
            on_rate: {
                let token = token.clone();
                move |r: Option<i16>| {
                    telegram::haptic(telegram::Haptic::Select);
                    rating_now.set(Some(r));
                    let t = token.clone();
                    spawn(async move {
                        let _ = api::set_rating(&t, kind, id, r).await;
                    });
                }
            },
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
            on_season: move |n| chosen_season.set(Some(n)),
        }
        if let Some(Some(extras)) = extras.read().clone() {
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
                Some(Ok(list)) => rsx! { ReleasePicker {
                heading,
                releases: list.releases,
                chosen: chosen_hash,
                watched,
                filter: filter(),
                sort: sort(),
                panel: panel(),
                mock: !list.configured,
                note: defaults_relaxed().then(|| match filter().language {
                    Some(l) => format!("По строгим фильтрам ничего не нашлось — показаны все доступные в озвучке «{l}»."),
                    None => "Ваши фильтры по умолчанию ничего не нашли, показаны все раздачи.".to_owned(),
                }),
                on_filter: move |f| filter.set(f),
                on_sort: move |s| sort.set(s),
                on_panel: move |p| panel.set(p),
                on_pick: move |r: ReleaseOption| {
                    let choice = r.to_choice(&summary, season_key(&target));
                    picked_now.with_mut(|v| {
                        v.retain(|c| c.season != choice.season);
                        v.insert(0, choice.clone());
                    });
                    picker.set(None);
                    playing.set(Some((target.clone(), choice.clone())));
                    let t = t3.clone();
                    spawn(async move {
                        let _ = api::save_release_choice(&t, &choice).await;
                    });
                },
                on_preload: {
                    let summary = details.summary.clone();
                    let t4 = token.clone();
                    let pre_season = picker_season.unwrap_or(0);
                    move |r: ReleaseOption| {
                        close_picker.call(());
                        let req = PreloadRequest {
                            kind,
                            tmdb_id: id,
                            season: pre_season,
                            infohash: r.infohash.clone(),
                            title: Some(r.title.clone()),
                        };
                        let t = t4.clone();
                        let name = summary.title.clone();
                        telegram::haptic(telegram::Haptic::Success);
                        spawn(async move {
                            let msg = match api::add_preload(&t, &req).await {
                                Ok(_) => format!("«{name}» поставлен в загрузку на сервер. Следить можно в профиле, раздел «Загрузки»."),
                                Err(e) => format!("Не удалось поставить в загрузку: {}", e.message),
                            };
                            if !*notice_pushed.peek() {
                                notice_pushed.set(true);
                                telegram::push_history(None);
                            }
                            notice.set(Some(msg));
                        });
                    }
                },
                on_close: move |_| close_picker.call(()),
            } },
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
        if let Some((t, rel)) = playing() {
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
                    choices: choices.clone(),
                    release: rel,
                },
                on_close: move |_| {
                    playing.set(None);
                    if *overlay_pushed.peek() {
                        overlay_pushed.set(false);
                        telegram::pop_history();
                    }
                    data.restart();
                },
                on_need_release: {
                    let mut open_picker = open_picker.clone();
                    move |(season, episode): (i32, i32)| {
                        playing.set(None);
                        open_picker(PlayTarget { season, episode, position_sec: 0.0 });
                    }
                },
            }
        }
    }
}
