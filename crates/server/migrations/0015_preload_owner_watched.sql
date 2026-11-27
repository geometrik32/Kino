-- Track whether the preload owner has personally watched the downloaded title/episode
-- so other users' views never shorten the owner's initial 7-day retention window,
-- while still extending the post-watch 2-day window when another user watches.

ALTER TABLE preload
    ADD COLUMN IF NOT EXISTS owner_watched_at TIMESTAMPTZ;

ALTER TABLE preload_episode
    ADD COLUMN IF NOT EXISTS owner_watched_at TIMESTAMPTZ;

-- Backfill movies already watched by the preload owner.
UPDATE preload p
SET owner_watched_at = COALESCE(p.ready_at, now())
WHERE p.media_kind = 'movie'
  AND p.state = 'ready'
  AND p.owner_watched_at IS NULL
  AND (
      EXISTS (
          SELECT 1 FROM watch_progress wp
          WHERE wp.user_id = p.user_id AND wp.media_kind = 'movie'
            AND wp.tmdb_id = p.tmdb_id AND wp.finished = TRUE
      )
      OR EXISTS (
          SELECT 1 FROM title_state ts
          WHERE ts.user_id = p.user_id AND ts.media_kind = 'movie'
            AND ts.tmdb_id = p.tmdb_id AND ts.status = 'watched'
      )
  );

-- Backfill whole-torrent TV preloads already watched by the preload owner.
UPDATE preload p
SET owner_watched_at = COALESCE(p.ready_at, now())
WHERE p.media_kind = 'tv'
  AND p.whole_torrent = TRUE
  AND p.state = 'ready'
  AND p.owner_watched_at IS NULL
  AND EXISTS (
      SELECT 1 FROM title_state ts
      WHERE ts.user_id = p.user_id AND ts.media_kind = 'tv'
        AND ts.tmdb_id = p.tmdb_id AND ts.status = 'watched'
  );

-- Backfill individual preloaded episodes already finished by the preload owner.
UPDATE preload_episode pe
SET owner_watched_at = COALESCE(pe.ready_at, now())
FROM preload p
WHERE pe.preload_id = p.id
  AND p.media_kind = 'tv'
  AND pe.state = 'ready'
  AND pe.owner_watched_at IS NULL
  AND (
      EXISTS (
          SELECT 1 FROM watch_progress wp
          WHERE wp.user_id = p.user_id AND wp.media_kind = 'tv'
            AND wp.tmdb_id = p.tmdb_id AND wp.season = pe.season
            AND wp.episode = pe.episode AND wp.finished = TRUE
      )
      OR EXISTS (
          SELECT 1 FROM title_state ts
          WHERE ts.user_id = p.user_id AND ts.media_kind = 'tv'
            AND ts.tmdb_id = p.tmdb_id AND ts.status = 'watched'
      )
  );

-- Restore the 7-day retention deadline for preloads/episodes the owner has NOT watched yet.
UPDATE preload
SET delete_after = ready_at + interval '7 days'
WHERE state = 'ready'
  AND owner_watched_at IS NULL
  AND ready_at IS NOT NULL
  AND (delete_after IS NULL OR delete_after < ready_at + interval '7 days');

UPDATE preload_episode
SET delete_after = ready_at + interval '7 days'
WHERE state = 'ready'
  AND owner_watched_at IS NULL
  AND ready_at IS NOT NULL
  AND (delete_after IS NULL OR delete_after < ready_at + interval '7 days');
