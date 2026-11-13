//! Fork: albums that are plainly one album are merged without a person.
//!
//! Two live albums with the same grouping key (folded album artist and
//! title) whose tracks all sit in the same folder are one album split by
//! history (files moved together by hand, or filed before the want fix).
//! Only albums the scan grouped by itself are touched: anything a person
//! split, merged or locked is left alone. The merge runs through the same
//! preview and apply as the album page's "Merge with another local album",
//! so it carries every reference across and is recorded in the history.

use super::models::{
    ApplyMeta, CorrectionError, IdentityChoice, MembershipKind, MembershipRequest,
};
use super::service::Corrections;
use super::sql;
use rusqlite::OptionalExtension as _;
use std::collections::{BTreeMap, BTreeSet};

impl Corrections {
    /// Merge every set of same-folder albums; answers the albums folded.
    pub fn merge_same_folder_albums(&self) -> Result<usize, CorrectionError> {
        let (pairs, admin) = self.db().read_blocking(
            "corrections.same_folder_albums",
            |conn| -> Result<_, CorrectionError> {
                let mut statement = conn.prepare_cached(sql::SAME_FOLDER_ALBUMS.text())?;
                let pairs = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let admin: Option<String> = conn
                    .query_row(sql::FIRST_ADMIN.text(), [], |row| row.get(0))
                    .optional()?;
                Ok((pairs, admin))
            },
        )?;
        let Some(actor) = admin else {
            return Ok(0);
        };
        let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (keep, fold) in pairs {
            groups.entry(keep).or_default().insert(fold);
        }
        let mut folded = 0;
        for (keep, folds) in groups {
            if self.merge_albums_into(&keep, &folds, &actor, "auto-merge")? {
                tracing::info!(album = %keep, merged = folds.len(), "same-folder albums merged");
                folded += folds.len();
            }
        }
        Ok(folded)
    }

    /// Fold `folds` into `keep` through the album page's merge preview and
    /// apply, recorded under `actor`. Answers whether anything moved.
    pub(super) fn merge_albums_into(
        &self,
        keep: &str,
        folds: &BTreeSet<String>,
        actor: &str,
        key_prefix: &str,
    ) -> Result<bool, CorrectionError> {
        let track_ids = self.album_tracks(folds)?;
        if track_ids.is_empty() {
            return Ok(false);
        }
        let request = MembershipRequest {
            kind: MembershipKind::Merge,
            album_id: None,
            track_ids,
            expected_album_revisions: BTreeMap::new(),
            target_album_id: Some(keep.to_owned()),
            title: None,
            album_artist_name: None,
            identity_choice: IdentityChoice::RetainManual,
        };
        let previewed = self.preview_membership(&request, actor)?;
        let key = format!(
            "{key_prefix}:{keep}:{}",
            folds.iter().cloned().collect::<Vec<_>>().join(",")
        );
        self.apply_membership(
            &request,
            &ApplyMeta {
                preview_token: previewed.token,
                idempotency_key: Some(key),
                actor: actor.to_owned(),
            },
        )?;
        Ok(true)
    }

    /// The first administrator, whom automatic merges are recorded under.
    pub(super) fn merge_actor(&self) -> Result<Option<String>, CorrectionError> {
        self.db().read_blocking(
            "corrections.merge_actor",
            |conn| -> Result<_, CorrectionError> {
                Ok(conn
                    .query_row(sql::FIRST_ADMIN.text(), [], |row| row.get(0))
                    .optional()?)
            },
        )
    }

    /// The tracks of `albums` (a merge takes whole albums from any of them).
    fn album_tracks(&self, albums: &BTreeSet<String>) -> Result<Vec<String>, CorrectionError> {
        self.db()
            .read_blocking("corrections.same_folder_tracks", |conn| {
                let mut ids = Vec::new();
                let mut statement = conn.prepare_cached(sql::ALBUM_TRACK_IDS.text())?;
                for album in albums {
                    let rows = statement.query_map([album], |row| row.get::<_, String>(0))?;
                    for id in rows {
                        ids.push(id?);
                    }
                }
                Ok(ids)
            })
    }
}
