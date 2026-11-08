-- Catalogue extension 2: what identifies a playlist entry at its provider.
--
-- An imported entry is a name copy of the provider's track. Its ids are
-- kept beside it so the entry can be linked to the library track placed
-- for it (local_entity_source_links carries the same ids), and so a
-- request for it can carry them. One row per entry; it goes with the
-- entry. A later provider adds its own column in a later file.
--   spotify_track_id  22 base-62 characters
--   isrc              upper case, 12 characters

CREATE TABLE catalog_playlist_entry_identities (
    entry_id TEXT PRIMARY KEY NOT NULL
        REFERENCES catalog_playlist_tracks(id) ON DELETE CASCADE,
    spotify_track_id TEXT CHECK(spotify_track_id IS NULL OR length(spotify_track_id) = 22),
    isrc TEXT CHECK(isrc IS NULL OR (length(isrc) = 12 AND isrc = upper(isrc))),
    CHECK(spotify_track_id IS NOT NULL OR isrc IS NOT NULL)
) STRICT;

CREATE INDEX idx_catalog_playlist_entry_spotify
    ON catalog_playlist_entry_identities(spotify_track_id) WHERE spotify_track_id IS NOT NULL;
CREATE INDEX idx_catalog_playlist_entry_isrc
    ON catalog_playlist_entry_identities(isrc) WHERE isrc IS NOT NULL;
