//! The statements of the Spotify import: authorize states
//! (`spotify_auth_requests`), links (`user_connections`) and the source
//! refs of imported playlists (`catalog_playlists`).

droppedneedle_kernel::sql! {
    /// Drop the states that expired by `?1`.
    pub(super) PRUNE_STATES = "DELETE FROM spotify_auth_requests WHERE expires_at <= ?1";
    /// A new authorize state.
    pub(super) INSERT_STATE = "INSERT INTO spotify_auth_requests \
             (state, user_id, redirect_uri, expires_at) VALUES (?1, ?2, ?3, ?4)";
    /// The user, redirect URI and expiry of state `?1`.
    pub(super) STATE_BY_KEY = "SELECT user_id, redirect_uri, expires_at \
             FROM spotify_auth_requests WHERE state = ?1";
    /// Drop state `?1`.
    pub(super) DELETE_STATE = "DELETE FROM spotify_auth_requests WHERE state = ?1";
    /// The sealed document of user `?1`'s enabled link to service `?2`.
    pub(super) LINK_DOCUMENT = "SELECT connection_data FROM user_connections \
             WHERE user_id = ?1 AND service = ?2 AND enabled = 1";
    /// User `?1`'s playlists imported from Spotify, as `(source_ref, id)`.
    pub(super) SPOTIFY_SOURCE_REFS = "SELECT source_ref, id FROM catalog_playlists \
             WHERE user_id = ?1 AND source_ref LIKE 'spotify:%'";
    /// Fork: whether playlist `?1` is user `?2`'s, and from Spotify.
    pub(super) OWNED_PLAYLIST_SOURCE = "SELECT COALESCE(source_ref LIKE 'spotify:%', 0) \
             FROM catalog_playlists WHERE id = ?1 AND user_id = ?2";
    /// Fork: the Spotify playlist id of user `?2`'s playlist `?1`; no row
    /// when it is not theirs, NULL when it is not from Spotify.
    pub(super) OWNED_SPOTIFY_ID = "SELECT CASE WHEN source_ref LIKE 'spotify:%' \
             THEN substr(source_ref, 9) END FROM catalog_playlists WHERE id = ?1 AND user_id = ?2";
    /// Fork: playlist `?1`'s sync setting.
    pub(super) SYNC_OF = "SELECT auto_request, checked_at, last_error \
             FROM acquire_spotify_playlist_syncs WHERE playlist_id = ?1";
    /// Fork: turn sync on for playlist `?1` with auto-request `?2` at `?3`.
    pub(super) UPSERT_SYNC = "INSERT INTO acquire_spotify_playlist_syncs \
             (playlist_id, auto_request, created_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT (playlist_id) DO UPDATE SET auto_request = excluded.auto_request";
    /// Fork: turn sync off for playlist `?1`.
    pub(super) DELETE_SYNC = "DELETE FROM acquire_spotify_playlist_syncs WHERE playlist_id = ?1";
    /// Fork: every synced playlist from Spotify, least recently checked
    /// first: id, owner, Spotify id, auto-request, snapshot.
    pub(super) SYNC_ROWS = "SELECT s.playlist_id, p.user_id, substr(p.source_ref, 9), \
             s.auto_request, s.snapshot_id FROM acquire_spotify_playlist_syncs s \
             JOIN catalog_playlists p ON p.id = s.playlist_id \
             WHERE p.source_ref LIKE 'spotify:%' AND p.user_id IS NOT NULL \
             ORDER BY COALESCE(s.checked_at, 0), s.playlist_id";
    /// Fork: record a check of playlist `?1`: snapshot `?2` (kept when
    /// NULL), at `?3`, failing with `?4`.
    pub(super) RECORD_SYNC = "UPDATE acquire_spotify_playlist_syncs SET \
             snapshot_id = COALESCE(?2, snapshot_id), checked_at = ?3, last_error = ?4 \
             WHERE playlist_id = ?1";
    /// Fork: a playlist owner's role and the name their requests show.
    /// Fork: songs asked for by name not yet fetched (failed, or queued
    /// and not started), at most `?1`: request key, owner, artist, album,
    /// title, length, task.
    pub(super) OPEN_WANTS = "SELECT r.musicbrainz_id_lower, r.user_id, r.artist_name, \
             r.album_title, r.track_title, r.duration_seconds, r.download_task_id \
             FROM request_history r JOIN download_tasks t ON t.id = r.download_task_id \
             WHERE r.request_kind = 'track' AND r.musicbrainz_id_lower LIKE 'track:want-%' \
               AND (t.status = 'failed' OR (t.status = 'queued' AND r.status != 'failed')) \
             ORDER BY r.requested_at LIMIT ?1";
    pub(super) SYNC_OWNER = "SELECT role, COALESCE(username_display, username) \
             FROM auth_users WHERE id = ?1";
    /// Fork: the entries of playlist `?1`, user `?2`'s: id, recording,
    /// artist, title, and whether the library plays it.
    pub(super) ROW_ENTRIES = "SELECT t.id, t.recording_mbid, t.artist_name, t.track_name, \
             t.library_file_id IS NOT NULL, x.spotify_track_id FROM catalog_playlist_tracks t \
             JOIN catalog_playlists p ON p.id = t.playlist_id \
             LEFT JOIN catalog_playlist_entry_identities x ON x.entry_id = t.id \
             WHERE t.playlist_id = ?1 AND p.user_id = ?2 ORDER BY t.position";
    /// Fork: whether user `?1` has a file of track key `?2` (recording or
    /// want key) waiting in the held list.
    pub(super) ROW_HELD = "SELECT EXISTS (SELECT 1 FROM acquire_held_entries e \
             JOIN acquire_held_files f ON f.entry_id = e.id \
             WHERE e.user_id = ?1 AND e.state = 'held' AND f.state = 'held' \
             AND (lower(e.recording_mbid) = ?2 OR lower(f.recording_mbid) = ?2))";
    /// Fork: whether playlist `?1` marks the song with want key `?2` or
    /// Spotify track `?3` not to be fetched again (its file was removed).
    /// Answers with the newest mark's time, seconds, or no row.
    pub(super) ROW_DECLINED = "SELECT MAX(declined_at) FROM catalog_playlist_declines \
             WHERE playlist_id = ?1 AND (name_key = ?2 OR (?3 IS NOT NULL AND spotify_track_id = ?3)) \
             HAVING COUNT(*) > 0";
    /// Fork: the request status of track key `?1` and when it was asked, seconds.
    pub(super) ROW_REQUEST = "SELECT status, requested_at / 1000 FROM request_history \
             WHERE musicbrainz_id_lower = 'track:' || ?1";

    // Fork: the Spotify library (spotify_library.rs).

    /// Users with a library sync on, when their artists were last checked,
    /// and whether a followed artist was never looked up (a sync cut short).
    pub(crate) LIBRARY_USERS = "SELECT user_id, follow_sync, album_sync, artists_checked_at, \
         EXISTS (SELECT 1 FROM acquire_spotify_followed_artists AS a \
         WHERE a.user_id = acquire_spotify_library.user_id AND a.matched_at IS NULL) \
         FROM acquire_spotify_library \
         WHERE follow_sync = 1 OR album_sync = 1";
    /// One user's library settings.
    pub(crate) LIBRARY_ROW = "SELECT follow_sync, album_sync, artists_checked_at, albums_checked_at, \
         last_error FROM acquire_spotify_library WHERE user_id = ?1";
    /// Save one user's toggles.
    pub(crate) SET_LIBRARY = "INSERT INTO acquire_spotify_library (user_id, follow_sync, album_sync, \
         created_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (user_id) DO UPDATE SET \
         follow_sync = excluded.follow_sync, album_sync = excluded.album_sync";
    /// Make sure one user has a settings row.
    pub(crate) ENSURE_LIBRARY = "INSERT INTO acquire_spotify_library (user_id, created_at) \
         VALUES (?1, ?2) ON CONFLICT (user_id) DO NOTHING";
    /// Record a finished artist check.
    pub(crate) ARTISTS_CHECKED = "UPDATE acquire_spotify_library SET artists_checked_at = ?2, \
         last_error = ?3 WHERE user_id = ?1";
    /// Record a finished album check.
    pub(crate) ALBUMS_CHECKED = "UPDATE acquire_spotify_library SET albums_checked_at = ?2, \
         last_error = ?3 WHERE user_id = ?1";
    /// Record a failed check.
    pub(crate) LIBRARY_FAILED = "UPDATE acquire_spotify_library SET last_error = ?2 WHERE user_id = ?1";

    /// One user's saved albums, as last read.
    pub(crate) SAVED_ALBUMS = "SELECT spotify_album_id, title, artist_name, year, cover_url, \
         release_group, matched_by, matched_at, reviewed, favorite_album_id \
         FROM acquire_spotify_saved_albums WHERE user_id = ?1 \
         ORDER BY saved_at DESC, first_seen_at DESC, title";
    /// Count the albums flagged new.
    pub(crate) NEW_ALBUMS = "SELECT COUNT(*) FROM acquire_spotify_saved_albums \
         WHERE user_id = ?1 AND reviewed = 0";
    /// Save one album as read.
    pub(crate) UPSERT_ALBUM = "INSERT INTO acquire_spotify_saved_albums (user_id, spotify_album_id, \
         title, artist_name, year, cover_url, total_tracks, upc, saved_at, first_seen_at, \
         reviewed) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT (user_id, spotify_album_id) DO UPDATE SET title = excluded.title, \
         artist_name = excluded.artist_name, year = excluded.year, \
         cover_url = excluded.cover_url, total_tracks = excluded.total_tracks, \
         upc = excluded.upc, saved_at = excluded.saved_at";
    /// Record one album's lookup.
    pub(crate) MATCH_ALBUM = "UPDATE acquire_spotify_saved_albums SET release_group = ?3, \
         matched_by = ?4, matched_at = ?5 WHERE user_id = ?1 AND spotify_album_id = ?2";
    /// Record the favourite the sync added.
    pub(crate) FAVORITED_ALBUM = "UPDATE acquire_spotify_saved_albums SET favorite_album_id = ?3 \
         WHERE user_id = ?1 AND spotify_album_id = ?2";
    /// Forget one un-saved album.
    pub(crate) DROP_ALBUM = "DELETE FROM acquire_spotify_saved_albums \
         WHERE user_id = ?1 AND spotify_album_id = ?2";
    /// Confirm the review: nothing is new any more.
    pub(crate) REVIEW_ALBUMS = "UPDATE acquire_spotify_saved_albums SET reviewed = 1 WHERE user_id = ?1";
    /// Whether a request for an album is open or done.
    pub(crate) REQUESTED = "SELECT 1 FROM request_history WHERE musicbrainz_id_lower = ?1 \
         AND status NOT IN ('cancelled', 'failed', 'rejected')";

    /// One user's followed artists, as last read.
    pub(crate) FOLLOWED = "SELECT spotify_artist_id, name, artist_mbid, matched_at, followed_by_sync, \
         attempts, fetched_at FROM acquire_spotify_followed_artists WHERE user_id = ?1";
    /// Save one artist as read.
    pub(crate) UPSERT_ARTIST = "INSERT INTO acquire_spotify_followed_artists (user_id, spotify_artist_id, \
         name, first_seen_at) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (user_id, spotify_artist_id) DO UPDATE SET name = excluded.name";
    /// Record one artist's lookup and whether the sync made the follow.
    pub(crate) MATCH_ARTIST = "UPDATE acquire_spotify_followed_artists SET artist_mbid = ?3, \
         matched_by = ?4, matched_at = ?5, followed_by_sync = ?6 \
         WHERE user_id = ?1 AND spotify_artist_id = ?2";
    /// Record one artist's lookup that matched nothing yet, before any call
    /// it makes to Spotify (`?4`, when it reads the artist's albums there),
    /// so a lookup cut short waits its turn like any other.
    pub(crate) ATTEMPT_ARTIST = "UPDATE acquire_spotify_followed_artists SET matched_at = ?3, \
         attempts = attempts + 1, fetched_at = COALESCE(?4, fetched_at) \
         WHERE user_id = ?1 AND spotify_artist_id = ?2";
    /// Forget one unfollowed artist.
    pub(crate) DROP_ARTIST = "DELETE FROM acquire_spotify_followed_artists \
         WHERE user_id = ?1 AND spotify_artist_id = ?2";

    /// The end of the wait Spotify last asked for on rate row `?1`.
    pub(crate) COOLDOWN_UNTIL = "SELECT until_at FROM acquire_spotify_cooldowns WHERE scope = ?1";
    /// Keep the end of the wait Spotify asked for on rate row `?1`.
    pub(crate) KEEP_COOLDOWN = "INSERT INTO acquire_spotify_cooldowns (scope, until_at, updated_at) \
         VALUES (?1, ?2, ?3) ON CONFLICT (scope) DO UPDATE SET until_at = excluded.until_at, \
         updated_at = excluded.updated_at";
}
