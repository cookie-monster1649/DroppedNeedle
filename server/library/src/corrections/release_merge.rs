//! Fork: albums on the same MusicBrainz release are one album.
//!
//! The album page offers a "Copies in your library" picker when a release
//! maps to more than one local album. That split comes from history: the
//! same release filed under two folders (an upgrade or a second source
//! landing beside the first copy, a hand move), or tags that fold to two
//! grouping keys ("Perception EP" and "Perception"), so the same-folder
//! pass ([`super::auto_merge`]) never sees them as one.
//!
//! Here every live album matched to one release on one root is folded into
//! the oldest of them, through the album page's merge, when their track
//! sets do not clash: no two of them hold a file for the same release
//! track. An album a person grouped by hand is never folded, and albums
//! that do clash keep the picker, which stays as the fallback for real
//! conflicts.

use super::models::CorrectionError;
use super::service::Corrections;
use super::sql;
use std::collections::{BTreeMap, BTreeSet, HashSet};

impl Corrections {
    /// Merge albums that share a release and do not clash; answers how many
    /// were folded.
    pub fn merge_same_release_albums(&self) -> Result<usize, CorrectionError> {
        let pairs = self.db().read_blocking(
            "corrections.same_release_albums",
            |conn| -> Result<_, CorrectionError> {
                let mut statement = conn.prepare_cached(sql::SAME_RELEASE_ALBUMS.text())?;
                let pairs = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(pairs)
            },
        )?;
        if pairs.is_empty() {
            return Ok(0);
        }
        let Some(actor) = self.merge_actor()? else {
            return Ok(0);
        };
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (keep, fold) in pairs {
            groups.entry(keep).or_default().push(fold);
        }
        let mut folded = 0;
        for (keep, candidates) in groups {
            let mut held = self.release_tracks(&keep)?;
            let mut folds = BTreeSet::new();
            for fold in candidates {
                let tracks = self.release_tracks(&fold)?;
                if tracks.is_disjoint(&held) {
                    held.extend(tracks);
                    folds.insert(fold);
                } else {
                    tracing::info!(album = %keep, other = %fold, "albums on one release hold the same track; left for a person");
                }
            }
            if folds.is_empty() {
                continue;
            }
            if self.merge_albums_into(&keep, &folds, &actor, "release-merge")? {
                tracing::info!(album = %keep, merged = folds.len(), "albums on one release merged");
                folded += folds.len();
            }
        }
        Ok(folded)
    }

    fn release_tracks(&self, album: &str) -> Result<HashSet<String>, CorrectionError> {
        self.db()
            .read_blocking("corrections.album_release_tracks", |conn| {
                let mut statement = conn.prepare_cached(sql::ALBUM_RELEASE_TRACKS.text())?;
                let rows = statement.query_map([album], |row| row.get::<_, String>(0))?;
                let mut set = HashSet::new();
                for row in rows {
                    set.insert(row?);
                }
                Ok(set)
            })
    }
}
