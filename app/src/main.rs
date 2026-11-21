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
    image_url, AuthResponse, ImageSize, MarkWatched, MediaKind, PreloadRequest, ReleaseChoiceUpdate, TitleStatus,
    TitleSummary, UserPrefs, WidgetAuthRequest,
};
use kino_ui::{
    play_target, HomeView, LoginView, MyView, PersonView, PickerPanel, PlayTarget, ProfileView, ReleaseFilter,
    ReleaseOption, ReleasePicker, ReleaseSort, SearchView, Shell, Tab, Theme, TitleExtrasView, TitleView, FAVICON_URI,
    STYLE,
};

const FONTS_URL: &str = "https://fonts.googleapis.com/css2?family=Golos+Text:wght@400;500;600;700&family=Playfair+Display:ital,wght@0,500;0,600;0,700;1,500&display=swap";
const THEME_KEY: &str = "kino.theme";

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
    LoggedOut { error: Option<String> },
    LoggedIn { token: String, name: String },
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
            Session::LoggedIn { token, name } => rsx! {
                Main {
                    token,
                    name,
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
    rsx! {
        LoginView { invite: telegram::invite(), error: shown_error,
            match (state(), ready) {
                (LinkLogin::Waiting, Some(l)) => rsx! {
                    p { "Откройте Telegram и нажмите «Войти на сайт» в сообщении бота. Эта страница откроется сама." }
                    a {
                        class: "btn ghost",
                        href: "{l.url}",
                        target: "_blank",
                        rel: "noopener",
                        onclick: move |e: Event<MouseData>| open_link(&e, &l.url),
                        "Открыть Telegram ещё раз"
                    }
                    p {
                        a { href: "#", onclick: move |e: Event<MouseData>| { e.prevent_default(); state.set(LinkLogin::Idle) }, "Отмена" }
                    }
                },
                (_, Some(l)) => rsx! {
                    a { class: "btn primary", href: "{l.url}", target: "_blank", rel: "noopener",
                        onclick: move |e: Event<MouseData>| {
                            open_link(&e, &l.url);
                            wait(l.token.clone());
                        },
                        "Войти через Telegram"
                    }
                    if !platform::is_web() {
                        input {
                            class: "field",
                            placeholder: "Код приглашения (если есть)",
                            oninput: move |e| invite.set(Some(e.value().trim().to_owned()).filter(|v| !v.is_empty())),
                        }
                    }
                },
                (_, None) => rsx! { p { class: "muted", "Готовим вход…" } },
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
}

#[component]
fn Main(
    token: String,
    name: String,
    theme: Theme,
    on_theme: EventHandler<Theme>,
    on_logout: EventHandler<()>,
) -> Element {
    // A link to a title (from a bot message) opens it, with Home behind it.
    let link = use_hook(telegram::open_target);
    let mut view = use_signal(|| match link {
        Some((kind, id)) => View::Title(kind, id, false),
        None => View::Tab(Tab::Home),
    });
    // Where "Back" goes from a title page.
    let mut trail = use_signal(|| link.map(|_| vec![View::Tab(Tab::Home)]).unwrap_or_default());
    let mut query = use_signal(String::new);
    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let tab = match view() {
        View::Tab(t) => Some(t),
        View::Title(..) | View::Person(_) => None,
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
    };
    rsx! {
        if !trail().is_empty() {
            native::BackButton {
                on_back: move |_| {
                    telegram::haptic(telegram::Haptic::Tap);
                    if let Some(prev) = trail.with_mut(|t| t.pop()) {
                        view.set(prev);
                    }
                },
            }
        }
        Shell {
            tab,
            query: query(),
            user_initial: initial,
            on_tab: move |t| {
                telegram::haptic(telegram::Haptic::Tap);
                trail.set(Vec::new());
                query.set(String::new());
                view.set(View::Tab(t));
            },
            on_query: move |q: String| {
                trail.set(Vec::new());
                view.set(View::Tab(if q.trim().is_empty() { Tab::Home } else { Tab::Search }));
                query.set(q);
            },
            match view() {
                View::Tab(Tab::Home) => rsx! {
                    Home { token: token.clone(), on_open: open, on_play: move |(k, id)| open_title(k, id, true) }
                },
                View::Tab(Tab::Search) => rsx! { Search { token: token.clone(), query, on_open: open } },
                View::Tab(Tab::Mine) => rsx! { Mine { token: token.clone(), on_open: open } },
                View::Tab(Tab::Profile) => rsx! {
                    Profile {
                        token: token.clone(),
                        name: name.clone(),
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
                        // Closing a player that was started from Home goes back to Home.
                        on_exit: move |_| {
                            if let Some(prev) = trail.with_mut(|t| t.pop()) {
                                view.set(prev);
                            }
                        },
                    }
                },
                View::Person(id) => rsx! { Person { key: "person{id}", token: token.clone(), id, on_open: open } },
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

#[component]
fn Home(token: String, on_open: EventHandler<TitleSummary>, on_play: EventHandler<(MediaKind, i64)>) -> Element {
    let t2 = token.clone();
    let mut data = use_resource(move || {
        let t = token.clone();
        async move {
            let feed = api::home(&t).await?;
            let cont = api::continue_watching(&t).await?;
            Ok::<_, api::Failure>((feed, cont))
        }
    });
    let state = data.read().clone();
    match state {
        None => loading(),
        Some(Err(e)) => error_text(&e.message),
        Some(Ok((feed, cont))) => rsx! {
            HomeView {
                feed,
                continue_items: cont,
                on_open,
                // Plays at once from where the user stopped, without the title page in between.
                on_play: move |c: kino_core::ContinueItem| on_play.call((c.last.kind, c.last.tmdb_id)),
                on_drop: move |c: kino_core::ContinueItem| {
                    let t = t2.clone();
                    spawn(async move {
                        if api::set_status(&t, c.last.kind, c.last.tmdb_id, Some(TitleStatus::Dropped)).await.is_ok() {
                            data.restart();
                        }
                    });
                },
            }
        },
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
fn Mine(token: String, on_open: EventHandler<TitleSummary>) -> Element {
    let mut filter = use_signal(|| None::<TitleStatus>);
    let items = use_resource(move || {
        let t = token.clone();
        async move { api::my(&t).await }
    });
    let state = items.read().clone();
    match state {
        None => loading(),
        Some(Err(e)) => error_text(&e.message),
        Some(Ok(items)) => rsx! {
            MyView { items, filter: filter(), on_filter: move |f| filter.set(f), on_open }
        },
    }
}

#[component]
fn Profile(
    token: String,
    name: String,
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
    on_exit: EventHandler<()>,
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
    let t_prefs = token.clone();
    let user_prefs = use_resource(move || {
        let t = t_prefs.clone();
        async move { api::prefs(&t).await.ok() }
    });
    let mut sort = use_signal(ReleaseSort::default);
    let mut panel = use_signal(|| None::<PickerPanel>);
    let mut playing = use_signal(|| None::<(PlayTarget, ReleaseChoiceUpdate)>);
    let mut auto = use_signal(|| autoplay);
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

    // Once the releases are in: when the default filters leave nothing, drop them
    // (the season of the episode stays) and say so.
    use_effect(move || {
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
        let season_only = match (kind, picker.peek().as_ref()) {
            (MediaKind::Tv, Some(t)) => ReleaseFilter::default().for_season(t.season),
            _ => ReleaseFilter::default(),
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

    let loaded_prefs = user_prefs.read().clone().flatten().unwrap_or_default();
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
            let f = base.or(ReleaseFilter::from_prefs(&loaded_prefs));
            let f = if kind == MediaKind::Tv {
                f.for_season(t.season)
            } else {
                f
            };
            filter.set(f);
            sort.set(ReleaseSort::from_key(loaded_prefs.sort.as_deref()));
            defaults_pending.set(true);
            defaults_relaxed.set(false);
            panel.set(None);
            picker.set(Some(t));
        }
    };
    let on_play = Callback::new({
        let choice_for = choice_for.clone();
        let mut open_picker = open_picker.clone();
        move |t: PlayTarget| {
            telegram::haptic(telegram::Haptic::Tap);
            match choice_for(season_key(&t)) {
                Some(c) => playing.set(Some((t, c))),
                None => open_picker(t),
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
            native::BackButton { on_back: move |_| picker.set(None) }
        }
        if notice().is_some() {
            native::BackButton { on_back: move |_| notice.set(None) }
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
                    div { class: "sheet-bg", onclick: move |_| picker.set(None),
                        div { class: "sheet", onclick: move |e| e.stop_propagation(),
                            h3 { "{heading}" }
                            p { class: "muted", "Ищем раздачи на трекерах…" }
                        }
                    }
                },
                Some(Err(e)) => rsx! {
                    div { class: "sheet-bg", onclick: move |_| picker.set(None),
                        div { class: "sheet", onclick: move |e| e.stop_propagation(),
                            h3 { "{heading}" }
                            p { class: "error", "{e.message}" }
                            button { class: "btn ghost", onclick: move |_| picker.set(None), "Закрыть" }
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
                note: defaults_relaxed().then(|| "Ваши фильтры по умолчанию ничего не нашли, показаны все раздачи.".to_owned()),
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
                        picker.set(None);
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
                            notice.set(Some(match api::add_preload(&t, &req).await {
                                Ok(_) => format!("«{name}» поставлен в загрузку на сервер. Следить можно в профиле, раздел «Загрузки»."),
                                Err(e) => format!("Не удалось поставить в загрузку: {}", e.message),
                            }));
                        });
                    }
                },
                on_close: move |_| picker.set(None),
            } },
            }
        }
        if let Some(text) = notice() {
            div { class: "sheet-bg", onclick: move |_| notice.set(None),
                div { class: "sheet", onclick: move |e| e.stop_propagation(),
                    p { "{text}" }
                    button { class: "btn ghost", onclick: move |_| notice.set(None), "Хорошо" }
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
                    if autoplay {
                        on_exit.call(());
                    } else {
                        data.restart();
                    }
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
