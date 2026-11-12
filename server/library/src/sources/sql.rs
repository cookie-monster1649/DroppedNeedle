//! Fork: the SQL of the metadata sources.

droppedneedle_kernel::sql! {
    /// Keep a minted id (`?1`) and what it names; minting again changes nothing.
    pub(super) KEEP_IDENTITY = "INSERT INTO source_identities \
         (uuid, provider, kind, provider_id, provider_album_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT DO NOTHING";

    /// What stand-in id `?1` names.
    pub(super) IDENTITY_OF = "SELECT uuid, provider, kind, provider_id, provider_album_id \
         FROM source_identities WHERE uuid = ?1";

    /// The album whose match is provider `?1`'s album `?2`: a confirmed one
    /// first, then the newest.
    pub(super) ALBUM_OF_MATCH = "SELECT local_album_id \
         FROM library_album_provider_matches WHERE provider = ?1 AND provider_album_id = ?2 \
         ORDER BY match_state = 'confirmed' DESC, updated_at DESC LIMIT 1";

    /// Album `?1`'s album artist's MusicBrainz id, when it has one.
    pub(super) ALBUM_ARTIST_MBID = "SELECT e.provider_artist_id \
         FROM local_albums a JOIN local_artist_external_identities e \
           ON e.local_artist_id = a.album_artist_id AND e.provider = 'musicbrainz' \
         WHERE a.id = ?1";

    /// Album `?1`'s release group, when it has an identity.
    pub(super) ALBUM_GROUP = "SELECT release_group_mbid \
         FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = 'musicbrainz'";

    /// Confirmed matches of provider `?1` whose album has no identity yet, at
    /// most `?2`.
    pub(super) UNADOPTED = "SELECT m.local_album_id \
         FROM library_album_provider_matches m JOIN local_albums a ON a.id = m.local_album_id \
         WHERE m.provider = ?1 AND m.match_state = 'confirmed' AND a.retired_into_album_id IS NULL \
           AND NOT EXISTS (SELECT 1 FROM local_album_external_identities e \
             WHERE e.local_album_id = m.local_album_id) \
           AND NOT EXISTS (SELECT 1 FROM source_promotion_checks c \
             WHERE c.local_album_id = m.local_album_id AND c.outcome = 'unplaceable' \
               AND c.checked_at >= ?3) \
         ORDER BY m.updated_at, m.local_album_id LIMIT ?2";

    /// Albums on stand-in ids not looked up at MusicBrainz since `?1`, the
    /// never-checked first, at most `?2`.
    pub(super) PROMOTION_DUE = "SELECT e.local_album_id \
         FROM local_album_external_identities e \
         LEFT JOIN source_promotion_checks c ON c.local_album_id = e.local_album_id \
         WHERE e.provider = 'musicbrainz' AND lower(e.release_group_mbid) LIKE 'd0ab1e00-50c3-8%' \
           AND (c.checked_at IS NULL OR c.checked_at < ?1) \
         ORDER BY c.checked_at IS NOT NULL, c.checked_at, e.local_album_id LIMIT ?2";

    /// Note album `?1`'s check at `?2` with outcome `?3`.
    pub(super) NOTE_CHECK = "INSERT INTO source_promotion_checks \
         (local_album_id, checked_at, outcome) VALUES (?1, ?2, ?3) \
         ON CONFLICT (local_album_id) DO UPDATE SET checked_at = excluded.checked_at, \
           outcome = excluded.outcome";
    /// Who confirmed album `?1`'s match.
    pub(super) DECIDED_BY = "SELECT decided_by_user_id \
         FROM library_album_provider_matches WHERE local_album_id = ?1";

    /// Albums on stand-in ids with a confirmed match whose source editions
    /// were not looked up since `?1`, the never-checked first, at most `?2`.
    pub(super) EDITIONS_DUE = "SELECT m.local_album_id \
         FROM library_album_provider_matches m \
         JOIN local_album_external_identities e \
           ON e.local_album_id = m.local_album_id AND e.provider = 'musicbrainz' \
         LEFT JOIN source_edition_checks c ON c.local_album_id = m.local_album_id \
         WHERE m.match_state = 'confirmed' \
           AND lower(e.release_group_mbid) LIKE 'd0ab1e00-50c3-8%' \
           AND (c.checked_at IS NULL OR c.checked_at < ?1) \
         ORDER BY c.checked_at IS NOT NULL, c.checked_at, m.local_album_id LIMIT ?2";

    /// Note album `?1`'s edition check at `?2`.
    pub(super) NOTE_EDITION_CHECK = "INSERT INTO source_edition_checks \
         (local_album_id, checked_at) VALUES (?1, ?2) \
         ON CONFLICT (local_album_id) DO UPDATE SET checked_at = excluded.checked_at";

    /// Drop provider `?1`'s editions of its album `?2`.
    pub(super) DROP_EDITIONS = "DELETE FROM source_editions \
         WHERE provider = ?1 AND parent_album_id = ?2";

    /// Keep one edition.
    pub(super) KEEP_EDITION = "INSERT INTO source_editions \
         (provider, provider_album_id, parent_album_id, title, release_date, upc, label, \
          record_type, cover_url, tracks_json, fetched_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT (provider, provider_album_id) DO UPDATE SET \
           parent_album_id = excluded.parent_album_id, title = excluded.title, \
           release_date = excluded.release_date, upc = excluded.upc, label = excluded.label, \
           record_type = excluded.record_type, cover_url = excluded.cover_url, \
           tracks_json = excluded.tracks_json, fetched_at = excluded.fetched_at";

    /// Provider `?1`'s edition `?2`: its album, title, date, UPC, label,
    /// record type, cover, tracks.
    pub(super) EDITION = "SELECT parent_album_id, title, release_date, upc, label, \
           record_type, cover_url, tracks_json \
         FROM source_editions WHERE provider = ?1 AND provider_album_id = ?2";

    /// Provider `?1`'s editions of its album `?2`, oldest first.
    pub(super) EDITIONS_OF = "SELECT provider_album_id \
         FROM source_editions WHERE provider = ?1 AND parent_album_id = ?2 \
         ORDER BY release_date IS NULL, release_date, provider_album_id";

    /// Keep provider `?1`'s album `?2` as artist `?3` (`?4`) lists it; a
    /// kept track list stays.
    pub(super) KEEP_LISTED = "INSERT INTO source_catalogue \
         (provider, provider_album_id, provider_artist_id, artist_name, title, release_date, \
          record_type, cover_url, listed_at) \
         VALUES (?1, ?2, ?3, COALESCE((SELECT m.artist_name FROM library_album_provider_matches m \
           WHERE m.provider = ?1 AND m.provider_artist_id = ?3 AND m.artist_name <> '' \
           ORDER BY m.match_state = 'confirmed' DESC LIMIT 1), ?4), ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT (provider, provider_album_id) DO UPDATE SET \
           title = excluded.title, release_date = excluded.release_date, \
           record_type = excluded.record_type, \
           cover_url = COALESCE(excluded.cover_url, source_catalogue.cover_url), \
           listed_at = excluded.listed_at";

    /// Keep provider `?1`'s album `?2` with its details and track list; an
    /// album already listed keeps the artist it was listed under.
    pub(super) KEEP_CATALOGUE_ALBUM = "INSERT INTO source_catalogue \
         (provider, provider_album_id, provider_artist_id, artist_name, title, release_date, \
          record_type, cover_url, upc, label, genres_json, tracks_json, listed_at, fetched_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13) \
         ON CONFLICT (provider, provider_album_id) DO UPDATE SET \
           artist_name = excluded.artist_name, title = excluded.title, \
           release_date = COALESCE(excluded.release_date, source_catalogue.release_date), \
           record_type = COALESCE(excluded.record_type, source_catalogue.record_type), \
           cover_url = COALESCE(excluded.cover_url, source_catalogue.cover_url), \
           upc = excluded.upc, label = excluded.label, genres_json = excluded.genres_json, \
           tracks_json = excluded.tracks_json, fetched_at = excluded.fetched_at";

    /// Provider `?1`'s catalogue album `?2` when its track list is missing
    /// or older than `?3`, and no match or edition describes it.
    pub(super) CATALOGUE_DUE = "SELECT 1 FROM source_catalogue c \
         WHERE c.provider = ?1 AND c.provider_album_id = ?2 \
           AND (c.tracks_json IS NULL OR c.fetched_at IS NULL OR c.fetched_at < ?3) \
           AND NOT EXISTS (SELECT 1 FROM library_album_provider_matches m \
             WHERE m.provider = ?1 AND m.provider_album_id = ?2) \
           AND NOT EXISTS (SELECT 1 FROM source_editions e \
             WHERE e.provider = ?1 AND e.provider_album_id = ?2)";

    /// Provider `?1`'s catalogue album `?2`: artist id and name, title,
    /// date, record type, cover, UPC, label, genres, tracks.
    pub(super) CATALOGUE_ALBUM = "SELECT provider_artist_id, artist_name, title, release_date, \
           record_type, cover_url, upc, label, genres_json, tracks_json \
         FROM source_catalogue WHERE provider = ?1 AND provider_album_id = ?2";

    /// Provider `?1`'s catalogue albums of artist `?2` that are not an
    /// edition of another album, newest first: album, title, date, type.
    pub(super) CATALOGUE_OF_ARTIST = "SELECT c.provider_album_id, c.title, c.release_date, \
           c.record_type \
         FROM source_catalogue c \
         WHERE c.provider = ?1 AND c.provider_artist_id = ?2 \
           AND NOT EXISTS (SELECT 1 FROM source_editions e \
             WHERE e.provider = c.provider AND e.provider_album_id = c.provider_album_id) \
         ORDER BY c.release_date IS NULL, c.release_date DESC, c.title";

    /// Whether a live album holds provider `?1`'s album `?2`: by release
    /// group `?3`, or by a confirmed match.
    pub(super) CATALOGUE_HELD = "SELECT EXISTS (SELECT 1 FROM local_album_external_identities i \
             JOIN local_albums b ON b.id = i.local_album_id \
           WHERE i.provider = 'musicbrainz' AND lower(i.release_group_mbid) = ?3 \
             AND b.retired_into_album_id IS NULL AND b.has_indexed_track = 1) \
         OR EXISTS (SELECT 1 FROM library_album_provider_matches m \
             JOIN local_albums b ON b.id = m.local_album_id \
           WHERE m.provider = ?1 AND m.provider_album_id = ?2 AND m.match_state = 'confirmed' \
             AND b.retired_into_album_id IS NULL AND b.has_indexed_track = 1)";
}
