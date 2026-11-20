-- Login of the phone and desktop apps through the Telegram bot: the app creates a
-- one-time code, the user presses Start in the bot with it, the app picks up the session.
CREATE TABLE login_link (
    code        TEXT PRIMARY KEY,
    invite      TEXT,
    user_id     BIGINT REFERENCES users (id) ON DELETE CASCADE,
    not_invited BOOLEAN     NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
