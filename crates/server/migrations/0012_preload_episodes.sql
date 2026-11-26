-- Track per-episode download progress and partial-torrent selection for TV series preloads.

ALTER TABLE preload
    ADD COLUMN IF NOT EXISTS whole_torrent BOOLEAN NOT NULL DEFAULT TRUE;

CREATE TABLE IF NOT EXISTS preload_episode (
    preload_id   BIGINT NOT NULL REFERENCES preload (id) ON DELETE CASCADE,
    season       INT    NOT NULL,
    episode      INT    NOT NULL,
    file_index   INT,
    file_path    TEXT,
    size_bytes   BIGINT,
    progress     REAL   NOT NULL DEFAULT 0,
    state        TEXT   NOT NULL DEFAULT 'queued'
                        CHECK (state IN ('queued', 'downloading', 'ready', 'failed', 'deleted')),
    ready_at     TIMESTAMPTZ,
    delete_after TIMESTAMPTZ,
    PRIMARY KEY (preload_id, season, episode)
);

CREATE INDEX IF NOT EXISTS preload_episode_state_idx
    ON preload_episode (state, delete_after)
    WHERE delete_after IS NOT NULL;
