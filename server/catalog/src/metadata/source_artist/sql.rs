//! Fork: the statements of artist pages on stand-in ids.

droppedneedle_kernel::sql! {
    /// The provider's picture of stand-in artist `?1`, from a confirmed
    /// album match that credits it.
    pub(super) SOURCE_ARTIST_PICTURE = "SELECT m.artist_picture_url FROM source_identities s \
           JOIN library_album_provider_matches m \
             ON m.provider = s.provider AND m.provider_artist_id = s.provider_id \
          WHERE s.uuid = ?1 AND s.kind = 'artist' \
            AND m.match_state = 'confirmed' AND m.artist_picture_url IS NOT NULL \
          ORDER BY m.updated_at DESC LIMIT 1";
    /// The held albums whose confirmed match credits stand-in artist `?1`:
    /// release group, title, year, the provider's record type.
    pub(super) SOURCE_ARTIST_ALBUMS = "SELECT e.release_group_mbid, b.title, b.year, \
             m.record_type \
           FROM source_identities s \
           JOIN library_album_provider_matches m \
             ON m.provider = s.provider AND m.provider_artist_id = s.provider_id \
            AND m.match_state = 'confirmed' \
           JOIN local_albums b ON b.id = m.local_album_id \
           JOIN local_album_external_identities e \
             ON e.local_album_id = b.id AND e.provider = 'musicbrainz' \
          WHERE s.uuid = ?1 AND s.kind = 'artist' \
            AND b.retired_into_album_id IS NULL AND b.has_indexed_track = 1 \
          ORDER BY b.year IS NULL, b.year DESC, b.title_folded";
}
