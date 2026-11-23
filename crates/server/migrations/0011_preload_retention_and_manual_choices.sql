-- Track whether a release choice was picked explicitly by a user (`manual`)
-- and enforce 7-day maximum server preload retention from ready time.

ALTER TABLE release_choice
    ADD COLUMN IF NOT EXISTS manual BOOLEAN NOT NULL DEFAULT TRUE;

UPDATE preload
SET delete_after = LEAST(
    COALESCE(delete_after, ready_at + interval '7 days'),
    ready_at + interval '7 days'
)
WHERE state = 'ready' AND ready_at IS NOT NULL;
