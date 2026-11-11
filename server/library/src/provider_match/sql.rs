//! Fork: the SQL of an album's match outside MusicBrainz.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

droppedneedle_kernel::sql! {
    /// Listed albums nothing on MusicBrainz fits, with no MusicBrainz link
    /// and no provider match yet, or one not found before `?2`: newest
    /// first, at most `?1`.
    pub(super) PENDING_ALBUMS = "SELECT a.id AS album_id, a.title AS title, \
         COALESCE(a.album_artist_name, '') AS artist_name \
         FROM library_album_match_state s \
         JOIN local_albums a ON a.id = s.local_album_id \
         LEFT JOIN library_album_provider_matches m ON m.local_album_id = a.id \
         WHERE s.state = 'unmatched' AND " + (crate::owned_album!("a")) + " \
           AND NOT EXISTS (SELECT 1 FROM local_album_external_identities e \
             WHERE e.local_album_id = a.id) \
           AND (m.local_album_id IS NULL \
             OR (m.match_state = 'not_found' AND m.updated_at < ?2)) \
         ORDER BY m.local_album_id IS NOT NULL, s.updated_at DESC, a.id LIMIT ?1";
    /// Album `?1`'s indexed files, in album order, with an ISRC when a
    /// source link or the playlist entry the file came from carries one.
    pub(super) HELD_FILES = "SELECT t.id AS track_id, t.title AS title, \
         t.duration_seconds AS duration_seconds, \
         COALESCE((SELECT l.external_id FROM local_entity_source_links l \
            WHERE l.local_track_id = t.id AND l.provider = 'isrc' \
            AND l.external_entity_type = 'recording' ORDER BY l.external_id LIMIT 1), \
          (SELECT x.isrc FROM catalog_playlist_tracks p \
            JOIN catalog_playlist_entry_identities x ON x.entry_id = p.id \
            WHERE p.library_file_id = t.id AND x.isrc IS NOT NULL \
            ORDER BY x.isrc LIMIT 1)) AS isrc \
         FROM local_albums a \
         CROSS JOIN local_tracks t INDEXED BY idx_local_tracks_album_availability \
         WHERE a.id = ?1 AND t.local_album_id = a.id AND t.availability = 'indexed' \
         ORDER BY t.disc_number, t.track_number, t.id";
    /// Keep a match for album `?1`, unless a person already decided one.
    pub(super) PUT_MATCH = "INSERT INTO library_album_provider_matches \
         (local_album_id, provider, provider_album_id, title, artist_name, provider_artist_id, \
          artist_picture_url, release_date, label, upc, record_type, cover_url, link_url, \
          genres_json, tracks_json, score, match_state, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?18) \
         ON CONFLICT (local_album_id) DO UPDATE SET provider = excluded.provider, \
           provider_album_id = excluded.provider_album_id, title = excluded.title, \
           artist_name = excluded.artist_name, provider_artist_id = excluded.provider_artist_id, \
           artist_picture_url = excluded.artist_picture_url, release_date = excluded.release_date, \
           label = excluded.label, upc = excluded.upc, record_type = excluded.record_type, \
           cover_url = excluded.cover_url, link_url = excluded.link_url, \
           genres_json = excluded.genres_json, tracks_json = excluded.tracks_json, \
           score = excluded.score, match_state = excluded.match_state, \
           updated_at = excluded.updated_at \
         WHERE library_album_provider_matches.match_state IN ('suggested', 'not_found')";
    /// Album `?1` has nothing on the provider (`?2`), as of `?3`.
    pub(super) PUT_NOT_FOUND = "INSERT INTO library_album_provider_matches \
         (local_album_id, provider, provider_album_id, title, artist_name, score, match_state, \
          created_at, updated_at) VALUES (?1, ?2, '-', '', '', 0, 'not_found', ?3, ?3) \
         ON CONFLICT (local_album_id) DO UPDATE SET updated_at = excluded.updated_at \
         WHERE library_album_provider_matches.match_state = 'not_found'";
    /// Album `?1`'s match, whatever its state.
    pub(super) MATCH_OF = "SELECT local_album_id, provider, provider_album_id, title, artist_name, \
         provider_artist_id, artist_picture_url, release_date, label, upc, record_type, \
         cover_url, link_url, genres_json, tracks_json, score, match_state \
         FROM library_album_provider_matches WHERE local_album_id = ?1";
    /// A person settles album `?1`'s suggestion as `?2` (confirmed or
    /// declined), user `?3`, at `?4`.
    pub(super) DECIDE = "UPDATE library_album_provider_matches \
         SET match_state = ?2, decided_by_user_id = ?3, updated_at = ?4 \
         WHERE local_album_id = ?1 AND match_state IN ('suggested', 'confirmed', 'declined')";
    /// Link album `?2` to the provider's album, as row `?1`: provider `?3`,
    /// id `?4`, page `?5`, chosen by user `?6` at `?7`.
    pub(super) LINK_ALBUM = "INSERT INTO local_entity_source_links \
         (id, local_album_id, provider, external_entity_type, external_id, canonical_url, \
          decision_source, selected_by_user_id, verified_at, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'album', ?4, ?5, 'manual', ?6, ?7, ?7, ?7) \
         ON CONFLICT (local_album_id, provider, external_entity_type, external_id) \
         WHERE local_album_id IS NOT NULL DO NOTHING";
    /// Album `?1`'s indexed files as their tags stand, in album order:
    /// what a Deezer tag write plans from.
    pub(super) EDITION_FILES = "SELECT t.id AS track_id, t.title AS title, \
         t.artist_name AS artist_name, t.album_artist_name AS album_artist_name, \
         t.disc_number AS disc_number, t.track_number AS track_number, \
         t.duration_seconds AS duration_seconds \
         FROM local_albums a \
         CROSS JOIN local_tracks t INDEXED BY idx_local_tracks_album_availability \
         WHERE a.id = ?1 AND t.local_album_id = a.id AND t.availability = 'indexed' \
         ORDER BY t.disc_number, t.track_number, t.id";
    pub(super) row PendingAlbumRow for [PENDING_ALBUMS] {
        album_id: String,
        title: String,
        artist_name: String,
    }
    pub(super) row HeldFileRow for [HELD_FILES] {
        track_id: String,
        title: String,
        duration_seconds: Option<f64>,
        isrc: Option<String>,
    }
    pub(super) row MatchRow for [MATCH_OF] {
        local_album_id: String,
        provider: String,
        provider_album_id: String,
        title: String,
        artist_name: String,
        provider_artist_id: Option<String>,
        artist_picture_url: Option<String>,
        release_date: Option<String>,
        label: Option<String>,
        upc: Option<String>,
        record_type: Option<String>,
        cover_url: Option<String>,
        link_url: Option<String>,
        genres_json: String,
        tracks_json: String,
        score: f64,
        match_state: String,
    }
    pub(super) row EditionFileRow for [EDITION_FILES] {
        track_id: String,
        title: String,
        artist_name: Option<String>,
        album_artist_name: Option<String>,
        disc_number: i64,
        track_number: i64,
        duration_seconds: Option<f64>,
    }
}
