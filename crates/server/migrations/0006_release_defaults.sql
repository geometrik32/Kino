-- Release filters a user wants applied by default.
ALTER TABLE user_prefs
    ADD COLUMN language     TEXT,
    ADD COLUMN hdr          BOOLEAN,
    ADD COLUMN dolby_vision BOOLEAN,
    ADD COLUMN subtitles    BOOLEAN,
    ADD COLUMN codec        TEXT,
    ADD COLUMN direct_play  BOOLEAN,
    ADD COLUMN min_seeders  INTEGER,
    ADD COLUMN max_size_gb  INTEGER,
    ADD COLUMN sort         TEXT;

CREATE INDEX torrent_source_updated ON torrent_source (updated_at);
