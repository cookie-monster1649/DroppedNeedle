//! The statements of bulk playlist requests (`acquire_bulk_runs`,
//! `acquire_bulk_items`).

droppedneedle_kernel::sql! {
    /// Playlist `?1`'s owner.
    pub(super) PLAYLIST_OWNER = "SELECT user_id FROM catalog_playlists WHERE id = ?1";
    /// User `?1`'s role and display name.
    pub(super) OWNER = "SELECT role, COALESCE(username_display, username) \
             FROM auth_users WHERE id = ?1";
    /// The songs of playlist `?1` the library does not play, in playlist
    /// order: entry id, position, title, artist, album, release group,
    /// recording, ISRC, Spotify track id, length (ms).
    pub(super) MISSING_SONGS = "SELECT t.id, t.position, t.track_name, t.artist_name, \
             t.album_name, NULLIF(t.album_id, ''), NULLIF(lower(t.recording_mbid), ''), \
             i.isrc, i.spotify_track_id, t.duration_ms \
             FROM catalog_playlist_tracks t \
             LEFT JOIN catalog_playlist_entry_identities i ON i.entry_id = t.id \
             WHERE t.playlist_id = ?1 AND COALESCE(t.library_file_id, '') = '' \
             ORDER BY t.position, t.id";
    /// A run of playlist `?1` still under way: its id.
    pub(super) OPEN_RUN = "SELECT id FROM acquire_bulk_runs WHERE playlist_id = ?1 \
             AND state IN ('running', 'pilot_done', 'paused') LIMIT 1";
    pub(super) INSERT_RUN = "INSERT INTO acquire_bulk_runs (id, playlist_id, user_id, state, \
             per_hour, pilot, created_at, updated_at) VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6, ?6)";
    pub(super) INSERT_ITEM = "INSERT OR IGNORE INTO acquire_bulk_items (run_id, position, \
             playlist_track_id, artist_name, track_title, album_title, release_group_mbid, \
             recording_mbid, isrc, spotify_track_id, duration_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)";
    /// Every column of a run, in the order `Run::from_row` reads them.
    pub(super) fragment RUN_ROW = "SELECT id, playlist_id, user_id, state, per_hour, pilot, \
             created_at, updated_at FROM acquire_bulk_runs";
    pub(super) RUN = RUN_ROW + " WHERE id = ?1";
    pub(super) RUNS = RUN_ROW + " ORDER BY created_at DESC LIMIT 50";
    pub(super) RUNNING = RUN_ROW + " WHERE state = 'running' ORDER BY created_at";
    pub(super) SET_RUN_STATE = "UPDATE acquire_bulk_runs SET state = ?2, updated_at = ?3 \
             WHERE id = ?1";
    /// Run `?1`'s pilot is spent: the rest goes.
    pub(super) PILOT_SPENT = "UPDATE acquire_bulk_runs SET pilot = 0 WHERE id = ?1";
    /// Songs of run `?1` sent since `?2` (ms).
    pub(super) SENT_SINCE = "SELECT COUNT(*) FROM acquire_bulk_items \
             WHERE run_id = ?1 AND sent_at >= ?2";
    /// Songs of run `?1` no longer queued.
    pub(super) HANDLED = "SELECT COUNT(*) FROM acquire_bulk_items \
             WHERE run_id = ?1 AND state != 'queued'";
    /// Every column of a queued song, in the order `Song::from_row` reads.
    pub(super) fragment SONG_ROW = "SELECT id, artist_name, track_title, album_title, \
             release_group_mbid, recording_mbid, isrc, spotify_track_id, duration_ms, solo \
             FROM acquire_bulk_items";
    /// Run `?1`'s next `?2` queued songs.
    pub(super) QUEUED = SONG_ROW + " WHERE run_id = ?1 AND state = 'queued' \
             ORDER BY position, id LIMIT ?2";
    /// Run `?1`'s queued songs of release group `?2` that an album
    /// download may take.
    pub(super) QUEUED_OF_ALBUM = SONG_ROW + " WHERE run_id = ?1 AND state = 'queued' \
             AND solo = 0 AND release_group_mbid = ?2 AND recording_mbid IS NOT NULL \
             ORDER BY position, id";
    pub(super) MARK_SENT = "UPDATE acquire_bulk_items SET state = 'sent', ask_key = ?2, \
             task_id = ?3, album_group = ?4, detail = ?5, sent_at = ?6 WHERE id = ?1";
    /// Run `?1`'s songs sent within an album download that failed: song
    /// id and the download's outcome code.
    pub(super) FAILED_ALBUM_SONGS = "SELECT b.id, t.outcome_code FROM acquire_bulk_items b \
             JOIN download_tasks t ON t.id = b.task_id \
             WHERE b.run_id = ?1 AND b.album_group = 1 AND b.state = 'sent' \
             AND t.status IN ('failed', 'cancelled')";
    /// Song `?1` goes again, on its own; `?2` says why. Its send time
    /// stays, so the album download still counts against the rate.
    pub(super) REQUEUE_SOLO = "UPDATE acquire_bulk_items SET state = 'queued', solo = 1, \
             album_group = 0, ask_key = NULL, task_id = NULL, detail = ?2 \
             WHERE id = ?1";
    pub(super) MARK_SOLO = "UPDATE acquire_bulk_items SET solo = 1 WHERE id = ?1";
    pub(super) MARK_FAILED = "UPDATE acquire_bulk_items SET state = 'failed', detail = ?2, \
             sent_at = ?3 WHERE id = ?1";
    /// Run `?1`'s songs with what became of them: id, position, artist,
    /// title, album, state, album group, detail, the newest download for
    /// the song's ask (status, outcome) and whether it holds files for a
    /// person, and whether the library now plays the song.
    pub(super) ITEMS = "WITH latest AS (SELECT b.id AS item_id, \
               (SELECT n.id FROM download_tasks n \
                 WHERE n.id = b.task_id OR (b.ask_key IS NOT NULL \
                   AND n.user_id = r.user_id AND n.created_at >= b.sent_at \
                   AND ((b.album_group = 1 AND n.download_type = 'album' \
                         AND n.release_group_mbid = b.ask_key) \
                     OR (b.album_group = 0 AND n.download_type = 'track' \
                         AND n.recording_mbid = b.ask_key))) \
                 ORDER BY n.created_at DESC, n.rowid DESC LIMIT 1) AS task_id \
             FROM acquire_bulk_items b JOIN acquire_bulk_runs r ON r.id = b.run_id \
             WHERE b.run_id = ?1) \
             SELECT b.id, b.position, b.artist_name, b.track_title, b.album_title, b.state, \
               b.album_group, b.detail, t.status, t.outcome_code, \
               EXISTS (SELECT 1 FROM acquire_held_files f \
                 WHERE f.source_task_id = t.id AND f.state = 'held'), \
               EXISTS (SELECT 1 FROM catalog_playlist_tracks p \
                 WHERE p.id = b.playlist_track_id AND COALESCE(p.library_file_id, '') != '') \
             FROM acquire_bulk_items b JOIN latest l ON l.item_id = b.id \
             LEFT JOIN download_tasks t ON t.id = l.task_id \
             ORDER BY b.position, b.id";
}
