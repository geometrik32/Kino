//! Kino client. One codebase for the website and the Telegram Mini App;
//! desktop and mobile builds come later (see README).

mod api;
mod telegram;

use dioxus::prelude::*;
use kino_core::{AuthResponse, ContinueItem, MediaKind, WidgetAuthRequest};

const CSS: Asset = asset!("/assets/main.css");

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
        document::Stylesheet { href: CSS }
        main {
            match session() {
                Session::Checking => rsx! { p { class: "muted", "Загрузка…" } },
                Session::LoggedOut { error } => rsx! { Login { error } },
                Session::LoggedIn { token, name } => rsx! {
                    Home {
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
        h1 { "Кино" }
        p { "Войдите через Telegram, чтобы видеть свою историю и продолжать с того же места на любом устройстве." }
        if let Some(e) = error { p { class: "error", "{e}" } }
        div { id: "tg-login" }
    }
}

fn clock(sec: f64) -> String {
    let s = sec.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[component]
fn Home(token: String, name: String, on_logout: EventHandler<()>) -> Element {
    let t1 = token.clone();
    let items = use_resource(move || {
        let t = t1.clone();
        async move { api::continue_watching(&t).await }
    });
    let watchlist = use_resource(move || {
        let t = token.clone();
        async move { api::watchlist(&t).await }
    });

    rsx! {
        h1 { "Привет, {name}" }
        h2 { "Продолжить просмотр" }
        match &*items.read() {
            None => rsx! { p { class: "muted", "Загрузка…" } },
            Some(Err(e)) => rsx! { p { class: "error", "{e.message}" } },
            Some(Ok(list)) if list.is_empty() => rsx! { p { class: "muted", "Пока пусто. Начните смотреть что-нибудь, и оно появится здесь." } },
            Some(Ok(list)) => rsx! {
                div { class: "cards",
                    for item in list.iter().cloned() {
                        ContinueCard { item }
                    }
                }
            },
        }
        h2 { "Хочу посмотреть" }
        match &*watchlist.read() {
            Some(Ok(list)) => rsx! { p { class: "muted", "В списке: {list.len()}" } },
            Some(Err(e)) => rsx! { p { class: "error", "{e.message}" } },
            None => rsx! {},
        }
        p { class: "muted", "Каталог и плеер появятся на следующих этапах." }
        button { onclick: move |_| on_logout.call(()), "Выйти" }
    }
}

#[component]
fn ContinueCard(item: ContinueItem) -> Element {
    let p = &item.last;
    // Titles and posters come from TMDB in the next stage; for now show ids.
    let what = match p.kind {
        MediaKind::Movie => format!("Фильм #{}", p.tmdb_id),
        MediaKind::Tv => format!("Сериал #{} · S{:02}E{:02}", p.tmdb_id, p.season, p.episode),
    };
    let detail = match item.next_episode {
        Some((s, e)) => format!("Дальше: S{s:02}E{e:02}"),
        None => format!("Остановились на {} из {}", clock(p.position_sec), clock(p.duration_sec)),
    };
    let pct = if p.duration_sec > 0.0 {
        (p.position_sec / p.duration_sec * 100.0).min(100.0)
    } else {
        0.0
    };
    let release = p.release.as_ref().and_then(|r| r.title.clone()).unwrap_or_default();
    rsx! {
        div { class: "card",
            div { "{what}" }
            div { class: "muted", "{detail}" }
            if !release.is_empty() { div { class: "muted", "{release}" } }
            div { class: "bar", div { style: "width: {pct}%" } }
        }
    }
}
