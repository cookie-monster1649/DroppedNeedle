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
//!
//! Two more passes run first. A ghost album, one on the release with no
//! indexed track whose files are all confirmed gone from disk (an old
//! folder moved or deleted by hand), is folded into the live album of the
//! release: its missing rows go with it, hidden as missing rows always are,
//! and never count as a clash. Then a true clash, two live albums each
//! holding a file for one release track, is settled by keeping the better
//! file ([`copy_rank`]) and sending the other to the recycle bin
//! ([`crate::removal::Mutations::recycle_clash_copy`]), after which the
//! albums merge as above.

use super::models::CorrectionError;
use super::service::Corrections;
use super::sql;
use crate::removal::Mutations;
use droppedneedle_kernel::runtime_config::sections::tier_rank;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// One file's quality as a sortable key: the landing's tier (format and
/// bit rate, as `acquire::landing::quality::tier_for` judges it), then bit
/// depth, sample rate and bit rate.
pub(crate) fn copy_rank(
    format: &str,
    bit_rate: Option<i64>,
    bit_depth: Option<i64>,
    sample_rate: Option<i64>,
) -> (usize, i64, i64, i64) {
    let format = format.to_ascii_lowercase();
    let lossless = ["flac", "wav", "ape", "wv"].contains(&format.as_str())
        || (["m4a", "mp4", "m4b"].contains(&format.as_str()) && bit_depth.is_some());
    let tier = if lossless {
        "lossless"
    } else {
        match bit_rate.unwrap_or(0) {
            rate if rate >= 320 => "mp3_320",
            rate if rate >= 256 => "mp3_256",
            rate if rate >= 192 => "mp3_192",
            _ => "low",
        }
    };
    (
        tier_rank(tier).unwrap_or(0),
        bit_depth.unwrap_or(0),
        sample_rate.unwrap_or(0),
        bit_rate.unwrap_or(0),
    )
}

type Clash = (String, (usize, i64, i64, i64), String, (usize, i64, i64, i64));

impl Corrections {
    /// Merge albums that share a release and do not clash; answers how many
    /// were folded.
    pub fn merge_same_release_albums(&self) -> Result<usize, CorrectionError> {
        let ghosts = self.fold_ghost_release_albums()?;
        Ok(ghosts + self.merge_live_release_albums()?)
    }

    fn same_release_pairs(&self) -> Result<Vec<(String, String)>, CorrectionError> {
        self.db().read_blocking(
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
        )
    }

    /// Fold every ghost album of a release (no indexed track, every file
    /// confirmed gone) into the release's live album; answers how many.
    pub fn fold_ghost_release_albums(&self) -> Result<usize, CorrectionError> {
        let pairs = self.db().read_blocking(
            "corrections.ghost_release_albums",
            |conn| -> Result<_, CorrectionError> {
                let mut statement = conn.prepare_cached(sql::GHOST_RELEASE_ALBUMS.text())?;
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
        let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (keep, ghost) in pairs {
            if self.files_present(&ghost)? {
                tracing::info!(album = %ghost, "ghost album still has a file on disk; left for the scan");
                continue;
            }
            groups.entry(keep).or_default().insert(ghost);
        }
        let mut folded = 0;
        for (keep, ghosts) in groups {
            if self.merge_albums_into(&keep, &ghosts, &actor, "release-ghost")? {
                tracing::info!(album = %keep, merged = ghosts.len(), "ghost albums on one release folded into the live copy");
                folded += ghosts.len();
            }
        }
        Ok(folded)
    }

    /// Whether any file an album's rows name is still on disk.
    fn files_present(&self, album: &str) -> Result<bool, CorrectionError> {
        let paths = self
            .db()
            .read_blocking("corrections.album_file_paths", |conn| -> Result<_, CorrectionError> {
                let mut statement = conn.prepare_cached(sql::ALBUM_FILE_PATHS.text())?;
                let rows = statement.query_map([album], |row| row.get::<_, String>(0))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })?;
        Ok(paths
            .iter()
            .any(|path| std::fs::symlink_metadata(path).is_ok()))
    }

    /// Settle true clashes between albums the release merge would fold:
    /// for each release track both hold a file for, the better file stays
    /// and the other goes to the recycle bin through `mutations`. Answers
    /// how many files were set aside.
    pub fn settle_release_clashes(&self, mutations: &Mutations) -> Result<usize, CorrectionError> {
        let pairs = self.same_release_pairs()?;
        if pairs.is_empty() {
            return Ok(0);
        }
        let Some(actor) = self.merge_actor()? else {
            return Ok(0);
        };
        let mut gone: HashSet<String> = HashSet::new();
        let mut set_aside = 0;
        for (keep, fold) in pairs {
            for (kept, kept_rank, other, other_rank) in self.release_clashes(&keep, &fold)? {
                if gone.contains(&kept) || gone.contains(&other) {
                    continue;
                }
                // Equal never replaces: the kept album's file stays.
                let loser = if other_rank > kept_rank { &kept } else { &other };
                match mutations.recycle_clash_copy(loser, &actor) {
                    Ok(_) => {
                        tracing::info!(album = %keep, other = %fold, track = %loser, "release clash settled; the worse copy went to the recycle bin");
                        gone.insert(loser.clone());
                        set_aside += 1;
                    }
                    Err(error) => {
                        tracing::warn!(album = %keep, other = %fold, track = %loser, %error, "release clash could not be settled; left for a person");
                    }
                }
            }
        }
        Ok(set_aside)
    }

    fn release_clashes(&self, keep: &str, fold: &str) -> Result<Vec<Clash>, CorrectionError> {
        self.db()
            .read_blocking("corrections.release_track_clashes", |conn| -> Result<_, CorrectionError> {
                let mut statement = conn.prepare_cached(sql::RELEASE_TRACK_CLASHES.text())?;
                let rows = statement.query_map([keep, fold], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        copy_rank(&row.get::<_, String>(1)?, row.get(2)?, row.get(3)?, row.get(4)?),
                        row.get::<_, String>(5)?,
                        copy_rank(&row.get::<_, String>(6)?, row.get(7)?, row.get(8)?, row.get(9)?),
                    ))
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
    }

    fn merge_live_release_albums(&self) -> Result<usize, CorrectionError> {
        let pairs = self.same_release_pairs()?;
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
