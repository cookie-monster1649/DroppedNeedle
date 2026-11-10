//! The collections stores' statements. In each playlist read that names a
//! caller, `?1` is that user's id.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_kernel::db::time::Millis;

use droppedneedle_library::owns_group;

droppedneedle_kernel::sql! {
    /// The caller may read the playlist: it is theirs, or it is public.
    fragment READABLE = "(COALESCE(p.user_id = ?1, 0) OR p.is_public != 0)";
    /// The playlist is the caller's.
    fragment OWNED = "p.user_id = ?1";
    /// One playlist with its aggregates, in `PlaylistRow`'s mapper order.
    fragment PLAYLIST_COLUMNS = "SELECT p.id, p.name, p.user_id, \
         COALESCE(u.display_name, u.username), p.is_public, p.source_ref, \
         p.created_at / 1000, p.updated_at / 1000, \
         (c.playlist_id IS NOT NULL), \
         (SELECT COUNT(*) FROM catalog_playlist_tracks t WHERE t.playlist_id = p.id), \
         (SELECT SUM(CAST(t.duration_ms AS REAL)) / 1000.0 FROM catalog_playlist_tracks t \
          WHERE t.playlist_id = p.id), \
         (SELECT COUNT(*) FROM catalog_playlist_tracks t \
          WHERE t.playlist_id = p.id AND t.library_file_id IS NOT NULL), \
         (SELECT COALESCE(SUM(CAST(t.duration_ms AS REAL)), 0.0) / 1000.0 FROM catalog_playlist_tracks t \
          WHERE t.playlist_id = p.id AND t.library_file_id IS NOT NULL)";
    /// The tables a playlist row is read from.
    fragment PLAYLIST_FROM = " FROM catalog_playlists p \
         LEFT JOIN auth_users u ON u.id = p.user_id \
         LEFT JOIN catalog_playlist_covers c ON c.playlist_id = p.id";
    /// The list order.
    fragment BY_NAME = " ORDER BY p.name COLLATE NOCASE, p.id";
    /// One entry, in `PlaylistTrack`'s mapper order.
    fragment TRACK_COLUMNS = "SELECT id, position, track_name, artist_name, album_name, \
         album_id, artist_id, track_source_id, cover_url, source_type, available_sources, \
         format, track_number, disc_number, duration_ms / 1000.0, created_at / 1000, \
         plex_rating_key, library_file_id, recording_mbid, \
         (SELECT x.spotify_track_id FROM catalog_playlist_entry_identities x \
          WHERE x.entry_id = catalog_playlist_tracks.id), \
         (SELECT x.isrc FROM catalog_playlist_entry_identities x \
          WHERE x.entry_id = catalog_playlist_tracks.id) FROM catalog_playlist_tracks";

    /// Every playlist, then whether the caller may read it. A private
    /// playlist with no owner is left out unless `?2`, an admin asking.
    pub(super) LIST_ALL = PLAYLIST_COLUMNS + ", " + READABLE + PLAYLIST_FROM
        + " WHERE (p.user_id IS NOT NULL OR p.is_public != 0 OR ?2)" + BY_NAME;
    /// The playlists the caller may read.
    pub(super) LIST_READABLE = PLAYLIST_COLUMNS + PLAYLIST_FROM + " WHERE " + READABLE + BY_NAME;
    /// The first `?2` entry covers of each playlist the caller may read.
    pub(super) LIST_COVERS = "SELECT r.playlist_id, r.cover_url FROM (SELECT playlist_id, \
         cover_url, ROW_NUMBER() OVER (PARTITION BY playlist_id ORDER BY position) AS rank \
         FROM catalog_playlist_tracks WHERE cover_url IS NOT NULL AND cover_url != '') r \
         JOIN catalog_playlists p ON p.id = r.playlist_id \
         WHERE r.rank <= ?2 AND " + READABLE + " ORDER BY r.playlist_id, r.rank";
    /// One playlist `?2` the caller may read.
    pub(super) GET_READABLE = PLAYLIST_COLUMNS + PLAYLIST_FROM
        + " WHERE p.id = ?2 AND " + READABLE;
    /// One playlist `?2` of the caller's.
    pub(super) GET_OWNED = PLAYLIST_COLUMNS + PLAYLIST_FROM + " WHERE p.id = ?2 AND " + OWNED;
    /// Whether the caller owns playlist `?2`, which they may read.
    pub(super) READABLE_OWNER = "SELECT COALESCE(p.user_id = ?1, 0) FROM catalog_playlists p \
         WHERE p.id = ?2 AND " + READABLE;
    /// The first `?2` entry covers of one playlist `?1`.
    pub(super) PLAYLIST_COVERS = "SELECT cover_url FROM catalog_playlist_tracks \
         WHERE playlist_id = ?1 AND cover_url IS NOT NULL AND cover_url != '' \
         ORDER BY position LIMIT ?2";
    /// The entries of playlist `?2`, which the caller may read.
    pub(super) TRACKS_READABLE = TRACK_COLUMNS
        + " WHERE playlist_id = (SELECT p.id FROM catalog_playlists p WHERE p.id = ?2 AND "
        + READABLE + ") ORDER BY position";
    /// The entries of playlist `?2`, the caller's.
    pub(super) TRACKS_OWNED = TRACK_COLUMNS
        + " WHERE playlist_id = (SELECT p.id FROM catalog_playlists p WHERE p.id = ?2 AND "
        + OWNED + ") ORDER BY position";
    /// The uploaded cover of playlist `?2`, which the caller may read.
    pub(super) COVER_READABLE = "SELECT c.content_type, c.image FROM catalog_playlist_covers c \
         JOIN catalog_playlists p ON p.id = c.playlist_id WHERE c.playlist_id = ?2 AND " + READABLE;
    /// The uploaded cover of playlist `?2`, the caller's.
    pub(super) COVER_OWNED = "SELECT c.content_type, c.image FROM catalog_playlist_covers c \
         JOIN catalog_playlists p ON p.id = c.playlist_id WHERE c.playlist_id = ?2 AND " + OWNED;
    /// The caller's playlist imported from source `?2`.
    pub(super) FIND_BY_SOURCE = "SELECT id FROM catalog_playlists WHERE user_id = ?1 AND source_ref = ?2";
    /// For each `[track, artist, album]` in the JSON array `?2`, by its
    /// index, the playlists the caller may read holding an entry with
    /// exactly those names.
    pub(super) MEMBERSHIP = "WITH q(idx, track_name, artist_name, album_name) AS \
         (SELECT key, json_extract(value, '$[0]'), json_extract(value, '$[1]'), \
         json_extract(value, '$[2]') FROM json_each(?2)) \
         SELECT DISTINCT q.idx, t.playlist_id FROM q \
         JOIN catalog_playlist_tracks t ON t.track_name = q.track_name \
         AND t.artist_name = q.artist_name AND t.album_name = q.album_name \
         JOIN catalog_playlists p ON p.id = t.playlist_id WHERE " + READABLE
        + " ORDER BY q.idx, t.playlist_id";

    /// Whether playlist `?1` is user `?2`'s.
    pub(super) OWNED_GUARD = "SELECT 1 FROM catalog_playlists WHERE id = ?1 AND user_id = ?2";
    /// Stamp user `?3`'s playlist `?2` as changed at `?1`, milliseconds.
    pub(super) TOUCH = "UPDATE catalog_playlists SET updated_at = ?1 \
         WHERE id = ?2 AND user_id = ?3";
    /// One empty private playlist made at `?3`, milliseconds.
    pub(super) INSERT_PLAYLIST = "INSERT INTO catalog_playlists (id, name, created_at, \
         updated_at, source_ref, user_id, is_public) VALUES (?1, ?2, ?3, ?3, ?4, ?5, 0)";
    /// Rename user `?3`'s playlist `?2`.
    pub(super) RENAME = "UPDATE catalog_playlists SET name = ?1 WHERE id = ?2 AND user_id = ?3";
    /// Set the public flag of user `?3`'s playlist `?2`.
    pub(super) SET_PUBLIC = "UPDATE catalog_playlists SET is_public = ?1 WHERE id = ?2 AND user_id = ?3";
    /// Delete user `?2`'s playlist `?1`; its entries and cover go with it.
    pub(super) DELETE_PLAYLIST = "DELETE FROM catalog_playlists WHERE id = ?1 AND user_id = ?2";
    /// Delete playlist `?1` if it has no owner; its entries and cover go
    /// with it.
    pub(super) DELETE_STRAY = "DELETE FROM catalog_playlists WHERE id = ?1 AND user_id IS NULL";
    /// One playlist as a write left it: name, flag and made time in epoch
    /// seconds.
    pub(super) SAVED = "SELECT name, is_public, created_at / 1000 \
         FROM catalog_playlists WHERE id = ?1";
    /// Entry ids in position order.
    pub(super) ENTRY_ORDER = "SELECT id FROM catalog_playlist_tracks WHERE playlist_id = ?1 \
         ORDER BY position";
    /// Place one entry.
    pub(super) SET_POSITION = "UPDATE catalog_playlist_tracks SET position = ?1 WHERE id = ?2";
    /// One entry with its length `?16` and made time `?17`, milliseconds.
    pub(super) INSERT_ENTRY = "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, \
         track_name, artist_name, album_name, album_id, artist_id, track_source_id, \
         cover_url, source_type, available_sources, format, track_number, disc_number, \
         duration_ms, created_at, plex_rating_key, library_file_id, recording_mbid) VALUES \
         (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)";
    /// Every entry of one playlist.
    pub(super) CLEAR_ENTRIES = "DELETE FROM catalog_playlist_tracks WHERE playlist_id = ?1";
    /// One entry of one playlist.
    pub(super) REMOVE_ENTRY = "DELETE FROM catalog_playlist_tracks WHERE playlist_id = ?1 AND id = ?2";
    /// Whether playlist `?1` holds entry `?2`.
    pub(super) ENTRY_EXISTS = "SELECT 1 FROM catalog_playlist_tracks WHERE playlist_id = ?1 AND id = ?2";
    /// Fork: link entries the library can now play. An entry with no file
    /// takes an indexed track that is provably its track: the same
    /// MusicBrainz recording (the Spotify import proves it by ISRC), or the
    /// same Spotify track or ISRC among the track's provider links (a want
    /// placed from the entry carries them). The lowest id wins when the
    /// library holds several copies. Entries already linked are left alone,
    /// so a person's own choice stays.
    pub(super) LINK_LIBRARY = "WITH unlinked AS ( \
         SELECT p.id, lower(p.recording_mbid) AS recording, x.spotify_track_id, x.isrc \
         FROM catalog_playlist_tracks p \
         LEFT JOIN catalog_playlist_entry_identities x ON x.entry_id = p.id \
         WHERE p.library_file_id IS NULL), \
         candidates AS ( \
         SELECT u.id AS entry_id, i.local_track_id AS track_id FROM unlinked u \
         JOIN local_track_external_identities i ON i.recording_mbid = u.recording \
         UNION ALL SELECT u.id, l.local_track_id FROM unlinked u \
         JOIN local_entity_source_links l ON l.provider = 'spotify' \
         AND l.external_entity_type = 'track' AND l.external_id = u.spotify_track_id \
         UNION ALL SELECT u.id, l.local_track_id FROM unlinked u \
         JOIN local_entity_source_links l ON l.provider = 'isrc' \
         AND l.external_entity_type = 'recording' AND l.external_id = u.isrc), \
         chosen AS ( \
         SELECT c.entry_id, MIN(c.track_id) AS track_id FROM candidates c \
         WHERE (SELECT t.availability FROM local_tracks t WHERE t.id = c.track_id) = 'indexed' \
         GROUP BY c.entry_id) \
         UPDATE catalog_playlist_tracks SET library_file_id = chosen.track_id \
         FROM chosen WHERE catalog_playlist_tracks.id = chosen.entry_id";
    /// Fork: entries no id links, asked for by name: id, artist, title.
    pub(super) UNLINKED_BY_NAME = "SELECT id, artist_name, track_name \
         FROM catalog_playlist_tracks WHERE library_file_id IS NULL \
         AND COALESCE(recording_mbid, '') = ''";
    /// Fork: the newest indexed track a download for want key `?1` landed
    /// as (the request a playlist row's artist and title ask under).
    pub(super) TRACK_OF_WANT = "SELECT t.id FROM download_tasks d \
         CROSS JOIN local_tracks t ON t.download_task_id = d.id \
         WHERE d.recording_mbid = ?1 AND t.download_task_id IS NOT NULL \
         AND t.availability = 'indexed' \
         ORDER BY t.imported_at DESC, t.id LIMIT 1";
    /// Fork: link entry `?1` to track `?2`.
    pub(super) LINK_ENTRY = "UPDATE catalog_playlist_tracks SET library_file_id = ?2 \
         WHERE id = ?1 AND library_file_id IS NULL";
    /// Fork: a linked entry lists the library among its sources, which is
    /// what the player and the playlist page read to play it locally; an
    /// entry with no source yet (a Spotify import's) takes the library.
    pub(super) MARK_LINKED_LOCAL = "UPDATE catalog_playlist_tracks SET available_sources = \
         json_insert(CASE WHEN json_valid(available_sources) THEN available_sources \
         ELSE '[]' END, '$[#]', 'local'), \
         source_type = CASE WHEN source_type = '' THEN 'local' ELSE source_type END \
         WHERE library_file_id IS NOT NULL AND (available_sources IS NULL \
         OR NOT json_valid(available_sources) \
         OR NOT EXISTS (SELECT 1 FROM json_each(available_sources) s WHERE s.value = 'local'))";
    /// Fork: a linked library entry takes the shape of one added from the
    /// library: its source id is the file, its album and artist are the
    /// MusicBrainz ids of the album holding the file (so the row links where
    /// the album page is), and format and recording come from the file. An
    /// album or artist the library hasn't matched keeps what the entry had.
    pub(super) FILL_LINKED_LOCAL = "UPDATE catalog_playlist_tracks SET \
         track_source_id = library_file_id, \
         album_id = COALESCE((SELECT e.release_group_mbid FROM local_tracks t \
           JOIN local_album_external_identities e ON e.local_album_id = t.local_album_id \
           AND e.provider = 'musicbrainz' WHERE t.id = library_file_id \
           AND COALESCE(e.release_group_mbid, '') != ''), NULLIF(album_id, '')), \
         artist_id = COALESCE(NULLIF(artist_id, ''), (SELECT ae.provider_artist_id \
           FROM local_tracks t JOIN local_albums b ON b.id = t.local_album_id \
           JOIN local_artist_external_identities ae ON ae.local_artist_id = b.album_artist_id \
           AND ae.provider = 'musicbrainz' WHERE t.id = library_file_id)), \
         format = COALESCE(NULLIF(format, ''), (SELECT lower(t.file_format) \
           FROM local_tracks t WHERE t.id = library_file_id)), \
         recording_mbid = COALESCE(NULLIF(recording_mbid, ''), (SELECT i.recording_mbid \
           FROM local_track_external_identities i WHERE i.local_track_id = library_file_id \
           AND COALESCE(i.recording_mbid, '') != '' LIMIT 1)) \
         WHERE library_file_id IS NOT NULL AND source_type = 'local'";
    /// Fork: a linked library entry with a MusicBrainz album shows that
    /// album's cover, as one added from the library does, not the remote
    /// image its import carried.
    pub(super) FILL_LINKED_COVER = "UPDATE catalog_playlist_tracks SET \
         cover_url = '/api/v3/covers/release-group/' || lower(album_id) || '?size=250' \
         WHERE library_file_id IS NOT NULL AND source_type = 'local' \
         AND length(COALESCE(album_id, '')) = 36 \
         AND COALESCE(cover_url, '') NOT LIKE '/api/v3/covers/release-group/' || lower(album_id) || '%'";
    /// Fork: entries linked to a file a person removed (gone from the
    /// library with a `remove_track` action): entry, playlist, artist,
    /// title, Spotify track and file.
    pub(super) LINKED_REMOVED = "SELECT p.id, p.playlist_id, p.artist_name, p.track_name, \
         x.spotify_track_id, p.library_file_id FROM catalog_playlist_tracks p \
         JOIN local_tracks t ON t.id = p.library_file_id \
         LEFT JOIN catalog_playlist_entry_identities x ON x.entry_id = p.id \
         WHERE t.availability != 'indexed' AND EXISTS (SELECT 1 \
         FROM library_catalog_actions a WHERE a.local_track_id = t.id \
         AND a.action_kind = 'remove_track')";
    /// Fork: mark song `?2` (want key) of playlist `?1` not to be fetched
    /// again, with its Spotify track `?3`, removed file `?4` and time `?5`.
    pub(super) DECLINE = "INSERT INTO catalog_playlist_declines (playlist_id, name_key, \
         spotify_track_id, library_file_id, declined_at) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(playlist_id, name_key) DO UPDATE SET \
         spotify_track_id = COALESCE(excluded.spotify_track_id, spotify_track_id), \
         library_file_id = excluded.library_file_id, declined_at = excluded.declined_at";
    /// Fork: unlink entry `?1` from its removed file, the library no longer
    /// among its sources.
    pub(super) UNLINK_REMOVED = "UPDATE catalog_playlist_tracks SET library_file_id = NULL, \
         available_sources = (SELECT json_group_array(s.value) FROM json_each( \
           CASE WHEN json_valid(available_sources) THEN available_sources ELSE '[]' END) s \
           WHERE s.value != 'local') \
         WHERE id = ?1";
    /// Fork: a removed file put back from the recycle bin lifts its mark.
    /// Starts from the marks, so the track table is reached by id.
    pub(super) UNDECLINE_RESTORED = "DELETE FROM catalog_playlist_declines \
         WHERE library_file_id IS NOT NULL AND EXISTS (SELECT 1 FROM local_tracks t \
           WHERE t.id = catalog_playlist_declines.library_file_id \
           AND t.availability = 'indexed')";
    /// Fork: an entry's provider ids (`catalog_playlist_entry_identities`).
    pub(super) INSERT_ENTRY_IDENTITY = "INSERT INTO catalog_playlist_entry_identities \
         (entry_id, spotify_track_id, isrc, spotify_album_id) VALUES (?1, ?2, ?3, ?4)";
    /// An entry's preferred source. A recording belongs to the source that
    /// gave it, so another source starts with none.
    pub(super) SET_SOURCE_TYPE = "UPDATE catalog_playlist_tracks SET source_type = ?1, \
         recording_mbid = CASE WHEN source_type = ?1 THEN recording_mbid END \
         WHERE id = ?2 AND playlist_id = ?3";
    /// An entry's known sources, as a JSON list.
    pub(super) SET_AVAILABLE_SOURCES = "UPDATE catalog_playlist_tracks SET available_sources = ?1 \
         WHERE id = ?2 AND playlist_id = ?3";
    /// Store or replace one playlist's uploaded cover.
    pub(super) UPSERT_COVER = "INSERT INTO catalog_playlist_covers (playlist_id, content_type, image, \
         updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (playlist_id) DO UPDATE SET \
         content_type = excluded.content_type, image = excluded.image, \
         updated_at = excluded.updated_at";
    /// Delete one playlist's uploaded cover.
    pub(super) DELETE_COVER = "DELETE FROM catalog_playlist_covers WHERE playlist_id = ?1";

    /// A user's favorites, of one kind `?2` or every kind, newest first.
    pub(super) FAVORITES = "SELECT f.item_id AS item_id, \
         n.display_name AS display_name, f.created_at / 1000.0 AS created_at \
         FROM catalog_user_favorites f LEFT JOIN catalog_user_favorite_names n \
         ON n.user_id = f.user_id AND n.item_kind = f.item_kind AND n.item_id = f.item_id \
         WHERE f.user_id = ?1 AND (?2 IS NULL OR f.item_kind = ?2) \
         ORDER BY f.created_at DESC, f.item_kind, f.item_id";
    pub(super) row FavoriteColumns for [FAVORITES] {
        item_id: String,
        display_name: Option<String>,
        created_at: f64,
    }
    /// When each of some items was favorited: item id, then the time.
    family STARRED_AT = starred_at;
    pub(super) row Starred for family [starred_at] {
        item_id: String,
        created_at: Millis,
    }
    /// Favorite one item; an existing row keeps its date.
    pub(super) ADD_FAVORITE = "INSERT INTO catalog_user_favorites \
         (user_id, item_kind, item_id, created_at) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (user_id, item_kind, item_id) DO NOTHING";
    /// Save or replace the name shown for one favorite.
    pub(super) NAME_FAVORITE = "INSERT INTO catalog_user_favorite_names \
         (user_id, item_kind, item_id, display_name) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (user_id, item_kind, item_id) DO UPDATE SET \
         display_name = excluded.display_name";
    /// Delete the name saved for one favorite.
    pub(super) UNNAME_FAVORITE = "DELETE FROM catalog_user_favorite_names \
         WHERE user_id = ?1 AND item_kind = ?2 AND item_id = ?3";
    /// Unfavorite one item.
    pub(super) REMOVE_FAVORITE = "DELETE FROM catalog_user_favorites \
         WHERE user_id = ?1 AND item_kind = ?2 AND item_id = ?3";

    /// Clear a user's rating of one item.
    pub(super) CLEAR_RATING = "DELETE FROM catalog_user_ratings \
         WHERE user_id = ?1 AND item_kind = ?2 AND item_id = ?3";
    /// Set a user's rating of one item.
    pub(super) SET_RATING = "INSERT INTO catalog_user_ratings \
         (user_id, item_kind, item_id, rating, rated_at) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (user_id, item_kind, item_id) DO UPDATE SET \
         rating = excluded.rating, rated_at = excluded.rated_at";
    /// Some items' ratings: item id, the user's own, the mean of everyone's.
    family RATINGS = ratings;
    /// The ids of one kind a user rated, highest first, then the most
    /// recently rated.
    pub(super) RATED_IDS = "SELECT item_id FROM catalog_user_ratings \
         WHERE user_id = ?1 AND item_kind = ?2 ORDER BY rating DESC, rated_at DESC, item_id";

    /// One follow with its follower and approval verdict.
    fragment FOLLOW_SELECT = "SELECT f.user_id AS user_id, \
         COALESCE(u.username_display, u.username, u.display_name, f.user_id) AS user_name, \
         COALESCE(u.role, 'user') AS user_role, f.artist_mbid AS artist_mbid, \
         f.artist_name AS artist_name, f.auto_download AS auto_download, \
         a.state AS approval_state, a.requested_at / 1000.0 AS requested_at, \
         f.followed_at / 1000.0 AS followed_at, f.updated_at / 1000.0 AS updated_at \
         FROM acquire_followed_artists f \
         LEFT JOIN auth_users u ON u.id = f.user_id \
         LEFT JOIN auto_download_approvals a \
         ON a.user_id = f.user_id AND a.artist_mbid_lower = f.artist_mbid_lower";
    /// User `?1`'s follow of lowercased artist `?2`.
    pub(super) FOLLOW = FOLLOW_SELECT + " WHERE f.user_id = ?1 AND f.artist_mbid_lower = ?2";
    /// A user's follows, newest first.
    pub(super) FOLLOWS = FOLLOW_SELECT
        + " WHERE f.user_id = ?1 ORDER BY f.followed_at DESC, f.artist_mbid_lower";
    pub(super) row FollowColumns for [FOLLOW, FOLLOWS] {
        user_id: String,
        user_name: String,
        user_role: String,
        artist_mbid: String,
        artist_name: String,
        auto_download: bool,
        approval_state: Option<String>,
        requested_at: Option<f64>,
        followed_at: f64,
        updated_at: f64,
    }
    /// The albums that own the feed row `n`'s release group.
    fragment OWNERS = (owns_group!("= +n.release_group_mbid_lower"));
    /// How many release sightings a filter matches.
    family RELEASE_COUNT = release_count;
    /// One page of release sightings, in `ReleaseRow`'s mapper order.
    family RELEASE_PAGE = release_page;
    /// Releases not yet owned, discovered after user `?1`'s seen marker.
    pub(super) UNSEEN_RELEASES = "SELECT COUNT(*) FROM acquire_new_release_feed n \
         JOIN acquire_followed_artists f \
         ON f.artist_mbid_lower = n.artist_mbid_lower AND f.user_id = ?1 \
         WHERE NOT EXISTS (" + OWNERS + ") \
         AND n.discovered_at > COALESCE( \
         (SELECT seen_at FROM catalog_new_release_seen_marks WHERE user_id = ?1), 0)";
    /// Move a user's seen marker.
    pub(super) MARK_SEEN = "INSERT INTO catalog_new_release_seen_marks (user_id, seen_at) \
         VALUES (?1, ?2) ON CONFLICT (user_id) DO UPDATE SET seen_at = excluded.seen_at";

    /// Mark the tracks of one scope `?3` played or unplayed for user `?1`
    /// at `?2`, milliseconds.
    family MARK_PLAY = mark_play;
}

/// `?, ?, ...` placeholders for an IN-list.
fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(", ")
}

/// `STARRED_AT`: the user, the kind, then the ids.
pub(super) fn starred_at(ids: ListLen) -> String {
    format!(
        "SELECT item_id, created_at FROM catalog_user_favorites \
         WHERE user_id = ? AND item_kind = ? AND item_id IN ({})",
        marks(ids)
    )
}

/// `RATINGS`: the user, the kind, then the ids.
pub(super) fn ratings(ids: ListLen) -> String {
    format!(
        "SELECT item_id, MAX(CASE WHEN user_id = ? THEN rating END), AVG(rating) \
         FROM catalog_user_ratings WHERE item_kind = ? AND item_id IN ({}) GROUP BY item_id",
        marks(ids)
    )
}

/// The rows a release list reads. The user binds first, then with `since`
/// a date and a discovery time: a release dated on or after the date or,
/// undated, discovered at or after the time.
fn release_from(owned_too: bool, since: bool) -> String {
    let mut from = "FROM acquire_new_release_feed n JOIN acquire_followed_artists f \
         ON f.artist_mbid_lower = n.artist_mbid_lower AND f.user_id = ? WHERE 1 = 1"
        .to_owned();
    if !owned_too {
        from.push_str(&format!(" AND NOT EXISTS ({OWNERS})"));
    }
    if since {
        from.push_str(
            " AND (n.first_release_date >= ? \
             OR (n.first_release_date IS NULL AND n.discovered_at >= ?))",
        );
    }
    from
}

/// `RELEASE_COUNT`.
pub(super) fn release_count(owned_too: bool, since: bool) -> String {
    format!("SELECT COUNT(*) {}", release_from(owned_too, since))
}

/// `RELEASE_PAGE`: the binds of [`release_from`], then limit and offset.
pub(super) fn release_page(owned_too: bool, since: bool) -> String {
    format!(
        "SELECT n.release_group_mbid, n.title, n.artist_name, f.artist_mbid, n.primary_type, \
         n.first_release_date, EXISTS ({OWNERS}) \
         {} ORDER BY n.first_release_date DESC, n.discovered_at DESC LIMIT ? OFFSET ?",
        release_from(owned_too, since)
    )
}

droppedneedle_kernel::sql_choice! {
    /// What a played or unplayed mark covers.
    pub(super) enum MarkTracks {
        /// One track row.
        Track,
        /// Every track row of an album, apart from the missing rows that
        /// continued into another track.
        Album,
        /// The streamable tracks on the albums an artist leads.
        AlbumArtist,
    }
}

/// `MARK_PLAY`: played keeps an earlier unplayed mark; unplayed clears
/// the played one.
pub(super) fn mark_play(tracks: MarkTracks, played: bool) -> String {
    let tracks = match tracks {
        MarkTracks::Track => "SELECT id FROM local_tracks WHERE id = ?3",
        MarkTracks::Album => {
            "SELECT id FROM local_tracks WHERE local_album_id = ?3 \
             AND id NOT IN (SELECT alias FROM local_track_aliases)"
        }
        MarkTracks::AlbumArtist => {
            "SELECT t.id FROM local_tracks t JOIN local_albums a ON a.id = t.local_album_id \
             WHERE +t.availability = 'indexed' AND a.album_artist_id = ?3"
        }
    };
    let (values, update) = if played {
        ("?2, NULL", "marked_played_at = excluded.marked_played_at")
    } else {
        (
            "NULL, ?2",
            "marked_unplayed_at = excluded.marked_unplayed_at, marked_played_at = NULL",
        )
    };
    format!(
        "INSERT INTO catalog_user_play_marks \
         (user_id, local_track_id, marked_played_at, marked_unplayed_at) \
         SELECT ?1, id, {values} FROM ({tracks}) WHERE true \
         ON CONFLICT (user_id, local_track_id) DO UPDATE SET {update}"
    )
}
