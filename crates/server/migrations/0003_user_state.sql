-- Per-user state of a title: status, rating, player tracks, new-episode notifications.
-- Replaces the `watchlist` table ("Хочу посмотреть" is status 'want').
CREATE TABLE title_state (
    user_id          BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_kind       TEXT        NOT NULL CHECK (media_kind IN ('movie', 'tv')),
    tmdb_id          BIGINT      NOT NULL,
    status           TEXT        CHECK (status IN ('watching', 'watched', 'want', 'dropped')),
    rating           SMALLINT    CHECK (rating BETWEEN 1 AND 5),
    audio_track      TEXT,
    subtitle_track   TEXT,
    -- Latest episode the user was told about (series); NULL until the first check.
    notified_season  INT,
    notified_episode INT,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, media_kind, tmdb_id)
);

CREATE INDEX title_state_by_status ON title_state (user_id, status, updated_at DESC);

INSERT INTO title_state (user_id, media_kind, tmdb_id, status, updated_at)
SELECT user_id, media_kind, tmdb_id, 'want', added_at FROM watchlist;

DROP TABLE watchlist;

-- Viewing sessions: one row per sitting (progress updates within 30 minutes extend it).
CREATE TABLE watch_history (
    id            BIGSERIAL PRIMARY KEY,
    user_id       BIGINT           NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_kind    TEXT             NOT NULL CHECK (media_kind IN ('movie', 'tv')),
    tmdb_id       BIGINT           NOT NULL,
    season        INT              NOT NULL DEFAULT 0,
    episode       INT              NOT NULL DEFAULT 0,
    started_at    TIMESTAMPTZ      NOT NULL DEFAULT now(),
    last_at       TIMESTAMPTZ      NOT NULL DEFAULT now(),
    position_from DOUBLE PRECISION NOT NULL,
    position_to   DOUBLE PRECISION NOT NULL,
    device        TEXT
);

CREATE INDEX watch_history_recent ON watch_history (user_id, last_at DESC);

CREATE TABLE user_prefs (
    user_id       BIGINT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    quality       TEXT,
    voice         TEXT,
    subtitle_lang TEXT
);

-- "Предварительная загрузка": a release downloaded to the server in advance.
CREATE TABLE preload (
    id           BIGSERIAL PRIMARY KEY,
    user_id      BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_kind   TEXT        NOT NULL CHECK (media_kind IN ('movie', 'tv')),
    tmdb_id      BIGINT      NOT NULL,
    season       INT         NOT NULL DEFAULT 0,
    infohash     TEXT        NOT NULL,
    title        TEXT,
    state        TEXT        NOT NULL DEFAULT 'queued'
                             CHECK (state IN ('queued', 'downloading', 'ready', 'failed', 'deleted')),
    progress     REAL        NOT NULL DEFAULT 0,
    size_bytes   BIGINT,
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ready_at     TIMESTAMPTZ,
    delete_after TIMESTAMPTZ,
    UNIQUE (user_id, infohash)
);
