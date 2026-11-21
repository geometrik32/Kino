//! Telegram bot: `/start` opens the app (optionally carrying an invite), `/search`
//! (or just a title typed in the chat), `/continue`, `/my`, `/login` for the
//! website, `/help`, and `/invite` for admins.

use std::sync::Arc;

use anyhow::Context;
use kino_core::{ImageSize, MediaKind, TitleStatus};
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
        BotCommand, BotCommandScope, CallbackQuery, ChatAction, InlineKeyboardButton, InlineKeyboardMarkup, InputFile,
        MenuButton, ParseMode, Recipient, WebAppInfo,
    },
    utils::command::BotCommands,
};
use tracing_subscriber::EnvFilter;
use url::Url;

mod screens;
use screens::{BotAction, Screen};

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
        #[command(description = "случайный фильм по параметрам")]
        Random,
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

async fn is_allowed(state: &BotState, telegram_id: i64) -> anyhow::Result<bool> {
    if state.admin_ids.contains(&telegram_id) {
        return Ok(true);
    }
    Ok(db::user_by_telegram(&state.pool, telegram_id).await?.is_some())
}

async fn require_allowed(
    bot: &Bot,
    chat: ChatId,
    from: Option<&teloxide::types::User>,
    state: &BotState,
) -> Result<bool, teloxide::RequestError> {
    let Some(u) = from else {
        return Ok(false);
    };
    match is_allowed(state, u.id.0 as i64).await {
        Ok(true) => Ok(true),
        Ok(false) => {
            say(
                bot,
                chat,
                "Доступ только по приглашению. Попросите ссылку у администратора.",
            )
            .await?;
            Ok(false)
        }
        Err(e) => {
            tracing::error!(error = %e, "access check failed");
            say(bot, chat, "Что-то пошло не так, попробуйте позже.").await?;
            Ok(false)
        }
    }
}

/// Callback data of the "confirm website login" button.
const LOGIN_CALLBACK: &str = "login:";

fn valid_login_token(token: &str) -> bool {
    (1..=58).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `/start login_<token>`: ask before logging the website in, so a link someone
/// else made can't silently log them in as you.
async fn ask_login(bot: &Bot, msg: &Message, token: &str, state: &BotState) -> ResponseResult<()> {
    if !msg.chat.is_private() {
        return say(
            bot,
            msg.chat.id,
            "Подтвердить вход на сайт можно только в личных сообщениях с ботом.",
        )
        .await;
    }
    if !valid_login_token(token) {
        return say(
            bot,
            msg.chat.id,
            "Эта ссылка для входа повреждена или устарела. Нажмите «Войти через Telegram» на сайте ещё раз.",
        )
        .await;
    }
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
    let (chat, is_private) = match q.regular_message() {
        Some(m) => (m.chat.id, m.chat.is_private()),
        None => (ChatId(q.from.id.0 as i64), true),
    };
    if let Some(which) = data.strip_prefix(screens::MY_CALLBACK) {
        return my_callback(&bot, q, which, is_private, &state).await;
    }
    if let Some(suffix) = data.strip_prefix(screens::CMD_CALLBACK) {
        let _ = bot.answer_callback_query(q.id.clone()).await;
        if let Some(action) = BotAction::parse_callback(suffix) {
            return run_action(&bot, chat, is_private, Some(&q.from), action, &state).await;
        }
        return Ok(());
    }
    if let Some(suffix) = data.strip_prefix(screens::CARD_CALLBACK) {
        let _ = bot.answer_callback_query(q.id.clone()).await;
        if !require_allowed(&bot, chat, Some(&q.from), &state).await? {
            return Ok(());
        }
        let Some((kind, tmdb_id)) = screens::parse_card_callback(suffix) else {
            return Ok(());
        };
        return send_title_card(&bot, chat, is_private, kind, tmdb_id, &state).await;
    }
    if let Some(suffix) = data.strip_prefix(screens::RANDOM_CALLBACK) {
        let _ = bot.answer_callback_query(q.id.clone()).await;
        if !require_allowed(&bot, chat, Some(&q.from), &state).await? {
            return Ok(());
        }
        let Some(action) = screens::parse_random_callback(suffix) else {
            return Ok(());
        };
        return handle_random_callback(&bot, &q, is_private, action, &state).await;
    }
    let Some(token) = data.strip_prefix(LOGIN_CALLBACK) else {
        let _ = bot.answer_callback_query(q.id).await;
        return Ok(());
    };
    if !is_private || !valid_login_token(token) {
        let _ = bot.answer_callback_query(q.id).await;
        return Ok(());
    }
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
    let _ = bot.answer_callback_query(q.id.clone()).await;
    if let Some(message) = q.regular_message() {
        let _ = bot.edit_message_text(message.chat.id, message.id, text).await;
    } else {
        bot.send_message(q.from.id, text).await?;
    }
    Ok(())
}

/// Sends a screen as a new message, replacing `WebApp` buttons with URL buttons in group chats.
async fn send_screen(bot: &Bot, chat: ChatId, is_private: bool, screen: Screen) -> ResponseResult<()> {
    let screen = screen.for_chat(is_private);
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
const POSTER_WAIT: std::time::Duration = std::time::Duration::from_secs(4);

async fn fetch_poster(catalog: &dyn Catalog, poster_path: Option<&str>) -> Option<(Vec<u8>, String)> {
    let file = poster_path?.trim_start_matches('/');
    if file.is_empty() {
        return None;
    }
    tokio::time::timeout(POSTER_WAIT, catalog.image(ImageSize::Poster.tmdb(), file))
        .await
        .ok()?
        .ok()
}

async fn send_card(
    bot: &Bot,
    chat: ChatId,
    is_private: bool,
    screen: Screen,
    poster_path: Option<&str>,
    poster: Option<Vec<u8>>,
) -> ResponseResult<()> {
    let screen = screen.for_chat(is_private);
    if let Some(bytes) = poster.filter(|b| !b.is_empty()) {
        let filename = poster_path
            .and_then(|p| p.rsplit('/').next())
            .filter(|s| !s.is_empty())
            .unwrap_or("poster.jpg")
            .to_owned();
        let file = InputFile::memory(bytes).file_name(filename);
        let mut req = bot
            .send_photo(chat, file)
            .caption(screen.text.clone())
            .parse_mode(ParseMode::Html);
        if !screen.keyboard.inline_keyboard.is_empty() {
            req = req.reply_markup(screen.keyboard.clone());
        }
        match req.await {
            Ok(_) => return Ok(()),
            Err(e) => tracing::warn!(error = %e, "sending poster card failed, falling back to text"),
        }
    }
    send_screen(bot, chat, is_private, screen).await
}

async fn send_title_card(
    bot: &Bot,
    chat: ChatId,
    is_private: bool,
    kind: MediaKind,
    tmdb_id: i64,
    state: &BotState,
) -> ResponseResult<()> {
    let _ = bot.send_chat_action(chat, ChatAction::Typing).await;
    match tokio::time::timeout(CATALOG_WAIT, state.catalog.details(kind, tmdb_id)).await {
        Ok(Ok(details)) => {
            let poster = fetch_poster(state.catalog.as_ref(), details.summary.poster_path.as_deref())
                .await
                .map(|(b, _)| b);
            let card = screens::title_details_card(&state.public_url, &details);
            send_card(
                bot,
                chat,
                is_private,
                card,
                details.summary.poster_path.as_deref(),
                poster,
            )
            .await
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "loading title details failed");
            say(
                bot,
                chat,
                "Не удалось загрузить карточку фильма, попробуйте чуть позже.",
            )
            .await
        }
        Err(_) => say(bot, chat, "Каталог отвечает слишком долго, попробуйте чуть позже.").await,
    }
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

async fn send_search_prompt(
    bot: &Bot,
    chat: ChatId,
    is_private: bool,
    from: Option<&teloxide::types::User>,
    state: &BotState,
) -> ResponseResult<()> {
    let mut req = bot
        .send_message(chat, screens::search_prompt())
        .parse_mode(ParseMode::Html);
    if is_private {
        let admin = match from {
            Some(u) => is_admin(state, u.id.0 as i64).await.unwrap_or(false),
            None => false,
        };
        req = req.reply_markup(screens::main_reply_keyboard(admin));
    }
    req.await?;
    Ok(())
}

async fn search(bot: &Bot, msg: &Message, query: &str, state: &BotState) -> ResponseResult<()> {
    let is_private = msg.chat.is_private();
    if !require_allowed(bot, msg.chat.id, msg.from.as_ref(), state).await? {
        return Ok(());
    }
    let query = query.trim();
    if query.is_empty() {
        return send_search_prompt(bot, msg.chat.id, is_private, msg.from.as_ref(), state).await;
    }
    if query.chars().count() > 200 {
        return say(
            bot,
            msg.chat.id,
            "Слишком длинный запрос — сократите его до 200 символов.",
        )
        .await;
    }
    let _ = bot.send_chat_action(msg.chat.id, ChatAction::Typing).await;
    let found = tokio::time::timeout(CATALOG_WAIT, state.catalog.search(query)).await;
    match found {
        Ok(Ok(results)) => {
            if results.items.is_empty() {
                return send_screen(bot, msg.chat.id, is_private, screens::search_empty(query)).await;
            }
            let shown = &results.items[..results.items.len().min(screens::LIST_LIMIT)];
            let cards_len = shown.len().min(screens::SEARCH_CARD_LIMIT);
            let (cards, rest) = shown.split_at(cards_len);

            let mut posters: Vec<Option<Vec<u8>>> = vec![None; cards.len()];
            let mut set = tokio::task::JoinSet::new();
            for (idx, item) in cards.iter().enumerate() {
                if let Some(path) = item.poster_path.clone() {
                    let catalog = state.catalog.clone();
                    set.spawn(async move {
                        let bytes = fetch_poster(catalog.as_ref(), Some(&path)).await.map(|(b, _)| b);
                        (idx, bytes)
                    });
                }
            }
            while let Some(Ok((idx, bytes))) = set.join_next().await {
                posters[idx] = bytes;
            }

            for (item, poster) in cards.iter().zip(posters) {
                let card = screens::title_card(&state.public_url, item);
                send_card(bot, msg.chat.id, is_private, card, item.poster_path.as_deref(), poster).await?;
            }
            if !rest.is_empty() {
                send_screen(
                    bot,
                    msg.chat.id,
                    is_private,
                    screens::more_search_results(query, cards.len(), rest),
                )
                .await?;
            }
            Ok(())
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "search failed");
            say(bot, msg.chat.id, "Поиск сейчас не отвечает, попробуйте чуть позже.").await
        }
        Err(_) => say(bot, msg.chat.id, "Поиск отвечает слишком долго, попробуйте чуть позже.").await,
    }
}

async fn title_names_parallel(state: &BotState, keys: &[(MediaKind, i64)]) -> Vec<Option<String>> {
    let mut results = vec![None; keys.len()];
    let mut set = tokio::task::JoinSet::new();
    for (idx, &(kind, tmdb_id)) in keys.iter().enumerate() {
        let catalog = state.catalog.clone();
        set.spawn(async move {
            let name = tokio::time::timeout(CATALOG_WAIT, catalog.details(kind, tmdb_id))
                .await
                .ok()
                .and_then(Result::ok)
                .map(|d| d.summary.title);
            (idx, name)
        });
    }
    while let Some(Ok((idx, name))) = set.join_next().await {
        results[idx] = name;
    }
    results
}

async fn continue_watching(
    bot: &Bot,
    chat: ChatId,
    is_private: bool,
    from: Option<&teloxide::types::User>,
    state: &BotState,
) -> ResponseResult<()> {
    let Some(from) = from else { return Ok(()) };
    if !is_private {
        return say(
            bot,
            chat,
            "Список просмотра доступен только в личных сообщениях с ботом.",
        )
        .await;
    }
    let user = match db::user_by_telegram(&state.pool, from.id.0 as i64).await {
        Ok(Some(user)) => user,
        Ok(None) => return send_screen(bot, chat, is_private, screens::not_registered(&state.public_url)).await,
        Err(e) => return db_failed(bot, chat, e).await,
    };
    let items = match db::continue_watching(&state.pool, user.id, screens::LIST_LIMIT as i64).await {
        Ok(items) => items,
        Err(e) => return db_failed(bot, chat, e).await,
    };
    let _ = bot.send_chat_action(chat, ChatAction::Typing).await;
    let keys: Vec<_> = items.iter().map(|it| (it.last.kind, it.last.tmdb_id)).collect();
    let names = title_names_parallel(state, &keys).await;
    let rows: Vec<_> = items
        .into_iter()
        .zip(names)
        .map(|(item, title)| screens::ContinueRow { item, title })
        .collect();
    send_screen(bot, chat, is_private, screens::continue_list(&state.public_url, &rows)).await
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
    let slice = &all[..all.len().min(screens::LIST_LIMIT)];
    let keys: Vec<_> = slice.iter().map(|t| (t.kind, t.tmdb_id)).collect();
    let names = title_names_parallel(state, &keys).await;
    let titles: Vec<_> = slice
        .iter()
        .zip(names)
        .map(|(t, name)| (t.kind, t.tmdb_id, name.unwrap_or_else(|| "Без названия".to_owned())))
        .collect();
    Ok(screens::my_list(&state.public_url, status, &titles, all.len()))
}

async fn my(
    bot: &Bot,
    chat: ChatId,
    is_private: bool,
    from: Option<&teloxide::types::User>,
    state: &BotState,
) -> ResponseResult<()> {
    let Some(from) = from else { return Ok(()) };
    if !is_private {
        return say(bot, chat, "Личные списки доступны только в личных сообщениях с ботом.").await;
    }
    match db::user_by_telegram(&state.pool, from.id.0 as i64).await {
        Ok(Some(user)) => match my_menu(state, user.id).await {
            Ok(screen) => send_screen(bot, chat, is_private, screen).await,
            Err(e) => db_failed(bot, chat, e).await,
        },
        Ok(None) => send_screen(bot, chat, is_private, screens::not_registered(&state.public_url)).await,
        Err(e) => db_failed(bot, chat, e).await,
    }
}

/// A tap in the "Мои" screen turns the same message into a list, or back to the menu.
async fn my_callback(
    bot: &Bot,
    q: CallbackQuery,
    which: &str,
    is_private: bool,
    state: &BotState,
) -> ResponseResult<()> {
    let _ = bot.answer_callback_query(q.id.clone()).await;
    if !is_private {
        return Ok(());
    }
    let Some(message) = q.regular_message() else {
        return Ok(());
    };
    let _ = bot.send_chat_action(message.chat.id, ChatAction::Typing).await;
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
            let screen = screen.for_chat(is_private);
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
async fn site_login(
    bot: &Bot,
    chat: ChatId,
    is_private: bool,
    from: Option<&teloxide::types::User>,
    state: &BotState,
) -> ResponseResult<()> {
    let Some(from) = from else { return Ok(()) };
    if !is_private {
        return say(bot, chat, "Ссылку для входа я пришлю только в личных сообщениях.").await;
    }
    let tg = tg_user(from);
    // A newcomer would be turned away by the site anyway; say why here.
    let known = match db::user_by_telegram(&state.pool, tg.id).await {
        Ok(user) => user.is_some() || state.admin_ids.contains(&tg.id),
        Err(e) => return db_failed(bot, chat, e).await,
    };
    if !known {
        return say(
            bot,
            chat,
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
        Ok(code) => send_screen(bot, chat, is_private, screens::login_link(&state.public_url, &code)).await,
        Err(e) => db_failed(bot, chat, e).await,
    }
}

async fn send_help(
    bot: &Bot,
    chat: ChatId,
    is_private: bool,
    from: Option<&teloxide::types::User>,
    state: &BotState,
) -> ResponseResult<()> {
    let admin = match from {
        Some(u) => is_admin(state, u.id.0 as i64).await.unwrap_or(false),
        None => false,
    };
    send_screen(bot, chat, is_private, screens::help(&state.public_url, admin)).await
}

async fn create_invite(
    bot: &Bot,
    chat: ChatId,
    is_private: bool,
    from: Option<&teloxide::types::User>,
    state: &BotState,
) -> ResponseResult<()> {
    let Some(user) = from else { return Ok(()) };
    if !is_private {
        return say(
            bot,
            chat,
            "Приглашения можно создавать только в личных сообщениях с ботом.",
        )
        .await;
    }
    let telegram_id = user.id.0 as i64;
    let reply = match is_admin(state, telegram_id).await {
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
    bot.send_message(chat, reply).await?;
    Ok(())
}

async fn send_random_movie(
    bot: &Bot,
    chat: ChatId,
    is_private: bool,
    filter_state: Option<screens::RandomState>,
    state: &BotState,
) -> ResponseResult<()> {
    let _ = bot.send_chat_action(chat, ChatAction::Typing).await;
    let filter = filter_state.map(screens::RandomState::to_filter).unwrap_or_default();
    match tokio::time::timeout(CATALOG_WAIT, state.catalog.random_movie(&filter)).await {
        Ok(Ok(pick)) => {
            let poster = fetch_poster(state.catalog.as_ref(), pick.details.summary.poster_path.as_deref())
                .await
                .map(|(b, _)| b);
            let card = screens::random_movie_card(&state.public_url, &pick, filter_state);
            send_card(
                bot,
                chat,
                is_private,
                card,
                pick.details.summary.poster_path.as_deref(),
                poster,
            )
            .await
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "random movie failed");
            say(bot, chat, "Не удалось подобрать фильм, попробуйте ещё раз чуть позже.").await
        }
        Err(_) => say(bot, chat, "Каталог отвечает слишком долго, попробуйте чуть позже.").await,
    }
}

async fn show_or_edit_screen(bot: &Bot, q: &CallbackQuery, is_private: bool, screen: Screen) -> ResponseResult<()> {
    let screen = screen.for_chat(is_private);
    if let Some(message) = q.regular_message() {
        if message.photo().is_none_or(<[_]>::is_empty) {
            let mut req = bot
                .edit_message_text(message.chat.id, message.id, screen.text)
                .parse_mode(ParseMode::Html);
            if !screen.keyboard.inline_keyboard.is_empty() {
                req = req.reply_markup(screen.keyboard);
            }
            if let Err(e) = req.await {
                tracing::debug!(error = %e, "editing random filter message");
            }
            return Ok(());
        }
        return send_screen(bot, message.chat.id, is_private, screen).await;
    }
    send_screen(bot, ChatId(q.from.id.0 as i64), is_private, screen).await
}

async fn handle_random_callback(
    bot: &Bot,
    q: &CallbackQuery,
    is_private: bool,
    action: screens::RandomCallback,
    state: &BotState,
) -> ResponseResult<()> {
    let chat = q
        .regular_message()
        .map(|m| m.chat.id)
        .unwrap_or_else(|| ChatId(q.from.id.0 as i64));
    match action {
        screens::RandomCallback::Any => send_random_movie(bot, chat, is_private, None, state).await,
        screens::RandomCallback::Go(st) => send_random_movie(bot, chat, is_private, Some(st), state).await,
        screens::RandomCallback::Menu(st) => show_or_edit_screen(bot, q, is_private, screens::random_menu(st)).await,
        screens::RandomCallback::Submenu(sub, st) => {
            show_or_edit_screen(bot, q, is_private, screens::random_submenu(sub, st)).await
        }
    }
}

async fn run_action(
    bot: &Bot,
    chat: ChatId,
    is_private: bool,
    from: Option<&teloxide::types::User>,
    action: BotAction,
    state: &BotState,
) -> ResponseResult<()> {
    match action {
        BotAction::Random => {
            if !require_allowed(bot, chat, from, state).await? {
                return Ok(());
            }
            send_screen(
                bot,
                chat,
                is_private,
                screens::random_menu(screens::RandomState::default()),
            )
            .await
        }
        BotAction::Search => send_search_prompt(bot, chat, is_private, from, state).await,
        BotAction::Continue => continue_watching(bot, chat, is_private, from, state).await,
        BotAction::My => my(bot, chat, is_private, from, state).await,
        BotAction::Login => site_login(bot, chat, is_private, from, state).await,
        BotAction::Help => send_help(bot, chat, is_private, from, state).await,
        BotAction::Invite => create_invite(bot, chat, is_private, from, state).await,
    }
}

/// Typing a title without a command searches for it, in private chats only.
async fn handle_text(bot: Bot, msg: Message, state: Arc<BotState>) -> ResponseResult<()> {
    if let Some(text) = msg.text() {
        if let Some(action) = BotAction::parse_button(text) {
            return run_action(
                &bot,
                msg.chat.id,
                msg.chat.is_private(),
                msg.from.as_ref(),
                action,
                &state,
            )
            .await;
        }
        search(&bot, &msg, text, &state).await?;
    }
    Ok(())
}

async fn handle(bot: Bot, msg: Message, cmd: Command, state: Arc<BotState>) -> ResponseResult<()> {
    let from = msg.from.as_ref();
    let is_private = msg.chat.is_private();
    match cmd {
        Command::Start(payload) if payload.trim().starts_with(db::LOGIN_PREFIX) => {
            let token = payload.trim()[db::LOGIN_PREFIX.len()..].to_owned();
            ask_login(&bot, &msg, &token, &state).await?;
        }
        Command::Start(payload) => {
            let admin = match from {
                Some(u) => is_admin(&state, u.id.0 as i64).await.unwrap_or(false),
                None => false,
            };
            let url = app_url(&state.public_url, &payload);
            let has_invite = url != state.public_url;
            if is_private {
                if has_invite {
                    send_screen(&bot, msg.chat.id, is_private, screens::invite_welcome(&url)).await?;
                    bot.send_message(msg.chat.id, "Кнопки команд закреплены ниже 👇")
                        .disable_notification(true)
                        .reply_markup(screens::main_reply_keyboard(admin))
                        .await?;
                } else {
                    bot.send_message(msg.chat.id, screens::start_welcome_text())
                        .parse_mode(ParseMode::Html)
                        .reply_markup(screens::main_reply_keyboard(admin))
                        .await?;
                }
            } else {
                // Never echo a one-time invite link back into a group chat.
                let group_url = if has_invite { &state.public_url } else { &url };
                send_screen(&bot, msg.chat.id, is_private, screens::start_inline(group_url, admin)).await?;
            }
        }
        Command::Help => run_action(&bot, msg.chat.id, is_private, from, BotAction::Help, &state).await?,
        Command::Search(query) => search(&bot, &msg, &query, &state).await?,
        Command::Random => run_action(&bot, msg.chat.id, is_private, from, BotAction::Random, &state).await?,
        Command::Continue => run_action(&bot, msg.chat.id, is_private, from, BotAction::Continue, &state).await?,
        Command::My => run_action(&bot, msg.chat.id, is_private, from, BotAction::My, &state).await?,
        Command::Login => run_action(&bot, msg.chat.id, is_private, from, BotAction::Login, &state).await?,
        Command::Invite => run_action(&bot, msg.chat.id, is_private, from, BotAction::Invite, &state).await?,
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
    // The text people see before they press "Начать", and on the bot's profile.
    // Cosmetic, so a failure only gets a warning.
    if let Err(e) = bot.set_my_description().description(screens::BOT_DESCRIPTION).await {
        tracing::warn!(error = %e, "setting the bot description failed");
    }
    if let Err(e) = bot
        .set_my_short_description()
        .short_description(screens::BOT_SHORT_DESCRIPTION)
        .await
    {
        tracing::warn!(error = %e, "setting the short description failed");
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
        assert_eq!(
            names(false),
            ["start", "search", "random", "continue", "my", "login", "help"]
        );
        assert_eq!(names(true).last().map(String::as_str), Some("invite"));
    }
}
