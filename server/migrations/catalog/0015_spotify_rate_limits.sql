-- Catalogue extension 15: the Spotify library sync keeps to Spotify's limits.
--
-- The wait Spotify last asked for, kept across a restart so a new process
-- never calls before it ends.
--   scope      the rate row the wait is on ('spotify')
--   until_at   epoch milliseconds the wait ends
CREATE TABLE acquire_spotify_cooldowns (
    scope TEXT PRIMARY KEY NOT NULL CHECK(length(scope) > 0),
    until_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- A followed artist's lookups.
--   attempts     lookups that matched nothing; each one waits longer
--                before the next (a day, a week, then a month)
--   fetched_at   when its albums were last read at Spotify, the one
--                lookup that calls Spotify; read again after a month
ALTER TABLE acquire_spotify_followed_artists ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE acquire_spotify_followed_artists ADD COLUMN fetched_at INTEGER;
