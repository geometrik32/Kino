-- Where a release found by the indexer can be fetched from. Keyed by info-hash,
-- or by a placeholder key (`u…`) until the `.torrent` is downloaded.
CREATE TABLE torrent_source (
    key        TEXT PRIMARY KEY,
    title      TEXT NOT NULL,
    link       TEXT,
    magnet     TEXT,
    infohash   TEXT,
    torrent    BYTEA,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
