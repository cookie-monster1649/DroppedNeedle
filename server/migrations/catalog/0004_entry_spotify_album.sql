-- Catalogue extension 4: the provider album a playlist entry's track is on.
--
-- A Spotify album id lets a library album built from the entry offer
-- "Add to MusicBrainz" from that album (and lets a later proof look it up).
--   spotify_album_id  22 base-62 characters

ALTER TABLE catalog_playlist_entry_identities
    ADD COLUMN spotify_album_id TEXT
        CHECK(spotify_album_id IS NULL OR length(spotify_album_id) = 22);
