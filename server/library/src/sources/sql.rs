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
}
