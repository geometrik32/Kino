-- The release a user picked for a film or for one season of a series.
-- "Смотреть" uses it by default instead of asking again; the user can change it.
CREATE TABLE release_choice (
    user_id       BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_kind    TEXT        NOT NULL CHECK (media_kind IN ('movie', 'tv')),
    tmdb_id       BIGINT      NOT NULL,
    -- 0 for films.
    season        INT         NOT NULL DEFAULT 0,
    infohash      TEXT        NOT NULL,
    release_title TEXT,
    -- Remembered so a new season can be pre-filtered to the same quality and voice-over.
    quality       TEXT,
    voice         TEXT,
    chosen_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, media_kind, tmdb_id, season)
);
