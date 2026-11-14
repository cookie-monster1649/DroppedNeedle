//! Fork: the statements that settle what was asked of a linked stand-in
//! album (`download_tasks`, `wanted_watches`, `acquire_held_entries`,
//! `acquire_held_files`).

droppedneedle_kernel::sql! {
    /// Stop the album downloads of stand-in `?1` that are queued or wait
    /// for a retry, at `?2` (ms), with outcome `?3` and log line `?4`: the
    /// library holds the group the stand-in is linked to. An upgrade is
    /// left to its own sweep.
    pub(super) STOP_OWNED_TASKS = "UPDATE download_tasks \
             SET status = 'cancelled', cancelled_at = ?2, remote_queued = 0, \
                 outcome_code = ?3, error_message = ?4, updated_at = ?2 \
             WHERE lower(release_group_mbid) = ?1 AND download_type = 'album' \
               AND origin != 'upgrade' AND status IN ('queued', 'failed', 'partial')";
    /// Point the queued album downloads of stand-in `?1` at MusicBrainz
    /// group `?2` at `?3` (ms): a stand-in edition pin and any search made
    /// for the stand-in are dropped, so the download searches again for
    /// the group. A person's own pick stays as it is. The request the task
    /// serves stays on the stand-in and settles through the link.
    pub(super) REPOINT_TASKS = "UPDATE download_tasks \
             SET release_group_mbid = ?2, \
                 release_mbid = CASE WHEN substr(lower(COALESCE(release_mbid, '')), 1, 15) \
                     = 'd0ab1e00-50c3-8' THEN NULL ELSE release_mbid END, \
                 search_job_id = NULL, candidate_index = NULL, updated_at = ?3 \
             WHERE lower(release_group_mbid) = ?1 AND download_type = 'album' \
               AND status = 'queued' AND picked_by_person = 0";
    /// A wanted watch on MusicBrainz group `?2` for whoever watches
    /// stand-in `?1`, unless the group is already watched.
    pub(super) COPY_WATCH = "INSERT INTO wanted_watches (release_group_mbid_lower, \
             release_group_mbid, user_id, artist_name, album_title, artist_mbid, year, cover_url, \
             kind, state, created_at, first_release_date, next_check_at) \
             SELECT ?2, ?2, user_id, artist_name, album_title, artist_mbid, year, cover_url, kind, \
                 'watching', created_at, first_release_date, next_check_at \
             FROM wanted_watches WHERE release_group_mbid_lower = ?1 AND state = 'watching' \
             ON CONFLICT DO NOTHING";
    /// Stop the watch on stand-in `?1` once group `?2` is watched: that
    /// watch carries it.
    pub(super) STOP_STAND_IN_WATCH = "UPDATE wanted_watches SET state = 'stopped' \
             WHERE release_group_mbid_lower = ?1 AND state = 'watching' \
               AND EXISTS (SELECT 1 FROM wanted_watches w \
                   WHERE w.release_group_mbid_lower = ?2 AND w.state = 'watching')";
    /// Open held album entries of stand-in `?1` waiting for a person.
    pub(super) HELD_ENTRIES = "SELECT id FROM acquire_held_entries \
             WHERE lower(release_group_mbid) = ?1 AND state = 'held' \
               AND COALESCE(recording_mbid, '') = '' ORDER BY id";
    /// The files entry `?1` still holds.
    pub(super) HELD_FILE_IDS = "SELECT id FROM acquire_held_files \
             WHERE entry_id = ?1 AND state = 'held' ORDER BY id";
    /// Point held entry `?1` at MusicBrainz group `?2`; its retry starts
    /// again from the first try.
    pub(super) REPOINT_ENTRY = "UPDATE acquire_held_entries \
             SET release_group_mbid = ?2, retry_tries = 0, retry_at = NULL \
             WHERE id = ?1 AND state = 'held'";
    /// The files of entry `?1` held because the stand-in had no
    /// MusicBrainz release (or did not pair with it) are held as a weak
    /// match instead, which the held list matches by itself when the
    /// match is clear.
    pub(super) RECHECK_FILES = "UPDATE acquire_held_files SET reason = 'weak_match' \
             WHERE entry_id = ?1 AND state = 'held' \
               AND reason IN ('no_release', 'tag_mismatch', 'album_not_found')";
}
