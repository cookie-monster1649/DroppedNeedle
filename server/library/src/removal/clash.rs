//! Fork: a same-release clash settled by the library itself.
//!
//! When two albums on one MusicBrainz release both hold a file for the
//! same release track, the worse file is set aside the way a duplicate
//! resolution sets one aside: it goes to the recycle bin (Put back can
//! return it) and its track leaves the catalog. See
//! [`crate::corrections::release_merge`].

use super::{Mutations, MutationError, Removal, RemovalEntry, Removed};

impl Mutations {
    /// Move the file of indexed track `track_id` to the recycle bin and
    /// take its track out of the catalog, recorded under `actor`.
    pub fn recycle_clash_copy(&self, track_id: &str, actor: &str) -> Result<Removed, MutationError> {
        let track = self.indexed_track(track_id, None)?;
        let album_id = track.album_id.clone();
        let removal = Removal {
            reason_code: "FILE_RECYCLED",
            actor,
            entry: RemovalEntry::Wait,
            expect_sha256: None,
            undo_album: Some(&album_id),
            album_removed: None,
            seen: None,
            guard: None,
        };
        let done = self.remove_tracks(&[track], true, &removal)?;
        Ok(Removed {
            id: track_id.to_owned(),
            track_ids: done.track_ids,
            release_group_mbid: None,
            recycled: done.recycled,
            removal_id: done.removal_id,
        })
    }
}
