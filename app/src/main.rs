//! Kino client. One codebase for the website and the Telegram Mini App;
//! desktop and mobile builds come later (see README).
//!
//! The screens live in `kino-ui`; this crate fetches data and handles clicks.

mod api;
mod telegram;

use dioxus::prelude::*;
use kino_core::{AuthResponse, MediaKind, TitleSummary, WidgetAuthRequest};
use kino_ui::{
    mock_releases, HomeView, LoginView, PlayTarget, ReleasePicker, SearchView, Shell, Tab, TitleView, WatchlistView,
    STYLE,
};

fn main() {
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
    let invite = telegram::invite();
    let attempt: Option<Result<AuthResponse, api::Failure>> = if let Some(init) = telegram::init_data() {
        Some(api::login_webapp(init, invite).await)
    } else if let Some(fields) = telegram::widget_fields() {
        let r = api::login_widget(WidgetAuthRequest { fields, invite }).await;
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

    if let Some(token) = api::saved_token() {
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
    use_effect(move || {
        telegram::ready();
        spawn(async move { session.set(establish().await) });
    });

    rsx! {
        style { "{STYLE}" }
        match session() {
            Session::Checking => rsx! { p { class: "muted", style: "padding: 24px", "Загрузка…" } },
            Session::LoggedOut { error } => rsx! { Login { error } },
            Session::LoggedIn { token, name } => rsx! { Main { token, name } },
        }
    }
}

#[component]
fn Login(error: Option<String>) -> Element {
    let config = use_resource(api::public_config);
    use_effect(move || {
        if let Some(Ok(cfg)) = &*config.read() {
            if let Some(bot) = &cfg.bot_username {
                telegram::mount_login_widget("tg-login", bot);
            }
        }
    });
    rsx! {
        LoginView { invite: telegram::invite(), error,
            div { id: "tg-login" }
        }
    }
}

/// What the main area shows.
#[derive(Clone, PartialEq)]
enum View {
    Tab(Tab),
    Title(MediaKind, i64),
}

#[component]
fn Main(token: String, name: String) -> Element {
    let mut view = use_signal(|| View::Tab(Tab::Home));
    let mut query = use_signal(String::new);
    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let tab = match view() {
        View::Tab(t) => Some(t),
        View::Title(..) => None,
    };
    let open = move |t: TitleSummary| view.set(View::Title(t.kind, t.tmdb_id));
    rsx! {
        Shell {
            tab,
            query: query(),
            user_initial: initial,
            on_tab: move |t| view.set(View::Tab(t)),
            on_query: move |q: String| {
                query.set(q);
                view.set(View::Tab(Tab::Search));
            },
            match view() {
                View::Tab(Tab::Home) => rsx! { Home { token: token.clone(), on_open: open } },
                View::Tab(Tab::Search) => rsx! { Search { token: token.clone(), query, on_open: open } },
                View::Tab(Tab::Watchlist) => rsx! { Watchlist { token: token.clone(), on_open: open } },
                View::Title(kind, id) => rsx! { Title { key: "{kind.as_str()}{id}", token: token.clone(), kind, id } },
            }
        }
    }
}

fn error_text(message: &str) -> Element {
    rsx! { p { class: "error section", "{message}" } }
}

fn loading() -> Element {
    rsx! { p { class: "muted section", "Загрузка…" } }
}

#[component]
fn Home(token: String, on_open: EventHandler<TitleSummary>) -> Element {
    let data = use_resource(move || {
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
                // Opens the title; its main button resumes where the user stopped.
                on_play: move |c: kino_core::ContinueItem| {
                    if let Some(t) = c.title { on_open.call(t) }
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
        SearchView { query: query(), results: shown, on_open, on_query: move |q| query.set(q) }
        if let Some(e) = err { {error_text(&e)} }
    }
}

#[component]
fn Watchlist(token: String, on_open: EventHandler<TitleSummary>) -> Element {
    let items = use_resource(move || {
        let t = token.clone();
        async move {
            let list = api::watchlist(&t).await?;
            let mut out = Vec::new();
            for w in list {
                if let Ok(d) = api::details(&t, w.kind, w.tmdb_id).await {
                    out.push(d.summary);
                }
            }
            Ok::<_, api::Failure>(out)
        }
    });
    let state = items.read().clone();
    match state {
        None => loading(),
        Some(Err(e)) => error_text(&e.message),
        Some(Ok(items)) => rsx! { WatchlistView { items, on_open } },
    }
}

#[component]
fn Title(token: String, kind: MediaKind, id: i64) -> Element {
    let t1 = token.clone();
    let data = use_resource(move || {
        let t = t1.clone();
        async move {
            let details = api::details(&t, kind, id).await?;
            let progress = api::title_progress(&t, kind, id).await?;
            let in_list = api::watchlist(&t)
                .await?
                .iter()
                .any(|w| w.kind == kind && w.tmdb_id == id);
            Ok::<_, api::Failure>((details, progress, in_list))
        }
    });
    let mut chosen_season = use_signal(|| None::<i32>);
    let t2 = token.clone();
    let season = use_resource(move || {
        let t = t2.clone();
        let picked = chosen_season();
        // Default: the season of the last watched episode, else the first one.
        let default = match &*data.read() {
            Some(Ok((d, p, _))) if kind == MediaKind::Tv => {
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
    let mut in_list = use_signal(|| None::<bool>);
    let mut picker = use_signal(|| None::<PlayTarget>);

    let guard = data.read();
    let (details, progress, listed) = match &*guard {
        None => return loading(),
        Some(Err(e)) => return error_text(&e.message),
        Some(Ok(v)) => v.clone(),
    };
    drop(guard);
    let listed = in_list().unwrap_or(listed);
    let last_release = progress
        .iter()
        .max_by_key(|p| p.updated_at)
        .and_then(|p| p.release.clone());
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
    let season_for_releases = picker().filter(|_| kind == MediaKind::Tv).map(|p| p.season);
    rsx! {
        TitleView {
            details: details.clone(),
            season: season.read().clone().flatten(),
            progress,
            in_watchlist: listed,
            on_play: move |p| picker.set(Some(p)),
            on_toggle_watchlist: move |on: bool| {
                in_list.set(Some(on));
                let t = token.clone();
                spawn(async move {
                    if api::set_watchlist(&t, kind, id, on).await.is_err() {
                        in_list.set(Some(!on));
                    }
                });
            },
            on_season: move |n| chosen_season.set(Some(n)),
        }
        if let Some(heading) = heading {
            ReleasePicker {
                heading,
                releases: mock_releases(&details.summary, season_for_releases),
                last_infohash: last_release.map(|r| r.infohash),
                mock: true,
                // Playback arrives in stage 3.
                on_pick: move |_| picker.set(None),
                on_preload: move |_| picker.set(None),
                on_close: move |_| picker.set(None),
            }
        }
    }
}
