//! Fork: the statements of the source cover fallback.

droppedneedle_kernel::sql! {
    /// The provider's cover URL for stand-in release group or release
    /// `?1`, from the confirmed album match it names.
    pub(super) SOURCE_COVER_URL = "SELECT m.cover_url FROM source_identities s \
           JOIN library_album_provider_matches m \
             ON m.provider = s.provider \
            AND m.provider_album_id = COALESCE(s.provider_album_id, s.provider_id) \
          WHERE s.uuid = ?1 AND s.kind IN ('release_group', 'release') \
            AND m.match_state = 'confirmed' AND m.cover_url IS NOT NULL \
          LIMIT 1";
}
