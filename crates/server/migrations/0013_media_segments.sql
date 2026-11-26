-- Shared intro / recap / credits timestamps and preloaded episode analysis tracking.

CREATE TABLE IF NOT EXISTS media_segment (
    media_kind       TEXT NOT NULL CHECK (media_kind IN ('movie', 'tv')),
    tmdb_id          BIGINT NOT NULL,
    season           INT NOT NULL DEFAULT 0,
    episode          INT NOT NULL DEFAULT 0,
    -- Empty string for release-agnostic open sources (IntroDB / AniSkip); lowercase 40-char infohash for local rip analysis/chapters.
    release_infohash TEXT NOT NULL DEFAULT '',
    segment_kind     TEXT NOT NULL CHECK (segment_kind IN ('intro', 'recap', 'outro', 'post_credits')),
    start_sec        DOUBLE PRECISION NOT NULL,
    end_sec          DOUBLE PRECISION NOT NULL,
    source           TEXT NOT NULL CHECK (source IN ('local_analysis', 'chapters', 'intro_db', 'ani_skip')),
    confidence       REAL NOT NULL DEFAULT 1.0,
    episode_duration DOUBLE PRECISION,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (media_kind, tmdb_id, season, episode, release_infohash, segment_kind)
);

CREATE INDEX IF NOT EXISTS media_segment_lookup_idx
    ON media_segment (media_kind, tmdb_id, season, episode);

CREATE TABLE IF NOT EXISTS media_segment_lookup (
    media_kind  TEXT NOT NULL CHECK (media_kind IN ('movie', 'tv')),
    tmdb_id     BIGINT NOT NULL,
    season      INT NOT NULL DEFAULT 0,
    episode     INT NOT NULL DEFAULT 0,
    status      TEXT NOT NULL CHECK (status IN ('found', 'empty')),
    checked_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (media_kind, tmdb_id, season, episode)
);

CREATE TABLE IF NOT EXISTS media_analysis_done (
    release_infohash TEXT NOT NULL,
    season           INT NOT NULL,
    episode          INT NOT NULL,
    file_path        TEXT NOT NULL,
    duration_sec     DOUBLE PRECISION,
    analyzed_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (release_infohash, season, episode)
);
