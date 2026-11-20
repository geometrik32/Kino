-- Users are Telegram accounts. Access is invite-only.
CREATE TABLE users (
    id           BIGSERIAL PRIMARY KEY,
    telegram_id  BIGINT      NOT NULL UNIQUE,
    username     TEXT,
    first_name   TEXT,
    last_name    TEXT,
    photo_url    TEXT,
    role         TEXT        NOT NULL DEFAULT 'user' CHECK (role IN ('admin', 'user')),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE invites (
    code       TEXT PRIMARY KEY,
    created_by BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    used_by    BIGINT REFERENCES users (id) ON DELETE SET NULL,
    used_at    TIMESTAMPTZ
);

-- One row per film, or per episode of a series (season/episode are 0 for films).
-- Stores the release and file the user picked so any device can resume exactly there.
CREATE TABLE watch_progress (
    user_id          BIGINT           NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_kind       TEXT             NOT NULL CHECK (media_kind IN ('movie', 'tv')),
    tmdb_id          BIGINT           NOT NULL,
    season           INT              NOT NULL DEFAULT 0,
    episode          INT              NOT NULL DEFAULT 0,
    position_sec     DOUBLE PRECISION NOT NULL CHECK (position_sec >= 0),
    duration_sec     DOUBLE PRECISION NOT NULL CHECK (duration_sec >= 0),
    finished         BOOLEAN          NOT NULL DEFAULT FALSE,
    release_infohash TEXT,
    release_file     INT,
    release_title    TEXT,
    updated_at       TIMESTAMPTZ      NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, media_kind, tmdb_id, season, episode)
);

CREATE INDEX watch_progress_recent ON watch_progress (user_id, updated_at DESC);

CREATE TABLE watchlist (
    user_id    BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_kind TEXT        NOT NULL CHECK (media_kind IN ('movie', 'tv')),
    tmdb_id    BIGINT      NOT NULL,
    added_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, media_kind, tmdb_id)
);
