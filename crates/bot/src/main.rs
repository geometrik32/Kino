//! Telegram bot. For now: `/start` opens the app (optionally carrying an invite),
//! `/invite` lets an admin create an invite link.

use std::sync::Arc;

use anyhow::Context;
use kino_server::{config::parse_ids, db};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, WebAppInfo},
    utils::command::BotCommands,
};
use tracing_subscriber::EnvFilter;
use url::Url;

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
        #[command(description = "показать помощь")]
        Help,
        #[command(description = "создать приглашение (только админ)")]
        Invite,
    }
}

use commands::Command;

struct BotState {
    pool: PgPool,
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

async fn handle(bot: Bot, msg: Message, cmd: Command, state: Arc<BotState>) -> ResponseResult<()> {
    match cmd {
        Command::Start(payload) => {
            bot.send_message(
                msg.chat.id,
                "Привет! Здесь можно найти фильм или сериал и смотреть его с того места, где остановились.",
            )
            .reply_markup(open_button(app_url(&state.public_url, &payload)))
            .await?;
        }
        Command::Help => {
            bot.send_message(msg.chat.id, Command::descriptions().to_string())
                .await?;
        }
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

    let bot = Bot::new(token);
    let me = bot.get_me().await.context("calling getMe, is BOT_TOKEN right?")?;
    let bot_username = me.username.clone().unwrap_or_default();
    bot.set_my_commands(Command::bot_commands())
        .await
        .context("setting bot commands")?;
    tracing::info!(%bot_username, "bot started");

    let state = Arc::new(BotState {
        pool,
        public_url,
        admin_ids,
        bot_username,
    });
    let handler = Update::filter_message().filter_command::<Command>().endpoint(handle);
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
}
