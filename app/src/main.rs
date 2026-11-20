//! Kino client. One codebase for the website and the Telegram Mini App;
//! desktop and mobile builds come later (see README).
//!
//! The screens live in `kino-ui`; this crate fetches data and handles clicks.

mod api;
mod platform;
mod player;
mod telegram;

use player::{PlayStart, Player};

use dioxus::prelude::*;
use kino_core::{
    image_url, AuthResponse, ImageSize, LoginPoll, MarkWatched, MediaKind, PreloadRequest, ReleaseChoiceUpdate,
    TitleStatus, TitleSummary, UserPrefs, WidgetAuthRequest,
};
use kino_ui::{
    play_target, HomeView, LoginView, MyView, PickerPanel, PlayTarget, ProfileView, ReleaseFilter, ReleaseOption,
    ReleasePicker, ReleaseSort, SearchView, Shell, Tab, TitleView, STYLE,
};

fn main() {
    #[cfg(target_arch = "wasm32")]
    dioxus::launch(App);

    #[cfg(not(target_arch = "wasm32"))]
    {
        // The WebView's own page: let the picture go under the notch and system bars
        // (the screens add the safe-area padding themselves).
        const HEAD: &str = r#"<script>
            var m = document.querySelector('meta[name=viewport]');
            if (m) { m.content += ', viewport-fit=cover'; }
        </script>"#;
        #[cfg(all(feature = "desktop", not(feature = "mobile")))]
        use dioxus::desktop::Config;
        #[cfg(feature = "mobile")]
        use dioxus::mobile::Config;
        dioxus::LaunchBuilder::new()
            .with_cfg(Config::new().with_custom_head(HEAD.to_owned()))
            .launch(App);
    }
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
    use_effect(move || {
        telegram::ready();
        spawn(async move { session.set(establish().await) });
    });

    rsx! {
        style { "{STYLE}" }
        match session() {
            Session::Checking => rsx! { p { class: "muted", style: "padding: 24px", "Загрузка…" } },
            Session::LoggedOut { error } => rsx! {
                Login {
                    error,
                    on_done: move |auth: AuthResponse| {
                        api::save_token(&auth.token);
                        session.set(Session::LoggedIn {
                            token: auth.token,
                            name: auth.me.first_name.unwrap_or_default(),
                        });
                    },
                }
            },
            Session::LoggedIn { token, name } => rsx! {
                Main {
                    token,
                    name,
                    on_logout: move |_| {
                        api::forget_token();
                        session.set(Session::LoggedOut { error: None });
                    },
                }
            },
        }
    }
}

#[component]
fn Login(error: Option<String>, on_done: EventHandler<AuthResponse>) -> Element {
    if !platform::is_web() {
        return rsx! { AppLogin { error, on_done } };
    }
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

/// Login of the phone and desktop apps: confirm in the Telegram bot, then pick up the session.
#[component]
fn AppLogin(error: Option<String>, on_done: EventHandler<AuthResponse>) -> Element {
    let mut invite = use_signal(String::new);
    let mut waiting = use_signal(|| false);
    let mut failure = use_signal(|| error.clone());
    let mut link = use_signal(|| None::<kino_core::LoginLink>);

    let start = move |_| {
        spawn(async move {
            failure.set(None);
            waiting.set(true);
            let code = invite().trim().to_owned();
            let l = match api::login_link((!code.is_empty()).then_some(code)).await {
                Ok(l) => l,
                Err(e) => {
                    failure.set(Some(e.message));
                    waiting.set(false);
                    return;
                }
            };
            platform::open_external(&l.url);
            let code = l.code.clone();
            let ttl = l.expires_in;
            link.set(Some(l));
            let mut waited = 0;
            while waited < ttl * 1000 {
                platform::sleep(2000).await;
                waited += 2000;
                match api::login_poll(&code).await {
                    Ok(LoginPoll::Pending) => {}
                    Ok(LoginPoll::Done(auth)) => {
                        waiting.set(false);
                        link.set(None);
                        on_done.call(auth);
                        return;
                    }
                    Ok(LoginPoll::NotInvited) => {
                        failure.set(Some(
                            "Доступ только по приглашению. Введите код приглашения выше или попросите его у администратора.".into(),
                        ));
                        break;
                    }
                    Err(e) if e.code == "network" => {}
                    Err(e) => {
                        failure.set(Some(e.message));
                        break;
                    }
                }
            }
            waiting.set(false);
            link.set(None);
        });
    };

    rsx! {
        LoginView { invite: None, error: failure(),
            if waiting() {
                p { class: "muted", "Откройте бота в Telegram и нажмите «Запустить» (Start). Приложение войдёт само." }
                if let Some(l) = link() {
                    button { class: "btn primary", onclick: move |_| platform::open_external(&l.url), "Открыть Telegram ещё раз" }
                }
                div { class: "spinner" }
            } else {
                input {
                    class: "field",
                    placeholder: "Код приглашения (если есть)",
                    value: "{invite}",
                    oninput: move |e| invite.set(e.value()),
                }
                button { class: "btn primary", onclick: start, "Войти через Telegram" }
            }
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
fn Main(token: String, name: String, on_logout: EventHandler<()>) -> Element {
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
                View::Tab(Tab::Mine) => rsx! { Mine { token: token.clone(), on_open: open } },
                View::Tab(Tab::Profile) => rsx! {
                    Profile {
                        token: token.clone(),
                        name: name.clone(),
                        on_open: move |(k, id)| view.set(View::Title(k, id)),
                        on_logout,
                    }
                },
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
                // Opens the title; its main button resumes where the user stopped.
                on_play: move |c: kino_core::ContinueItem| {
                    if let Some(t) = c.title { on_open.call(t) }
                },
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
        SearchView { query: query(), results: shown, on_open, on_query: move |q| query.set(q) }
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
            now,
            utc_offset_min,
            on_prefs: move |p: UserPrefs| {
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
fn Title(token: String, kind: MediaKind, id: i64) -> Element {
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
    let mut sort = use_signal(ReleaseSort::default);
    let mut panel = use_signal(|| None::<PickerPanel>);
    let mut playing = use_signal(|| None::<(PlayTarget, ReleaseChoiceUpdate)>);
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
    let (target, _) = play_target(&details, &progress);
    let release_label = choice_for(season_key(&target)).map(|c| choice_label(&c));
    let watched: Vec<String> = progress
        .iter()
        .filter_map(|p| p.release.as_ref().map(|r| r.infohash.clone()))
        .collect();

    let open_picker = {
        let latest = latest.clone();
        let choice_for = choice_for.clone();
        move |t: PlayTarget| {
            // A new season starts pre-filtered to the quality and voice-over picked last time.
            let f = match (choice_for(season_key(&t)), &latest) {
                (None, Some(prev)) => ReleaseFilter::like(prev),
                _ => ReleaseFilter::default(),
            };
            filter.set(f);
            panel.set(None);
            picker.set(Some(t));
        }
    };
    let on_play = {
        let choice_for = choice_for.clone();
        let mut open_picker = open_picker.clone();
        move |t: PlayTarget| match choice_for(season_key(&t)) {
            Some(c) => playing.set(Some((t, c))),
            None => open_picker(t),
        }
    };

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
    rsx! {
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
