//! Fork: the SQL of an artist's suggestions.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

droppedneedle_kernel::sql! {
    /// Live artists with no MusicBrainz link that lead or are credited on
    /// a listed album, never looked at or last looked at before `?2` with
    /// nothing decided: those with an accepted album match first, then the
    /// ones never looked at, at most `?1`.
    pub(super) PENDING_ARTISTS = "SELECT r.id AS artist_id, r.display_name AS name \
         FROM local_artists r \
         LEFT JOIN library_artist_suggestions s ON s.local_artist_id = r.id \
         WHERE r.retired_into_artist_id IS NULL AND r.kind != 'various_artists' \
           AND NOT EXISTS (SELECT 1 FROM local_artist_external_identities e \
             WHERE e.local_artist_id = r.id) \
           AND (EXISTS (SELECT 1 FROM local_albums a WHERE a.album_artist_id = r.id AND " + (crate::owned_album!("a")) + ") \
             OR EXISTS (SELECT 1 FROM local_album_artists laa \
               JOIN local_albums a ON a.id = laa.local_album_id \
               WHERE laa.local_artist_id = r.id AND " + (crate::owned_album!("a")) + ")) \
           AND (s.local_artist_id IS NULL \
             OR (s.suggestion_state = 'open' AND (s.checked_at < ?2 \
               OR (s.musicbrainz_artist_id IS NOT NULL AND s.musicbrainz_exact_count IS NULL)))) \
         ORDER BY NOT EXISTS (SELECT 1 FROM library_album_provider_matches m \
             JOIN local_albums a ON a.id = m.local_album_id \
             WHERE a.album_artist_id = r.id AND m.match_state = 'confirmed' \
             AND s.picture_url IS NULL), \
           s.local_artist_id IS NOT NULL, r.id LIMIT ?1";
    /// Every live artist, with its MusicBrainz link when it has one.
    pub(super) LIVE_ARTISTS = "SELECT r.id AS artist_id, r.display_name AS name, \
         e.provider_artist_id AS mbid \
         FROM local_artists r \
         LEFT JOIN local_artist_external_identities e \
           ON e.local_artist_id = r.id AND e.provider = 'musicbrainz' \
         WHERE r.retired_into_artist_id IS NULL AND r.kind != 'various_artists'";
    /// The Deezer artist of the accepted album matches of albums artist
    /// `?1` leads.
    pub(super) MATCHED_DEEZER_ARTISTS = "SELECT m.provider_artist_id AS deezer_artist_id, \
         m.artist_name AS name, m.artist_picture_url AS picture_url \
         FROM local_albums a \
         JOIN library_album_provider_matches m ON m.local_album_id = a.id \
         WHERE a.album_artist_id = ?1 AND m.match_state = 'confirmed' \
           AND m.provider_artist_id IS NOT NULL \
         ORDER BY m.updated_at DESC";
    /// Keep artist `?1`'s suggestions, as of `?10`, unless a person
    /// turned them down.
    pub(super) PUT_SUGGESTION = "INSERT INTO library_artist_suggestions \
         (local_artist_id, merge_artist_id, musicbrainz_artist_id, musicbrainz_name, \
          musicbrainz_disambiguation, deezer_artist_id, picture_url, link_url, fans, \
          suggestion_state, checked_at, updated_at, musicbrainz_exact_count) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'open', ?10, ?10, ?11) \
         ON CONFLICT (local_artist_id) DO UPDATE SET \
           merge_artist_id = COALESCE(excluded.merge_artist_id, library_artist_suggestions.merge_artist_id), \
           musicbrainz_artist_id = excluded.musicbrainz_artist_id, \
           musicbrainz_name = excluded.musicbrainz_name, \
           musicbrainz_disambiguation = excluded.musicbrainz_disambiguation, \
           musicbrainz_exact_count = excluded.musicbrainz_exact_count, \
           deezer_artist_id = COALESCE(excluded.deezer_artist_id, library_artist_suggestions.deezer_artist_id), \
           picture_url = COALESCE(excluded.picture_url, library_artist_suggestions.picture_url), \
           link_url = COALESCE(excluded.link_url, library_artist_suggestions.link_url), \
           fans = COALESCE(excluded.fans, library_artist_suggestions.fans), \
           checked_at = excluded.checked_at, updated_at = excluded.updated_at \
         WHERE library_artist_suggestions.suggestion_state = 'open'";
    /// The open suggestions to show: artists still live and unlinked, with
    /// a MusicBrainz artist or a live artist to merge into.
    pub(super) OPEN_SUGGESTIONS = "SELECT r.id AS artist_id, r.display_name AS name, \
         r.row_revision AS artist_revision, \
         (SELECT COUNT(*) FROM local_albums a WHERE a.album_artist_id = r.id \
           AND " + (crate::owned_album!("a")) + ") AS album_count, \
         (SELECT COUNT(*) FROM local_track_artists t JOIN local_tracks lt ON lt.id = t.local_track_id \
           WHERE t.local_artist_id = r.id AND lt.availability = 'indexed') AS track_count, \
         s.musicbrainz_artist_id AS musicbrainz_artist_id, s.musicbrainz_name AS musicbrainz_name, \
         s.musicbrainz_disambiguation AS musicbrainz_disambiguation, \
         m.id AS merge_artist_id, m.display_name AS merge_name, \
         m.row_revision AS merge_revision, me.provider_artist_id AS merge_mbid, \
         s.deezer_artist_id AS deezer_artist_id, s.link_url AS link_url, s.fans AS fans \
         FROM library_artist_suggestions s \
         JOIN local_artists r ON r.id = s.local_artist_id \
         LEFT JOIN local_artists m ON m.id = s.merge_artist_id \
           AND m.retired_into_artist_id IS NULL AND m.id != r.id \
         LEFT JOIN local_artist_external_identities me \
           ON me.local_artist_id = m.id AND me.provider = 'musicbrainz' \
         WHERE s.suggestion_state = 'open' AND r.retired_into_artist_id IS NULL \
           AND NOT EXISTS (SELECT 1 FROM local_artist_external_identities e \
             WHERE e.local_artist_id = r.id) \
           AND (m.id IS NOT NULL OR s.musicbrainz_artist_id IS NOT NULL) \
         ORDER BY album_count DESC, track_count DESC, r.folded_name, r.id";
    /// Fork: live artists with no MusicBrainz link that lead an album
    /// with an accepted Deezer match, whose Deezer artists were never
    /// asked of MusicBrainz, last asked before `?2`, or not all asked
    /// (a newer match names another), the never-asked first, at most `?1`.
    pub(super) DEEZER_LINK_PENDING = "SELECT r.id AS artist_id FROM local_artists r \
         LEFT JOIN library_artist_suggestions s ON s.local_artist_id = r.id \
         WHERE r.retired_into_artist_id IS NULL AND r.kind != 'various_artists' \
           AND NOT EXISTS (SELECT 1 FROM local_artist_external_identities e \
             WHERE e.local_artist_id = r.id) \
           AND EXISTS (SELECT 1 FROM local_albums a \
             JOIN library_album_provider_matches m ON m.local_album_id = a.id \
             WHERE a.album_artist_id = r.id AND m.provider = 'deezer' \
               AND m.match_state = 'confirmed' AND COALESCE(m.provider_artist_id, '') != '' \
               AND (s.deezer_link_checked_at IS NULL OR s.deezer_link_checked_at < ?2 \
                 OR instr(',' || COALESCE(s.deezer_link_ids, '') || ',', \
                   ',' || m.provider_artist_id || ',') = 0)) \
         ORDER BY s.deezer_link_checked_at IS NOT NULL, r.id LIMIT ?1";
    /// Fork: artist `?1`'s Deezer artists `?2` (comma-joined) were asked
    /// of MusicBrainz at `?3`. A new row waits for the suggestion loop.
    pub(super) MARK_DEEZER_LINK = "INSERT INTO library_artist_suggestions \
         (local_artist_id, suggestion_state, checked_at, updated_at, deezer_link_ids, \
          deezer_link_checked_at) VALUES (?1, 'open', 0, ?3, ?2, ?3) \
         ON CONFLICT (local_artist_id) DO UPDATE SET deezer_link_ids = excluded.deezer_link_ids, \
           deezer_link_checked_at = excluded.deezer_link_checked_at";
    /// Fork: the MusicBrainz artist a person turned down for artist `?1`,
    /// when they did.
    pub(super) DECLINED_MUSICBRAINZ = "SELECT musicbrainz_artist_id FROM library_artist_suggestions \
         WHERE local_artist_id = ?1 AND suggestion_state = 'declined'";
    /// Fork: artist `?1`'s open suggestion is a merge into artist `?2`
    /// (MusicBrainz artist `?3`), as of `?4`.
    pub(super) SUGGEST_DEEZER_MERGE = "UPDATE library_artist_suggestions \
         SET merge_artist_id = ?2, musicbrainz_artist_id = ?3, updated_at = ?4 \
         WHERE local_artist_id = ?1 AND suggestion_state = 'open'";
    /// Fork: the pair `?2`/`?3` (lower id first) offered as one artist,
    /// id `?1`, at `?4`, the way two records holding one id are.
    pub(super) OFFER_MERGE = "INSERT OR IGNORE INTO local_artist_merge_candidates (id, left_artist_id, \
         right_artist_id, reason_code, state, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'SHARED_PROVIDER_IDENTITY', 'open', ?4, ?4)";
    /// A person turns artist `?1`'s suggestions down: user `?2`, at `?3`.
    pub(super) DECLINE = "UPDATE library_artist_suggestions \
         SET suggestion_state = 'declined', decided_by_user_id = ?2, updated_at = ?3 \
         WHERE local_artist_id = ?1 AND suggestion_state = 'open'";
    /// Artist `?1`'s Deezer artist, when one was found.
    pub(super) ARTIST_DEEZER = "SELECT deezer_artist_id, link_url, fans \
         FROM library_artist_suggestions \
         WHERE local_artist_id = ?1 AND deezer_artist_id IS NOT NULL";
    /// The genres of the accepted album matches of the listed albums
    /// artist `?1` leads, one list per album.
    pub(super) ARTIST_MATCH_GENRES = "SELECT m.genres_json AS genres_json \
         FROM local_albums a \
         JOIN library_album_provider_matches m ON m.local_album_id = a.id \
         WHERE a.album_artist_id = ?1 AND m.match_state = 'confirmed' AND " + (crate::owned_album!("a"));
    /// Fork: whether a live artist with a MusicBrainz link has folded
    /// name `?1`: a known artist, whose name is never split.
    pub(crate) KNOWN_FOLDED = "SELECT EXISTS (SELECT 1 FROM local_artists r \
         JOIN local_artist_external_identities e ON e.local_artist_id = r.id \
         WHERE r.folded_name = ?1 AND r.retired_into_artist_id IS NULL)";
    /// Fork: live unlinked artists nobody turned down, credited on an album
    /// or track matched to a MusicBrainz release, whose matches were never
    /// read or last read before `?2`, at most `?1`.
    pub(super) EVIDENCE_PENDING = "SELECT r.id AS artist_id, r.display_name AS name FROM local_artists r \
         LEFT JOIN library_artist_suggestions s ON s.local_artist_id = r.id \
         WHERE r.retired_into_artist_id IS NULL AND r.kind != 'various_artists' \
           AND r.id NOT IN ('00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002') \
           AND NOT EXISTS (SELECT 1 FROM local_artist_external_identities e \
             WHERE e.local_artist_id = r.id) \
           AND COALESCE(s.suggestion_state, 'open') = 'open' \
           AND (s.album_evidence_checked_at IS NULL OR s.album_evidence_checked_at < ?2) \
           AND (EXISTS (SELECT 1 FROM local_albums a JOIN local_album_external_identities x \
               ON x.local_album_id = a.id WHERE a.album_artist_id = r.id AND x.release_mbid IS NOT NULL) \
             OR EXISTS (SELECT 1 FROM local_album_artists laa JOIN local_album_external_identities x \
               ON x.local_album_id = laa.local_album_id WHERE laa.local_artist_id = r.id \
               AND x.release_mbid IS NOT NULL) \
             OR EXISTS (SELECT 1 FROM local_track_artists t JOIN local_track_external_identities ti \
               ON ti.local_track_id = t.local_track_id WHERE t.local_artist_id = r.id \
               AND ti.release_track_mbid IS NOT NULL)) \
         ORDER BY s.album_evidence_checked_at IS NOT NULL, r.id LIMIT ?1";
    /// Fork: the live albums artist `?1` leads or is credited on, with the
    /// MusicBrainz release each is matched to.
    pub(super) ALBUM_RELEASES = "SELECT a.id AS album_id, x.release_mbid AS release_mbid \
         FROM local_albums a JOIN local_album_external_identities x ON x.local_album_id = a.id \
         WHERE a.retired_into_album_id IS NULL AND x.release_mbid IS NOT NULL \
           AND (a.album_artist_id = ?1 OR EXISTS (SELECT 1 FROM local_album_artists laa \
             WHERE laa.local_album_id = a.id AND laa.local_artist_id = ?1))";
    /// Fork: the indexed tracks artist `?1` is credited on, with the
    /// release track each is matched to.
    pub(super) TRACK_RELEASES = "SELECT lt.local_album_id AS album_id, ti.release_mbid AS release_mbid, \
         ti.release_track_mbid AS release_track_mbid \
         FROM local_track_artists t CROSS JOIN local_tracks lt ON lt.id = t.local_track_id \
         CROSS JOIN local_track_external_identities ti ON ti.local_track_id = lt.id \
         WHERE t.local_artist_id = ?1 AND lt.availability = 'indexed' \
           AND ti.release_mbid IS NOT NULL AND ti.release_track_mbid IS NOT NULL";
    /// Fork: artist `?1`'s matched albums were read at `?2`.
    pub(super) MARK_EVIDENCE = "INSERT INTO library_artist_suggestions \
         (local_artist_id, suggestion_state, checked_at, updated_at, album_evidence_checked_at) \
         VALUES (?1, 'open', 0, ?2, ?2) \
         ON CONFLICT (local_artist_id) DO UPDATE SET \
           album_evidence_checked_at = excluded.album_evidence_checked_at";
    /// Fork: live unlinked artists whose name may join several artists,
    /// never tried or last tried before `?2`, at most `?1`.
    pub(super) SPLIT_PENDING = "SELECT r.id AS artist_id, r.display_name AS name FROM local_artists r \
         LEFT JOIN library_artist_suggestions s ON s.local_artist_id = r.id \
         WHERE r.retired_into_artist_id IS NULL AND r.kind != 'various_artists' \
           AND r.id NOT IN ('00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002') \
           AND NOT EXISTS (SELECT 1 FROM local_artist_external_identities e \
             WHERE e.local_artist_id = r.id) \
           AND COALESCE(s.suggestion_state, 'open') = 'open' \
           AND (instr(r.display_name, '/') > 0 OR instr(r.display_name, ';') > 0 \
             OR instr(r.display_name, ', ') > 0 OR r.display_name LIKE '% & %' \
             OR r.display_name LIKE '% x %' OR r.display_name LIKE '% vs%' \
             OR r.display_name LIKE '% feat%' OR r.display_name LIKE '% ft%') \
           AND (s.split_checked_at IS NULL OR s.split_checked_at < ?2) \
         ORDER BY s.split_checked_at IS NOT NULL, r.id LIMIT ?1";
    /// Fork: artist `?1`'s name was tried as a joined credit at `?2`.
    pub(super) MARK_SPLIT = "INSERT INTO library_artist_suggestions \
         (local_artist_id, suggestion_state, checked_at, updated_at, split_checked_at) \
         VALUES (?1, 'open', 0, ?2, ?2) \
         ON CONFLICT (local_artist_id) DO UPDATE SET split_checked_at = excluded.split_checked_at";
    /// Fork: the tracks crediting artist `?1`.
    pub(super) TRACKS_CREDITING = "SELECT DISTINCT local_track_id FROM local_track_artists \
         WHERE local_artist_id = ?1";
    /// Fork: the albums crediting or led by artist `?1`.
    pub(super) ALBUMS_CREDITING = "SELECT local_album_id FROM local_album_artists \
         WHERE local_artist_id = ?1 UNION SELECT id FROM local_albums WHERE album_artist_id = ?1";
    /// Fork: the albums of the tracks crediting artist `?1`.
    pub(super) TRACK_ALBUMS_CREDITING = "SELECT DISTINCT lt.local_album_id FROM local_track_artists t \
         JOIN local_tracks lt ON lt.id = t.local_track_id WHERE t.local_artist_id = ?1";
    /// Fork: track `?1`'s credits in order.
    pub(super) TRACK_CREDITS = "SELECT position, local_artist_id, role, credited_name, join_phrase, \
         artist_mbid FROM local_track_artists WHERE local_track_id = ?1 ORDER BY position";
    pub(super) DELETE_TRACK_CREDITS = "DELETE FROM local_track_artists WHERE local_track_id = ?1";
    pub(super) INSERT_TRACK_CREDIT = "INSERT INTO local_track_artists (local_track_id, position, \
         local_artist_id, role, credited_name, join_phrase, artist_mbid) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
    /// Fork: album `?1`'s credits in order.
    pub(super) ALBUM_CREDITS = "SELECT position, local_artist_id, role, credited_name, join_phrase, \
         artist_mbid FROM local_album_artists WHERE local_album_id = ?1 ORDER BY position";
    pub(super) DELETE_ALBUM_CREDITS = "DELETE FROM local_album_artists WHERE local_album_id = ?1";
    pub(super) INSERT_ALBUM_CREDIT = "INSERT INTO local_album_artists (local_album_id, position, \
         local_artist_id, role, credited_name, join_phrase, artist_mbid) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
    /// Fork: the albums artist `?1` leads are led by `?2` from `?3` (ms).
    pub(super) REPOINT_LEAD = "UPDATE local_albums SET album_artist_id = ?2, updated_at = ?3, \
         row_revision = row_revision + 1 WHERE album_artist_id = ?1";
    /// Fork: the open best guesses a bulk accept may take: an unlinked live
    /// artist, no merge suggested, a MusicBrainz artist that is the only
    /// one of its name.
    pub(super) EXACT_GUESSES = "SELECT r.id AS artist_id, r.display_name AS name, \
         s.musicbrainz_artist_id AS musicbrainz_artist_id, \
         COALESCE(s.musicbrainz_name, '') AS musicbrainz_name \
         FROM library_artist_suggestions s JOIN local_artists r ON r.id = s.local_artist_id \
         WHERE s.suggestion_state = 'open' AND s.musicbrainz_artist_id IS NOT NULL \
           AND s.musicbrainz_exact_count = 1 AND r.retired_into_artist_id IS NULL \
           AND NOT EXISTS (SELECT 1 FROM local_artist_external_identities e \
             WHERE e.local_artist_id = r.id) \
           AND NOT EXISTS (SELECT 1 FROM local_artists m WHERE m.id = s.merge_artist_id \
             AND m.retired_into_artist_id IS NULL AND m.id != r.id) \
         ORDER BY r.id";
    pub(super) row NamedArtistRow for [EVIDENCE_PENDING, SPLIT_PENDING] {
        artist_id: String,
        name: String,
    }
    pub(super) row AlbumReleaseRow for [ALBUM_RELEASES] {
        album_id: String,
        release_mbid: String,
    }
    pub(super) row TrackReleaseRow for [TRACK_RELEASES] {
        album_id: String,
        release_mbid: String,
        release_track_mbid: String,
    }
    pub(super) row ExactGuessRow for [EXACT_GUESSES] {
        artist_id: String,
        name: String,
        musicbrainz_artist_id: String,
        musicbrainz_name: String,
    }
    pub(super) row ArtistDeezerRow for [ARTIST_DEEZER] {
        deezer_artist_id: String,
        link_url: Option<String>,
        fans: Option<i64>,
    }
    pub(super) row DeezerLinkPendingRow for [DEEZER_LINK_PENDING] {
        artist_id: String,
    }
    pub(super) row PendingArtistRow for [PENDING_ARTISTS] {
        artist_id: String,
        name: String,
    }
    pub(super) row LiveArtistRow for [LIVE_ARTISTS] {
        artist_id: String,
        name: String,
        mbid: Option<String>,
    }
    pub(super) row MatchedDeezerRow for [MATCHED_DEEZER_ARTISTS] {
        deezer_artist_id: String,
        name: String,
        picture_url: Option<String>,
    }
    pub(super) row OpenSuggestionRow for [OPEN_SUGGESTIONS] {
        artist_id: String,
        name: String,
        artist_revision: i64,
        album_count: i64,
        track_count: i64,
        musicbrainz_artist_id: Option<String>,
        musicbrainz_name: Option<String>,
        musicbrainz_disambiguation: Option<String>,
        merge_artist_id: Option<String>,
        merge_name: Option<String>,
        merge_revision: Option<i64>,
        merge_mbid: Option<String>,
        deezer_artist_id: Option<String>,
        link_url: Option<String>,
        fans: Option<i64>,
    }
}
