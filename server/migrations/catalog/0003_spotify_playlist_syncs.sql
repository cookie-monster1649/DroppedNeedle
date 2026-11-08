-- Catalogue extension 3: Spotify playlists kept in step with Spotify.
--
-- A row is a playlist imported from Spotify (catalog_playlists.source_ref
-- 'spotify:<id>') whose owner turned sync on; no row, no sync. The sync
-- loop reads the playlist's snapshot id from Spotify and imports it again
-- only when it changed, mirroring it (added songs appear, removed songs
-- go). With auto_request, songs added since the last import are asked
-- for (a track request when MusicBrainz proved the recording, else a
-- want), and the playlist linker attaches them once they land.
--   snapshot_id  the Spotify snapshot last imported, NULL before the first
--   checked_at   epoch milliseconds of the last check, NULL before it
--   last_error   why the last check failed, NULL when it did not

CREATE TABLE acquire_spotify_playlist_syncs (
    playlist_id TEXT PRIMARY KEY NOT NULL
        REFERENCES catalog_playlists(id) ON DELETE CASCADE,
    auto_request INTEGER NOT NULL DEFAULT 1 CHECK(auto_request IN (0, 1)),
    snapshot_id TEXT CHECK(snapshot_id IS NULL OR length(snapshot_id) > 0),
    checked_at INTEGER,
    last_error TEXT,
    created_at INTEGER NOT NULL
) STRICT;
