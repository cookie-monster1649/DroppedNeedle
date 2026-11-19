-- Catalogue extension 21: hearts from Spotify Liked Songs.
--
-- One row per library track a user's synced Liked Songs playlist has
-- linked. The row is kept while the song stays liked, so a heart the user
-- takes off by hand is not put back on the next sync.
--   hearted_by_sync  1 when the sync added the heart: only such a heart is
--                    removed when the song is unliked at Spotify; a track
--                    the user had hearted already is never touched
CREATE TABLE acquire_spotify_liked_hearts (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    track_id TEXT NOT NULL CHECK(length(track_id) > 0),
    hearted_by_sync INTEGER NOT NULL DEFAULT 0 CHECK(hearted_by_sync IN (0, 1)),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, track_id)
) STRICT;
