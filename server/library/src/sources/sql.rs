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
           title = excluded.title, \
           release_date = COALESCE(excluded.release_date, source_catalogue.release_date), \
           record_type = COALESCE(excluded.record_type, source_catalogue.record_type), \
           cover_url = COALESCE(excluded.cover_url, source_catalogue.cover_url), \
           artist_name = CASE WHEN source_catalogue.artist_name = '' \
             THEN excluded.artist_name ELSE source_catalogue.artist_name END, \
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
    /// group `?3` or the group it is linked to, or by a confirmed match.
    pub(super) CATALOGUE_HELD = "SELECT EXISTS (SELECT 1 FROM local_album_external_identities i \
             JOIN local_albums b ON b.id = i.local_album_id \
           WHERE i.provider = 'musicbrainz' AND (lower(i.release_group_mbid) = ?3 \
               OR lower(i.release_group_mbid) = \
                 (SELECT musicbrainz_id FROM source_links WHERE stand_in = ?3)) \
             AND b.retired_into_album_id IS NULL AND b.has_indexed_track = 1) \
         OR EXISTS (SELECT 1 FROM library_album_provider_matches m \
             JOIN local_albums b ON b.id = m.local_album_id \
           WHERE m.provider = ?1 AND m.provider_album_id = ?2 AND m.match_state = 'confirmed' \
             AND b.retired_into_album_id IS NULL AND b.has_indexed_track = 1)";

    /// Keep provider `?1`'s artist `?2` named `?3` with picture `?4`.
    pub(super) KEEP_SOURCE_ARTIST = "INSERT INTO source_artists \
         (provider, provider_artist_id, name, picture_url, kept_at) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (provider, provider_artist_id) DO UPDATE SET name = excluded.name, \
           picture_url = COALESCE(excluded.picture_url, source_artists.picture_url), \
           kept_at = excluded.kept_at";

    /// The name of provider `?1`'s artist `?2`: as search found it, else as
    /// a confirmed match or the catalogue credits it.
    pub(super) SOURCE_ARTIST_NAME = "SELECT name FROM ( \
           SELECT name, 0 AS rank FROM source_artists \
            WHERE provider = ?1 AND provider_artist_id = ?2 \
           UNION ALL SELECT artist_name, 1 FROM library_album_provider_matches \
            WHERE provider = ?1 AND provider_artist_id = ?2 AND artist_name <> '' \
           UNION ALL SELECT artist_name, 2 FROM source_catalogue \
            WHERE provider = ?1 AND provider_artist_id = ?2 AND artist_name <> '') \
         ORDER BY rank LIMIT 1";

    // Fork: stand-in groups linked to the MusicBrainz group they are.

    /// Link stand-in `?1` to MusicBrainz group `?2`, shown by `?3`; a
    /// stand-in keeps its first link.
    pub(super) KEEP_LINK = "INSERT INTO source_links (stand_in, musicbrainz_id, via, linked_at) \
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING";

    /// The MusicBrainz group stand-in `?1` is linked to.
    pub(super) LINKED = "SELECT musicbrainz_id FROM source_links WHERE stand_in = ?1";

    /// Every linked stand-in.
    pub(super) ALL_LINKED = "SELECT stand_in FROM source_links";

    /// Each stand-in album download with files in the library: the
    /// stand-in, the title asked for, the files' album's title and group
    /// (if any), the group the files carry.
    pub(super) TASK_FILE_GROUPS = "SELECT DISTINCT lower(t.release_group_mbid), t.album_title, \
           a.title, i.release_group_mbid, lt.embedded_release_group_mbid \
         FROM download_tasks t \
         JOIN local_tracks lt ON lt.download_task_id = t.id AND lt.download_task_id IS NOT NULL \
           AND +lt.availability = 'indexed' \
         JOIN local_albums a ON a.id = lt.local_album_id AND a.retired_into_album_id IS NULL \
         LEFT JOIN local_album_external_identities i \
           ON i.local_album_id = a.id AND i.provider = 'musicbrainz' \
         WHERE t.download_type = 'album' \
           AND substr(lower(t.release_group_mbid), 1, 15) = 'd0ab1e00-50c3-8' \
           AND NOT EXISTS (SELECT 1 FROM source_links l WHERE l.stand_in = lower(t.release_group_mbid))";

    /// Each album held entry on a stand-in whose downloaded files went in,
    /// the same way.
    pub(super) HELD_FILE_GROUPS = "SELECT DISTINCT lower(e.release_group_mbid), e.album_title, \
           a.title, i.release_group_mbid, lt.embedded_release_group_mbid \
         FROM acquire_held_entries e \
         JOIN acquire_held_files f ON f.entry_id = e.id AND f.source_task_id IS NOT NULL \
         JOIN local_tracks lt ON lt.download_task_id = f.source_task_id \
           AND lt.download_task_id IS NOT NULL AND +lt.availability = 'indexed' \
         JOIN local_albums a ON a.id = lt.local_album_id AND a.retired_into_album_id IS NULL \
         LEFT JOIN local_album_external_identities i \
           ON i.local_album_id = a.id AND i.provider = 'musicbrainz' \
         WHERE e.recording_mbid IS NULL \
           AND substr(lower(e.release_group_mbid), 1, 15) = 'd0ab1e00-50c3-8' \
           AND NOT EXISTS (SELECT 1 FROM source_links l WHERE l.stand_in = lower(e.release_group_mbid))";

    /// Provider `?1`'s confirmed album matches whose live album has a
    /// MusicBrainz group: the provider album, the group.
    pub(super) MATCHED_GROUPS = "SELECT m.provider_album_id, i.release_group_mbid \
         FROM library_album_provider_matches m \
         JOIN local_albums a ON a.id = m.local_album_id AND a.retired_into_album_id IS NULL \
         JOIN local_album_external_identities i \
           ON i.local_album_id = m.local_album_id AND i.provider = 'musicbrainz' \
         WHERE m.provider = ?1 AND m.match_state = 'confirmed' \
           AND substr(lower(i.release_group_mbid), 1, 15) <> 'd0ab1e00-50c3-8'";

    /// Stand-in groups still asked for and not linked: open album
    /// requests, wanted watches, album downloads, open held entries.
    pub(super) ASKED_STAND_INS = "SELECT DISTINCT id FROM ( \
           SELECT lower(musicbrainz_id) AS id FROM request_history \
             WHERE request_kind = 'album' AND status NOT IN ('imported', 'cancelled', 'rejected') \
           UNION ALL SELECT release_group_mbid_lower FROM wanted_watches WHERE state = 'watching' \
           UNION ALL SELECT lower(release_group_mbid) FROM download_tasks WHERE download_type = 'album' \
           UNION ALL SELECT lower(release_group_mbid) FROM acquire_held_entries \
             WHERE state IN ('held', 'placing') AND recording_mbid IS NULL \
             AND release_group_mbid IS NOT NULL) \
         WHERE substr(id, 1, 15) = 'd0ab1e00-50c3-8' \
           AND NOT EXISTS (SELECT 1 FROM source_links l WHERE l.stand_in = id) \
         ORDER BY id";

    /// The live albums holding files by the artist folded `?1` that have a
    /// MusicBrainz group: title, group.
    pub(super) ARTIST_ALBUM_GROUPS = "SELECT a.title, i.release_group_mbid \
         FROM local_albums a \
         JOIN local_artists r ON r.id = a.album_artist_id \
         JOIN local_album_external_identities i \
           ON i.local_album_id = a.id AND i.provider = 'musicbrainz' \
         WHERE (r.folded_name = ?1 OR a.album_artist_name_folded = ?1) \
           AND a.retired_into_album_id IS NULL AND a.has_indexed_track = 1 \
           AND substr(lower(i.release_group_mbid), 1, 15) <> 'd0ab1e00-50c3-8'";

    /// Linked stand-ins with an open album request or a wanted watch.
    pub(super) LINKED_OPEN = "SELECT l.stand_in FROM source_links l \
         WHERE EXISTS (SELECT 1 FROM request_history r \
             WHERE r.musicbrainz_id_lower = l.stand_in AND r.request_kind = 'album' \
               AND r.status NOT IN ('imported', 'cancelled', 'rejected', 'cancelling')) \
            OR EXISTS (SELECT 1 FROM wanted_watches w \
             WHERE w.release_group_mbid_lower = l.stand_in AND w.state = 'watching') \
         ORDER BY l.stand_in";
}
