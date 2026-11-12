//! Fork: an album on stand-in ids moves to MusicBrainz once MusicBrainz
//! knows it.
//!
//! A few albums a tick, each at most once a month, are recalled and ranked
//! as identification would. When the lead candidate is a MusicBrainz
//! release that would seal on its own (`lead_identified`), the album is
//! handed back to automatic identification ("Let DroppedNeedle choose"),
//! which then takes the MusicBrainz match. Anything less keeps the
//! source's identity: a guess never replaces it.

use std::sync::Arc;

use rusqlite::params;

use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_providers::RequestPriority;

use super::deezer::stand_in;
use super::sql;
use crate::identify::service::IdentifyService;
use crate::identify::sqlite::SqliteIdentifyStore;
use crate::jobs::models::OperationError;

/// How long an album waits between checks.
pub const RECHECK_MS: i64 = 30 * 24 * 3600 * 1000;

/// Who hands an album back.
const ACTOR: &str = "system";

/// What a check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checked {
    /// MusicBrainz has a confident match: handed back.
    Promoted,
    /// Nothing confident: the source's identity stays.
    Kept,
    /// MusicBrainz could not answer: asked again next tick.
    Deferred,
}

impl Checked {
    fn outcome(self) -> super::CheckOutcome {
        match self {
            Self::Promoted => super::CheckOutcome::Promoted,
            Self::Kept | Self::Deferred => super::CheckOutcome::Kept,
        }
    }
}

/// The promotion checks.
#[derive(Clone)]
pub struct Promotion {
    db: Db,
    store: Arc<SqliteIdentifyStore>,
    identify: Arc<IdentifyService>,
}

impl Promotion {
    pub fn new(db: Db, store: Arc<SqliteIdentifyStore>, identify: Arc<IdentifyService>) -> Self {
        Self {
            db,
            store,
            identify,
        }
    }

    /// Check up to `batch` albums due at `now_ms`. Answers how many moved
    /// to MusicBrainz. Stops at the first MusicBrainz could not answer.
    ///
    /// # Errors
    ///
    /// A store fault.
    pub async fn check_some(&self, batch: u32, now_ms: i64) -> Result<usize, OperationError> {
        let due: Vec<String> = self
            .db
            .read("sources.promotion_due", move |conn| {
                let mut stmt = conn.prepare_cached(sql::PROMOTION_DUE.text())?;
                let rows = stmt
                    .query_map(params![now_ms - RECHECK_MS, batch], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok::<_, OperationError>(rows)
            })
            .await?;
        let mut promoted = 0;
        for album_id in due {
            let checked = self.check(&album_id).await?;
            if checked == Checked::Deferred {
                break;
            }
            let id = album_id.clone();
            self.db
                .write(Lane::Background, "sources.promotion_check", move |tx| {
                    if checked == Checked::Promoted {
                        crate::edition::choice::hand_back(tx, &id, ACTOR, now_ms as f64 / 1000.0)?;
                    }
                    tx.prepare_cached(sql::NOTE_CHECK.text())?.execute(params![
                        id,
                        now_ms,
                        checked.outcome()
                    ])?;
                    Ok::<_, OperationError>(())
                })
                .await?;
            if checked == Checked::Promoted {
                promoted += 1;
                tracing::info!(album = %album_id, "album moved from its Deezer identity to MusicBrainz");
            }
        }
        Ok(promoted)
    }

    /// Whether MusicBrainz has a confident match for album `album_id`.
    async fn check(&self, album_id: &str) -> Result<Checked, OperationError> {
        let Some(facts) = self
            .store
            .album_facts(album_id)?
            .filter(|facts| !facts.tracks.is_empty())
        else {
            return Ok(Checked::Kept);
        };
        let recall = self
            .identify
            .recall(
                &facts,
                None,
                Lane::Background,
                RequestPriority::BackgroundSync,
            )
            .await?;
        if recall.provider_deferred {
            return Ok(Checked::Deferred);
        }
        let Some(ranking) = self.identify.rank(&facts, &recall) else {
            return Ok(Checked::Deferred);
        };
        Ok(confident(&ranking))
    }
}

/// Promote only on a lead that would seal on its own and is a real
/// MusicBrainz release.
fn confident(ranking: &crate::identify::service::Ranking) -> Checked {
    let lead = ranking.candidates.first();
    match lead {
        Some(lead)
            if ranking.lead_identified
                && !stand_in(&lead.release_group_mbid)
                && lead.release_mbid.as_deref().is_none_or(|id| !stand_in(id)) =>
        {
            Checked::Promoted
        }
        _ => Checked::Kept,
    }
}
