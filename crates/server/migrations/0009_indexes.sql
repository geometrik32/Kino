-- Performance indexes for background pruning, preload lookup, and login link cleanup.
CREATE INDEX IF NOT EXISTS login_link_created_at_idx ON login_link (created_at);
CREATE INDEX IF NOT EXISTS torrent_source_updated_at_idx ON torrent_source (updated_at);
CREATE INDEX IF NOT EXISTS torrent_source_infohash_idx ON torrent_source (infohash) WHERE infohash IS NOT NULL;
CREATE INDEX IF NOT EXISTS preload_infohash_idx ON preload (infohash);
CREATE INDEX IF NOT EXISTS preload_state_delete_idx ON preload (state, delete_after) WHERE delete_after IS NOT NULL;
