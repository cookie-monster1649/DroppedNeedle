-- Catalogue extension 16: the Spotify sync spends as few calls as it can.
--
-- Why Spotify last refused, kept with the wait so the card names it after
-- a restart, and the wait it set so a refusal right after it doubles it.
--   reason             'quota' (QUOTA_EXCEEDED) or 'rate_limit'
--   endpoint           the endpoint group refused
--   retry_after_secs   what Retry-After asked for, NULL when it said nothing
--   wait_ms            the wait set (the growing wait)
ALTER TABLE acquire_spotify_cooldowns ADD COLUMN reason TEXT;
ALTER TABLE acquire_spotify_cooldowns ADD COLUMN endpoint TEXT;
ALTER TABLE acquire_spotify_cooldowns ADD COLUMN retry_after_secs INTEGER;
ALTER TABLE acquire_spotify_cooldowns ADD COLUMN wait_ms INTEGER;

-- When a synced Liked Songs was last read to the end. Between such reads
-- the sync reads only down to the first page it knows.
ALTER TABLE acquire_spotify_playlist_syncs ADD COLUMN full_read_at INTEGER;

-- The user's Spotify playlists as last listed (the import page and the
-- sync both read /me/playlists), so a page view never calls Spotify. A
-- row with id 'liked' carries the Liked Songs count.
--   owned        1 when the user's Spotify account owns it
--   snapshot_id  its version when listed
--   listed_at    epoch milliseconds of the listing
CREATE TABLE acquire_spotify_listed_playlists (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    spotify_playlist_id TEXT NOT NULL CHECK(length(spotify_playlist_id) > 0),
    position INTEGER NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    owner_name TEXT NOT NULL DEFAULT '',
    owned INTEGER NOT NULL DEFAULT 0 CHECK(owned IN (0, 1)),
    track_count INTEGER NOT NULL DEFAULT 0,
    cover_url TEXT,
    snapshot_id TEXT,
    listed_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, spotify_playlist_id)
) STRICT;
