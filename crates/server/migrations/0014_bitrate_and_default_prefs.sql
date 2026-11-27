-- Maximum bitrate filter (Mbit/s) and default Russian language + bitrate limit for all users.
ALTER TABLE user_prefs
    ADD COLUMN IF NOT EXISTS max_bitrate_mbps INTEGER;

INSERT INTO user_prefs (user_id, language, max_bitrate_mbps)
SELECT id, 'Русский', 30
FROM users
ON CONFLICT (user_id) DO UPDATE SET
    language = COALESCE(user_prefs.language, 'Русский'),
    max_bitrate_mbps = COALESCE(user_prefs.max_bitrate_mbps, 30);
