//! Fork: the statements of the source-album leftover sweep.

droppedneedle_kernel::sql! {
    /// Album tasks on stand-in groups that settled partial or failed
    /// between `?1` and `?2` with no later task for the same group:
    /// id, owner, plan.
    pub(super) SETTLED_SOURCE_TASKS = "SELECT t.id, t.user_id, t.plan_json \
           FROM download_tasks t \
          WHERE t.download_type = 'album' AND t.status IN ('partial', 'failed') \
            AND t.release_group_mbid LIKE 'd0ab1e00-50c3-8%' \
            AND t.updated_at > ?1 AND t.updated_at <= ?2 \
            AND NOT EXISTS (SELECT 1 FROM download_tasks n \
                 WHERE n.release_group_mbid = t.release_group_mbid \
                   AND n.download_type = 'album' \
                   AND (n.created_at > t.created_at \
                        OR (n.created_at = t.created_at AND n.rowid > t.rowid)))";
    /// The confirmed provider album of library album `?1`: artist, title,
    /// tracks.
    pub(super) SOURCE_ALBUM = "SELECT artist_name, title, tracks_json \
           FROM library_album_provider_matches \
          WHERE local_album_id = ?1 AND match_state = 'confirmed'";
    /// The (disc, track) positions library album `?1` holds.
    pub(super) HELD_POSITIONS = "SELECT disc_number, track_number FROM local_tracks \
          WHERE local_album_id = ?1";
    /// Whether track request `?1` (stored key) exists.
    pub(super) TRACK_ASKED = "SELECT 1 FROM request_history WHERE musicbrainz_id_lower = ?1";
    /// The role and display name of user `?1`.
    pub(super) OWNER = "SELECT role, COALESCE(username_display, username) \
             FROM auth_users WHERE id = ?1";
}
