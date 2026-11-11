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
             OR (s.suggestion_state = 'open' AND s.checked_at < ?2)) \
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
          suggestion_state, checked_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'open', ?10, ?10) \
         ON CONFLICT (local_artist_id) DO UPDATE SET merge_artist_id = excluded.merge_artist_id, \
           musicbrainz_artist_id = excluded.musicbrainz_artist_id, \
           musicbrainz_name = excluded.musicbrainz_name, \
           musicbrainz_disambiguation = excluded.musicbrainz_disambiguation, \
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
         ORDER BY r.folded_name, r.id";
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
    pub(super) row ArtistDeezerRow for [ARTIST_DEEZER] {
        deezer_artist_id: String,
        link_url: Option<String>,
        fans: Option<i64>,
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
