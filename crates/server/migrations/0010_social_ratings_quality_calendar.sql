-- Stage 2-4 schema: waiting statuses, per-season/episode ratings with cached effective rating,
-- scoped & multi-quality release choices, device quality overrides, and bot daily trends.

DO $$
DECLARE
    c text;
BEGIN
    SELECT conname INTO c
    FROM pg_constraint
    WHERE conrelid = 'title_state'::regclass
      AND contype = 'c'
      AND pg_get_constraintdef(oid) LIKE '%status%';
    IF c IS NOT NULL THEN
        EXECUTE format('ALTER TABLE title_state DROP CONSTRAINT %I', c);
    END IF;
END $$;

ALTER TABLE title_state
    ADD CONSTRAINT title_state_status_check
    CHECK (status IN ('watching', 'waiting_episode', 'watched', 'want', 'waiting_release', 'dropped'));

ALTER TABLE title_state
    ADD COLUMN IF NOT EXISTS effective_rating REAL;

UPDATE title_state SET status = 'watched' WHERE rating IS NOT NULL AND status IS NULL;
UPDATE title_state SET effective_rating = rating::real WHERE rating IS NOT NULL AND effective_rating IS NULL;

INSERT INTO watch_progress (user_id, media_kind, tmdb_id, season, episode, position_sec, duration_sec, finished)
SELECT user_id, media_kind, tmdb_id, 0, 0, 0, 0, TRUE
FROM title_state
WHERE media_kind = 'movie' AND rating IS NOT NULL
ON CONFLICT (user_id, media_kind, tmdb_id, season, episode) DO UPDATE SET finished = TRUE;

CREATE TABLE season_rating (
    user_id          BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    tmdb_id          BIGINT      NOT NULL,
    season           INT         NOT NULL,
    rating           SMALLINT    CHECK (rating BETWEEN 1 AND 5),
    effective_rating REAL,
    aired_episodes   INT         NOT NULL DEFAULT 0,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, tmdb_id, season)
);

CREATE TABLE episode_rating (
    user_id    BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    tmdb_id    BIGINT      NOT NULL,
    season     INT         NOT NULL,
    episode    INT         NOT NULL,
    rating     SMALLINT    NOT NULL CHECK (rating BETWEEN 1 AND 5),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, tmdb_id, season, episode)
);

CREATE INDEX episode_rating_title ON episode_rating (user_id, tmdb_id, season, episode);

ALTER TABLE release_choice
    ADD COLUMN IF NOT EXISTS episode     INT  NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS quality_key TEXT NOT NULL DEFAULT '';

UPDATE release_choice
SET quality_key = CASE
    WHEN quality IN ('2160p', '4K', '4k', 'UHD') THEN '2160p'
    WHEN quality IN ('1080p', '1440p') THEN '1080p'
    WHEN quality = '720p' THEN '720p'
    WHEN quality IS NOT NULL AND quality <> '' THEN 'SD'
    ELSE ''
END;

ALTER TABLE release_choice DROP CONSTRAINT release_choice_pkey;
ALTER TABLE release_choice ADD PRIMARY KEY (user_id, media_kind, tmdb_id, season, episode, quality_key);

CREATE INDEX release_choice_shared
    ON release_choice (media_kind, tmdb_id, season, episode, quality_key, chosen_at DESC);

CREATE TABLE release_device_quality (
    user_id     BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_kind  TEXT        NOT NULL CHECK (media_kind IN ('movie', 'tv')),
    tmdb_id     BIGINT      NOT NULL,
    season      INT         NOT NULL DEFAULT 0,
    episode     INT         NOT NULL DEFAULT 0,
    device_kind TEXT        NOT NULL CHECK (device_kind IN ('desktop', 'mobile')),
    quality_key TEXT        NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, media_kind, tmdb_id, season, episode, device_kind)
);

ALTER TABLE user_prefs
    ADD COLUMN IF NOT EXISTS desktop_quality TEXT,
    ADD COLUMN IF NOT EXISTS mobile_quality  TEXT;

CREATE TABLE bot_trend_prefs (
    user_id        BIGINT      PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    enabled        BOOLEAN     NOT NULL DEFAULT FALSE,
    include_tv     BOOLEAN     NOT NULL DEFAULT FALSE,
    send_hour      SMALLINT    NOT NULL DEFAULT 19 CHECK (send_hour BETWEEN 0 AND 23),
    item_limit     SMALLINT    NOT NULL DEFAULT 5 CHECK (item_limit BETWEEN 3 AND 8),
    last_sent_date DATE,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS title_state_by_title ON title_state (media_kind, tmdb_id);
CREATE INDEX IF NOT EXISTS watch_progress_by_title ON watch_progress (media_kind, tmdb_id, updated_at DESC);
