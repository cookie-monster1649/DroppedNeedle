-- Catalogue extension 5: playlist songs a person removed from the library.
--
-- Removing a playlist song's file by hand says the file was wrong and the
-- song is not to be fetched again for that playlist. Entries are rebuilt on
-- every Spotify sync, so the mark is keyed by the song, not the entry:
--   name_key          the want key the entry's artist and title give
--   spotify_track_id  the entry's Spotify track, when it has one
--   library_file_id   the removed file; restoring it clears the mark

CREATE TABLE catalog_playlist_declines (
    playlist_id TEXT NOT NULL REFERENCES catalog_playlists(id) ON DELETE CASCADE,
    name_key TEXT NOT NULL,
    spotify_track_id TEXT CHECK(spotify_track_id IS NULL OR length(spotify_track_id) = 22),
    library_file_id TEXT,
    declined_at INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, name_key)
) STRICT;

CREATE INDEX idx_catalog_playlist_declines_file
    ON catalog_playlist_declines(library_file_id) WHERE library_file_id IS NOT NULL;
