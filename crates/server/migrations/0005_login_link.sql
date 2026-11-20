-- Logging in on the website through the Telegram app: the site creates a link,
-- the bot fills in who confirmed it, the site picks the session up.
CREATE TABLE login_link (
    token        TEXT PRIMARY KEY,
    invite       TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    telegram_id  BIGINT,
    first_name   TEXT,
    last_name    TEXT,
    username     TEXT,
    confirmed_at TIMESTAMPTZ
);
