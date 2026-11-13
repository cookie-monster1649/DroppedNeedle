-- Catalogue extension 14: a Spotify library kept in step.
--
-- One settings row per user who turned on a library sync. The sync runs
-- with the playlist sync. "Sync now" runs it whatever the toggles say.
--   follow_sync   followed artists are followed here (never auto-download)
--   album_sync    saved albums are read again and new ones flagged for review
CREATE TABLE acquire_spotify_library (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    follow_sync INTEGER NOT NULL DEFAULT 0 CHECK(follow_sync IN (0, 1)),
    album_sync INTEGER NOT NULL DEFAULT 0 CHECK(album_sync IN (0, 1)),
    artists_checked_at INTEGER,
    albums_checked_at INTEGER,
    last_error TEXT,
    created_at INTEGER NOT NULL
) STRICT;

-- The albums a user saved at Spotify, as last read.
--   release_group      the album matched here (a MusicBrainz release group,
--                      or a Deezer stand-in), NULL when nothing matched
--   matched_by         how: link, barcode, deezer_barcode, name, deezer_name
--   matched_at         when it was last looked up, NULL before the first
--   reviewed           0 for an album saved since the user last confirmed
--                      the review; it shows as new and is never requested
--                      by itself
--   favorite_album_id  the library album the sync marked as a favourite,
--                      so an un-save removes only a favourite it added
CREATE TABLE acquire_spotify_saved_albums (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    spotify_album_id TEXT NOT NULL CHECK(length(spotify_album_id) > 0),
    title TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    year INTEGER,
    cover_url TEXT,
    total_tracks INTEGER,
    upc TEXT,
    release_group TEXT,
    matched_by TEXT,
    matched_at INTEGER,
    saved_at INTEGER,
    first_seen_at INTEGER NOT NULL,
    reviewed INTEGER NOT NULL DEFAULT 0 CHECK(reviewed IN (0, 1)),
    favorite_album_id TEXT,
    PRIMARY KEY (user_id, spotify_album_id)
) STRICT;

-- The artists a user follows at Spotify, as last read.
--   artist_mbid        the artist matched here (MusicBrainz, or a Deezer
--                      stand-in), NULL when nothing matched
--   followed_by_sync   1 when the sync made the follow here: only such a
--                      follow is removed when the artist is unfollowed at
--                      Spotify; a follow the user made by hand never is
CREATE TABLE acquire_spotify_followed_artists (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    spotify_artist_id TEXT NOT NULL CHECK(length(spotify_artist_id) > 0),
    name TEXT NOT NULL,
    artist_mbid TEXT,
    matched_by TEXT,
    matched_at INTEGER,
    followed_by_sync INTEGER NOT NULL DEFAULT 0 CHECK(followed_by_sync IN (0, 1)),
    first_seen_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, spotify_artist_id)
) STRICT;
