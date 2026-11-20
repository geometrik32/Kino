//! Telegram bot: `/start` opens the app (optionally carrying an invite), `/search`
//! (or just a title typed in the chat), `/continue`, `/my`, `/login` for the
//! website, `/help`, and `/invite` for admins.

use std::sync::Arc;

use anyhow::Context;
use kino_core::{MediaKind, TitleStatus};
use kino_server::{
    catalog::{self, Catalog},
    config::{env_value, parse_ids},
    db,
    telegram_auth::TelegramUser,
};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use teloxide::{
    prelude::*,
    types::{
        BotCommand, BotCommandScope, CallbackQuery, ChatAction, InlineKeyboardButton, InlineKeyboardMarkup, MenuButton,
        ParseMode, Recipient, WebAppInfo,
    },
    utils::command::BotCommands,
};
use tracing_subscriber::EnvFilter;
use url::Url;

mod screens;
use screens::Screen;

/// `/start` payload prefix for invites: `t.me/<bot>?start=inv_<code>`.
const INVITE_PREFIX: &str = "inv_";

mod commands {
    // teloxide's `BotCommands` derive emits code that Rust 1.100+ reports as unreachable.
    #![allow(unreachable_code)]

    use teloxide::utils::command::BotCommands;

    #[derive(BotCommands, Clone, Debug, PartialEq)]
    #[command(rename_rule = "lowercase", description = "Команды:")]
    pub enum Command {
        #[command(description = "открыть кинотеатр")]
        Start(String),
        #[command(description = "найти фильм или сериал")]
        Search(String),
        #[command(description = "продолжить просмотр")]
        Continue,
        #[command(description = "мои списки: смотрю, хочу, просмотрено")]
        My,
        #[command(description = "войти на сайт в браузере")]
        Login,
        #[command(description = "что умеет бот")]
        Help,
        #[command(description = "создать приглашение (только админ)", hide)]
        Invite,
    }
}

use commands::Command;

struct BotState {
    pool: PgPool,
    catalog: Arc<dyn Catalog>,
    public_url: Url,
    admin_ids: Vec<i64>,
    bot_username: String,
}

/// The app URL, with `?invite=<code>` when the user came through an invite.
fn app_url(base: &Url, start_payload: &str) -> Url {
    let mut url = base.clone();
    if let Some(code) = start_payload.trim().strip_prefix(INVITE_PREFIX) {
        if !code.is_empty() && code.chars().all(|c| c.is_ascii_alphanumeric()) {
            url.query_pairs_mut().append_pair("invite", code);
        }
    }
    url
}

fn open_button(url: Url) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new([[InlineKeyboardButton::web_app("Открыть кинопоказ", WebAppInfo { url })]])
}

async fn is_admin(state: &BotState, telegram_id: i64) -> anyhow::Result<bool> {
    if state.admin_ids.contains(&telegram_id) {
        return Ok(true);
    }
    let row = sqlx::query("SELECT role FROM users WHERE telegram_id = $1")
        .bind(telegram_id)
        .fetch_optional(&state.pool)
        .await?;
    Ok(row.map(|r| r.get::<String, _>("role") == "admin").unwrap_or(false))
}

/// Callback data of the "confirm website login" button.
const LOGIN_CALLBACK: &str = "login:";

/// `/start login_<token>`: ask before logging the website in, so a link someone
/// else made can't silently log them in as you.
async fn ask_login(bot: &Bot, msg: &Message, token: &str, state: &BotState) -> ResponseResult<()> {
    let site = state.public_url.host_str().unwrap_or("сайт").to_owned();
    let button = InlineKeyboardButton::callback("Войти на сайт", format!("{LOGIN_CALLBACK}{token}"));
    bot.send_message(
        msg.chat.id,
        format!(
            "Вход на {site}.\nНажмите кнопку, и сайт откроется уже с вашим аккаунтом. \
             Если вы сейчас не входили на сайт, ничего не нажимайте."
        ),
    )
    .reply_markup(InlineKeyboardMarkup::new([[button]]))
    .await?;
    Ok(())
}

async fn handle_callback(bot: Bot, q: CallbackQuery, state: Arc<BotState>) -> ResponseResult<()> {
    let data = q.data.clone().unwrap_or_default();
    if let Some(which) = data.strip_prefix(screens::MY_CALLBACK) {
        return my_callback(&bot, q, which, &state).await;
    }
    let Some(token) = data.strip_prefix(LOGIN_CALLBACK) else {
        bot.answer_callback_query(q.id).await?;
        return Ok(());
    };
    let tg = TelegramUser {
        id: q.from.id.0 as i64,
        first_name: Some(q.from.first_name.clone()),
        last_name: q.from.last_name.clone(),
        username: q.from.username.clone(),
        photo_url: None,
    };
    let text = match db::confirm_login_link(&state.pool, token, &tg).await {
        Ok(true) => "Готово, вы вошли. Вернитесь на сайт.",
        Ok(false) => "Эта ссылка для входа устарела. Нажмите «Войти через Telegram» на сайте ещё раз.",
        Err(e) => {
            tracing::error!(error = %e, "confirming login failed");
            "Что-то пошло не так, попробуйте ещё раз."
        }
    };
    bot.answer_callback_query(q.id.clone()).await?;
    if let Some(message) = q.regular_message() {
        bot.edit_message_text(message.chat.id, message.id, text).await?;
    } else {
        bot.send_message(q.from.id, text).await?;
    }
    Ok(())
}

/// Sends a screen as a new message.
async fn send_screen(bot: &Bot, chat: ChatId, screen: Screen) -> ResponseResult<()> {
    let mut req = bot.send_message(chat, screen.text).parse_mode(ParseMode::Html);
    if !screen.keyboard.inline_keyboard.is_empty() {
        req = req.reply_markup(screen.keyboard);
    }
    req.await?;
    Ok(())
}

async fn say(bot: &Bot, chat: ChatId, text: &str) -> ResponseResult<()> {
    bot.send_message(chat, text).parse_mode(ParseMode::Html).await?;
    Ok(())
}

/// How long to wait for the catalog before showing a title without its name.
const CATALOG_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

async fn title_name(state: &BotState, kind: MediaKind, tmdb_id: i64) -> Option<String> {
    tokio::time::timeout(CATALOG_WAIT, state.catalog.details(kind, tmdb_id))
        .await
        .ok()?
        .ok()
        .map(|d| d.summary.title)
}

fn tg_user(u: &teloxide::types::User) -> TelegramUser {
    TelegramUser {
        id: u.id.0 as i64,
        first_name: Some(u.first_name.clone()),
        last_name: u.last_name.clone(),
        username: u.username.clone(),
        photo_url: None,
    }
}

async fn search(bot: &Bot, msg: &Message, query: &str, state: &BotState) -> ResponseResult<()> {
    let query = query.trim();
    if query.is_empty() {
        return say(bot, msg.chat.id, &screens::search_prompt()).await;
    }
    bot.send_chat_action(msg.chat.id, ChatAction::Typing).await?;
    let found = tokio::time::timeout(CATALOG_WAIT, state.catalog.search(query)).await;
    match found {
        Ok(Ok(results)) => {
            send_screen(
                bot,
                msg.chat.id,
                screens::search_results(&state.public_url, query, &results.items),
            )
            .await
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "search failed");
            say(bot, msg.chat.id, "Поиск сейчас не отвечает, попробуйте чуть позже.").await
        }
        Err(_) => say(bot, msg.chat.id, "Поиск отвечает слишком долго, попробуйте чуть позже.").await,
    }
}

async fn continue_watching(bot: &Bot, msg: &Message, state: &BotState) -> ResponseResult<()> {
    let Some(from) = msg.from.as_ref() else { return Ok(()) };
    let user = match db::user_by_telegram(&state.pool, from.id.0 as i64).await {
        Ok(Some(user)) => user,
        Ok(None) => return send_screen(bot, msg.chat.id, screens::not_registered(&state.public_url)).await,
        Err(e) => return db_failed(bot, msg.chat.id, e).await,
    };
    let items = match db::continue_watching(&state.pool, user.id, screens::LIST_LIMIT as i64).await {
        Ok(items) => items,
        Err(e) => return db_failed(bot, msg.chat.id, e).await,
    };
    bot.send_chat_action(msg.chat.id, ChatAction::Typing).await?;
    let mut rows = Vec::new();
    for item in items {
        let title = title_name(state, item.last.kind, item.last.tmdb_id).await;
        rows.push(screens::ContinueRow { item, title });
    }
    send_screen(bot, msg.chat.id, screens::continue_list(&state.public_url, &rows)).await
}

async fn db_failed(bot: &Bot, chat: ChatId, e: sqlx::Error) -> ResponseResult<()> {
    tracing::error!(error = %e, "database query failed");
    say(bot, chat, "Что-то пошло не так, попробуйте ещё раз.").await
}

/// The "Мои" menu: a button per status with how many titles it holds.
async fn my_menu(state: &BotState, user_id: i64) -> Result<Screen, sqlx::Error> {
    let all = kino_server::userdata::list_states(&state.pool, user_id, None).await?;
    let counts: Vec<_> = TitleStatus::ALL
        .iter()
        .map(|s| (*s, all.iter().filter(|t| t.status == Some(*s)).count()))
        .collect();
    Ok(screens::my_menu(&counts))
}

async fn my_list(state: &BotState, user_id: i64, status: TitleStatus) -> Result<Screen, sqlx::Error> {
    let all = kino_server::userdata::list_states(&state.pool, user_id, Some(status)).await?;
    let mut titles = Vec::new();
    for t in all.iter().take(screens::LIST_LIMIT) {
        let name = title_name(state, t.kind, t.tmdb_id)
            .await
            .unwrap_or_else(|| "Без названия".to_owned());
        titles.push((t.kind, t.tmdb_id, name));
    }
    Ok(screens::my_list(&state.public_url, status, &titles, all.len()))
}

async fn my(bot: &Bot, msg: &Message, state: &BotState) -> ResponseResult<()> {
    let Some(from) = msg.from.as_ref() else { return Ok(()) };
    match db::user_by_telegram(&state.pool, from.id.0 as i64).await {
        Ok(Some(user)) => match my_menu(state, user.id).await {
            Ok(screen) => send_screen(bot, msg.chat.id, screen).await,
            Err(e) => db_failed(bot, msg.chat.id, e).await,
        },
        Ok(None) => send_screen(bot, msg.chat.id, screens::not_registered(&state.public_url)).await,
        Err(e) => db_failed(bot, msg.chat.id, e).await,
    }
}

/// A tap in the "Мои" screen turns the same message into a list, or back to the menu.
async fn my_callback(bot: &Bot, q: CallbackQuery, which: &str, state: &BotState) -> ResponseResult<()> {
    bot.answer_callback_query(q.id.clone()).await?;
    let Some(message) = q.regular_message() else {
        return Ok(());
    };
    let screen = match db::user_by_telegram(&state.pool, q.from.id.0 as i64).await {
        Ok(Some(user)) => match TitleStatus::parse(which) {
            Some(status) => my_list(state, user.id, status).await,
            None => my_menu(state, user.id).await,
        },
        Ok(None) => return Ok(()),
        Err(e) => Err(e),
    };
    match screen {
        Ok(screen) => {
            let mut req = bot
                .edit_message_text(message.chat.id, message.id, screen.text)
                .parse_mode(ParseMode::Html);
            if !screen.keyboard.inline_keyboard.is_empty() {
                req = req.reply_markup(screen.keyboard);
            }
            // Tapping the same list twice changes nothing, and Telegram calls that an error.
            if let Err(e) = req.await {
                tracing::debug!(error = %e, "editing the lists message");
            }
        }
        Err(e) => tracing::error!(error = %e, "loading lists failed"),
    }
    Ok(())
}

/// `/login`: a one-time link that signs the browser in as this Telegram account.
async fn site_login(bot: &Bot, msg: &Message, state: &BotState) -> ResponseResult<()> {
    let Some(from) = msg.from.as_ref() else { return Ok(()) };
    if !msg.chat.is_private() {
        return say(
            bot,
            msg.chat.id,
            "Ссылку для входа я пришлю только в личных сообщениях.",
        )
        .await;
    }
    let tg = tg_user(from);
    // A newcomer would be turned away by the site anyway; say why here.
    let known = match db::user_by_telegram(&state.pool, tg.id).await {
        Ok(user) => user.is_some() || state.admin_ids.contains(&tg.id),
        Err(e) => return db_failed(bot, msg.chat.id, e).await,
    };
    if !known {
        return say(
            bot,
            msg.chat.id,
            "Вход только по приглашению. Попросите у администратора ссылку-приглашение и откройте её.",
        )
        .await;
    }
    let made = async {
        let code = db::create_login_link(&state.pool, None).await?;
        db::confirm_login_link(&state.pool, &code, &tg).await?;
        Ok::<_, sqlx::Error>(code)
    }
    .await;
    match made {
        Ok(code) => send_screen(bot, msg.chat.id, screens::login_link(&state.public_url, &code)).await,
        Err(e) => db_failed(bot, msg.chat.id, e).await,
    }
}

/// Typing a title without a command searches for it, in private chats only.
async fn handle_text(bot: Bot, msg: Message, state: Arc<BotState>) -> ResponseResult<()> {
    if let Some(text) = msg.text() {
        search(&bot, &msg, text, &state).await?;
    }
    Ok(())
}

async fn handle(bot: Bot, msg: Message, cmd: Command, state: Arc<BotState>) -> ResponseResult<()> {
    match cmd {
        Command::Start(payload) if payload.trim().starts_with(db::LOGIN_PREFIX) => {
            let token = payload.trim()[db::LOGIN_PREFIX.len()..].to_owned();
            ask_login(&bot, &msg, &token, &state).await?;
        }
        Command::Start(payload) => {
            bot.send_message(
                msg.chat.id,
                "Привет! Здесь можно найти фильм или сериал и смотреть его с того места, где остановились.",
            )
            .reply_markup(open_button(app_url(&state.public_url, &payload)))
            .await?;
        }
        Command::Help => {
            let admin = match msg.from.as_ref() {
                Some(u) => is_admin(&state, u.id.0 as i64).await.unwrap_or(false),
                None => false,
            };
            send_screen(&bot, msg.chat.id, screens::help(&state.public_url, admin)).await?;
        }
        Command::Search(query) => search(&bot, &msg, &query, &state).await?,
        Command::Continue => continue_watching(&bot, &msg, &state).await?,
        Command::My => my(&bot, &msg, &state).await?,
        Command::Login => site_login(&bot, &msg, &state).await?,
        Command::Invite => {
            let Some(user) = msg.from.as_ref() else { return Ok(()) };
            let telegram_id = user.id.0 as i64;
            let reply = match is_admin(&state, telegram_id).await {
                Ok(true) => match db::create_invite(&state.pool, None).await {
                    Ok(code) => format!(
                        "Одноразовое приглашение:\nhttps://t.me/{}?start={INVITE_PREFIX}{code}",
                        state.bot_username
                    ),
                    Err(e) => {
                        tracing::error!(error = %e, "creating invite failed");
                        "Не получилось создать приглашение, попробуйте позже.".to_owned()
                    }
                },
                Ok(false) => "Приглашения создаёт только администратор.".to_owned(),
                Err(e) => {
                    tracing::error!(error = %e, "admin check failed");
                    "Что-то пошло не так, попробуйте позже.".to_owned()
                }
            };
            bot.send_message(msg.chat.id, reply).await?;
        }
    }
    Ok(())
}

/// The command list Telegram shows in the chat menu. Telegram wants names without the slash.
fn menu(admin: bool) -> Vec<BotCommand> {
    let mut list = Command::bot_commands();
    if admin {
        list.push(BotCommand::new("invite", "создать приглашение (только админ)"));
    }
    for c in &mut list {
        c.command = c.command.trim_start_matches('/').to_owned();
    }
    list
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let token = std::env::var("BOT_TOKEN").context("BOT_TOKEN is not set")?;
    let public_url: Url = std::env::var("PUBLIC_URL")
        .context("PUBLIC_URL is not set")?
        .parse()
        .context("PUBLIC_URL is not a valid URL")?;
    let admin_ids = parse_ids(&std::env::var("ADMIN_TELEGRAM_IDS").unwrap_or_default())
        .context("ADMIN_TELEGRAM_IDS must be a comma-separated list of numbers")?;
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?)
        .await
        .context("connecting to PostgreSQL")?;

    // Only Telegram goes through the proxy; everything else stays direct.
    let mut http = teloxide::net::default_reqwest_settings();
    if let Some(proxy) = env_value("TELEGRAM_PROXY") {
        http = http.proxy(reqwest::Proxy::all(&proxy).context("TELEGRAM_PROXY is not a valid proxy address")?);
        tracing::info!("talking to Telegram through TELEGRAM_PROXY");
    }
    let mut bot = Bot::with_client(token, http.build().context("building the HTTP client")?);
    // For a self-hosted Bot API server, or a fake one in tests.
    if let Some(api) = env_value("TELEGRAM_API_URL") {
        bot = bot.set_api_url(api.parse().context("TELEGRAM_API_URL is not a valid URL")?);
    }
    let catalog = catalog::from_settings(
        env_value("TMDB_API_KEY").as_deref(),
        &env_value("TMDB_LANGUAGE").unwrap_or_else(|| "ru-RU".into()),
        env_value("TMDB_PROXY").as_deref(),
    )?;
    let me = bot.get_me().await.context("calling getMe, is BOT_TOKEN right?")?;
    let bot_username = me.username.clone().unwrap_or_default();
    bot.set_my_commands(menu(false)).await.context("setting bot commands")?;
    // Admins also see /invite in their own menu. A chat that never wrote to the bot
    // can't be addressed yet; they get it after the next restart.
    let admin_menu = menu(true);
    for id in &admin_ids {
        let scope = BotCommandScope::Chat {
            chat_id: Recipient::Id(ChatId(*id)),
        };
        if let Err(e) = bot.set_my_commands(admin_menu.clone()).scope(scope).await {
            tracing::debug!(admin = id, error = %e, "admin command menu not set");
        }
    }
    // The blue button next to the message field opens the app in every private chat.
    bot.set_chat_menu_button()
        .menu_button(MenuButton::WebApp {
            text: "Кино".into(),
            web_app: WebAppInfo {
                url: public_url.clone(),
            },
        })
        .await
        .context("setting the menu button")?;
    tracing::info!(%bot_username, "bot started");

    let state = Arc::new(BotState {
        pool,
        catalog,
        public_url,
        admin_ids,
        bot_username,
    });
    let handler = dptree::entry()
        .branch(Update::filter_message().filter_command::<Command>().endpoint(handle))
        .branch(
            Update::filter_message()
                .filter(|m: Message| m.chat.is_private() && m.text().is_some_and(|t| !t.starts_with('/')))
                .endpoint(handle_text),
        )
        .branch(Update::filter_callback_query().endpoint(handle_callback));
    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_payload_goes_into_url() {
        let base: Url = "https://kino.example.com/".parse().unwrap();
        assert_eq!(app_url(&base, "").as_str(), "https://kino.example.com/");
        assert_eq!(
            app_url(&base, "inv_AbC123").as_str(),
            "https://kino.example.com/?invite=AbC123"
        );
        // Anything that isn't a clean code is ignored.
        assert_eq!(app_url(&base, "inv_a&b=c").as_str(), "https://kino.example.com/");
        assert_eq!(app_url(&base, "hello").as_str(), "https://kino.example.com/");
    }

    #[test]
    fn menu_names_have_no_slash_and_invite_is_admin_only() {
        let names = |admin| menu(admin).into_iter().map(|c| c.command).collect::<Vec<_>>();
        assert_eq!(names(false), ["start", "search", "continue", "my", "login", "help"]);
        assert_eq!(names(true).last().map(String::as_str), Some("invite"));
    }
}
