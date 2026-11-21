-- Cached Telegram profile photo so the browser can load it from this server
-- (t.me and api.telegram.org may need TELEGRAM_PROXY, and Bot API file URLs
-- carry the bot token).
ALTER TABLE users
    ADD COLUMN avatar_data       BYTEA,
    ADD COLUMN avatar_mime       TEXT,
    ADD COLUMN avatar_hash       TEXT,
    ADD COLUMN avatar_checked_at TIMESTAMPTZ;
