//! Fork: stand-in albums linked to the MusicBrainz group they turned out
//! to be, and what is asked of them settled.
//!
//! A Deezer album on a stand-in group (library `sources::links`) may be a
//! record MusicBrainz knows after all: files fetched or held for it went
//! in under a MusicBrainz album, or a check before a request found it.
//! The stand-in is linked to that group; once the library holds the group,
//! the stand-in's request is imported and its wanted watch fulfilled, as
//! for the group itself.
//!
//! A held match links as it imports ([`link_after_match`]); the backfill
//! job ([`spawn_stand_in_links`]) repairs albums handled before links
//! existed and settles linked stand-ins whose group has since arrived.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::jobs::BoxFuture;

use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::jobs::{AlreadyRunning, FnTick, Next, Schedule, Supervisor};
use droppedneedle_library::identity::reads::group_owned;
use droppedneedle_library::sources::links::{self, LinkVia};
use droppedneedle_providers::virtual_ids::is_virtual;
use rusqlite::Connection;

use crate::db::AcquireDb;
use crate::flows::seams::TickSink;
use crate::requests::ledger::WatchOutcome;
use crate::requests::settle::{Fulfilled, announce_fulfilled, on_album_in_library};

/// The backfill's job name.
pub const LINK_JOB: &str = "stand-in-links";
/// How often the backfill runs after the first pass.
pub const LINK_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Link `stand_in` to `musicbrainz_id` (a stand-in keeps its first link),
/// then settle the stand-in's request and watch when the library holds the
/// group it is linked to. Answers the requests that reached the library,
/// for the caller to announce after the transaction commits.
///
/// # Errors
///
/// A store fault.
pub fn link_and_settle(
    conn: &Connection,
    stand_in: &str,
    musicbrainz_id: &str,
    via: LinkVia,
    now_ms: i64,
) -> rusqlite::Result<Vec<Fulfilled>> {
    let made = links::link(conn, stand_in, musicbrainz_id, via, now_ms)?;
    if made {
        tracing::info!(
            stand_in,
            musicbrainz_id,
            via = via.as_str(),
            "stand-in album linked to its MusicBrainz group"
        );
    }
    settle_if_held(conn, stand_in, now_ms)
}

/// Settle linked stand-in `stand_in` when the library holds its group.
fn settle_if_held(
    conn: &Connection,
    stand_in: &str,
    now_ms: i64,
) -> rusqlite::Result<Vec<Fulfilled>> {
    if links::linked(conn, stand_in)?.is_none() || !group_owned(conn, stand_in)? {
        return Ok(Vec::new());
    }
    on_album_in_library(conn, stand_in, WatchOutcome::InLibrary, now_ms)
}

/// The stand-ins among `asked` that a match placing files on the album
/// titled `placed_title` under group `placed` links: each stand-in asked
/// for an album of the same title. A stand-in group placed is no link.
pub fn stand_ins_to_link<'a>(
    asked: impl IntoIterator<Item = (&'a str, &'a str)>,
    placed: &str,
    placed_title: &str,
) -> Vec<String> {
    if placed.trim().is_empty() || is_virtual(placed) {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for (stand_in, title) in asked {
        let stand_in = stand_in.trim().to_ascii_lowercase();
        if is_virtual(&stand_in)
            && links::same_title(title, placed_title)
            && !out.contains(&stand_in)
        {
            out.push(stand_in);
        }
    }
    out
}

/// After a person's match placed files on group `placed` (titled
/// `placed_title`): link each stand-in in `asked` (`(group, title)`) of
/// the same title to it, and settle what was asked of it. Best effort:
/// the import already stuck.
pub async fn link_after_match(
    db: &AcquireDb,
    ticks: &dyn TickSink,
    asked: Vec<(String, String)>,
    placed: String,
    placed_title: String,
) {
    let stand_ins = stand_ins_to_link(
        asked
            .iter()
            .map(|(group, title)| (group.as_str(), title.as_str())),
        &placed,
        &placed_title,
    );
    if stand_ins.is_empty() {
        return;
    }
    let now = db.now_epoch();
    let now_ms = crate::db::to_i64(now).saturating_mul(1000);
    let group = placed.clone();
    let written = db
        .db()
        .write(Lane::Foreground, "stand_ins.link_after_match", move |tx| {
            let mut fulfilled = Vec::new();
            for stand_in in &stand_ins {
                fulfilled.extend(link_and_settle(
                    tx,
                    stand_in,
                    &group,
                    LinkVia::Files,
                    now_ms,
                )?);
            }
            Ok::<_, DbError>(fulfilled)
        })
        .await;
    match written {
        Ok(fulfilled) => announce_fulfilled(ticks, fulfilled, crate::db::to_i64(now)),
        Err(error) => {
            tracing::warn!(%placed, %error, "stand-in album link after a match failed");
        }
    }
}

/// Fork: what a request for a stand-in album checks first: the
/// MusicBrainz group the Deezer album is, by its barcode, then by a
/// confident artist and title match. `Ok(None)` when MusicBrainz has no
/// such album; an error when a service failed.
pub trait StandInCheck: Send + Sync {
    fn musicbrainz_of<'a>(
        &'a self,
        stand_in: &'a str,
    ) -> BoxFuture<'a, Result<Option<(String, LinkVia)>, String>>;
}

/// Fork: no MusicBrainz check: only a link already made redirects.
pub struct LinkedOnly;

impl StandInCheck for LinkedOnly {
    fn musicbrainz_of<'a>(
        &'a self,
        _stand_in: &'a str,
    ) -> BoxFuture<'a, Result<Option<(String, LinkVia)>, String>> {
        Box::pin(async { Ok(None) })
    }
}

/// How long a request waits on the MusicBrainz check of one stand-in.
pub const CHECK_DEADLINE: Duration = Duration::from_secs(15);

/// The group to request for album `mbid`: a stand-in's linked group, else
/// the group `check` finds (then linked), else `mbid` itself. A check that
/// fails or runs past [`CHECK_DEADLINE`] leaves the stand-in, logged.
pub async fn musicbrainz_for(db: &AcquireDb, check: &dyn StandInCheck, mbid: &str) -> String {
    if !is_virtual(mbid) {
        return mbid.to_owned();
    }
    let stand_in = mbid.trim().to_ascii_lowercase();
    let read = stand_in.clone();
    match db
        .db()
        .read("stand_ins.linked", move |conn| {
            links::linked(conn, &read).map_err(DbError::from)
        })
        .await
    {
        Ok(Some(group)) => return group,
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%stand_in, %error, "stand-in link unreadable before a request");
            return mbid.to_owned();
        }
    }
    let (group, via) =
        match tokio::time::timeout(CHECK_DEADLINE, check.musicbrainz_of(&stand_in)).await {
            Ok(Ok(Some(found))) => found,
            Ok(Ok(None)) => return mbid.to_owned(),
            Ok(Err(error)) => {
                tracing::info!(%stand_in, %error, "MusicBrainz check before a request failed");
                return mbid.to_owned();
            }
            Err(_) => {
                tracing::info!(%stand_in, "MusicBrainz check before a request ran out of time");
                return mbid.to_owned();
            }
        };
    let now_ms = crate::db::to_i64(db.now_epoch()).saturating_mul(1000);
    let (link_in, link_to) = (stand_in.clone(), group.clone());
    let linked = db
        .db()
        .write(
            Lane::Foreground,
            "stand_ins.link_before_request",
            move |tx| {
                links::link(tx, &link_in, &link_to, via, now_ms)?;
                links::linked(tx, &link_in).map_err(DbError::from)
            },
        )
        .await;
    match linked {
        Ok(Some(linked)) => {
            tracing::info!(%stand_in, musicbrainz_id = %linked, via = via.as_str(),
                "stand-in album requested as its MusicBrainz group");
            linked
        }
        Ok(None) => group,
        Err(error) => {
            tracing::warn!(%stand_in, %error, "stand-in link before a request failed");
            group
        }
    }
}

/// One backfill pass: make every link that is due, then settle linked
/// stand-ins whose group the library now holds. Answers the links made.
///
/// # Errors
///
/// A store fault.
pub async fn backfill_once(db: &Db, ticks: &dyn TickSink, now_secs: i64) -> Result<usize, DbError> {
    let repairs = db
        .read("stand_ins.repairs", |conn| {
            links::repairs(conn).map_err(DbError::from)
        })
        .await?;
    let made = repairs.len();
    let now_ms = now_secs.saturating_mul(1000);
    let fulfilled = db
        .write(Lane::Background, "stand_ins.backfill", move |tx| {
            let mut fulfilled = Vec::new();
            for repair in &repairs {
                fulfilled.extend(link_and_settle(
                    tx,
                    &repair.stand_in,
                    &repair.musicbrainz_id,
                    repair.via,
                    now_ms,
                )?);
            }
            for stand_in in links::linked_open(tx)? {
                fulfilled.extend(settle_if_held(tx, &stand_in, now_ms)?);
            }
            Ok::<_, DbError>(fulfilled)
        })
        .await?;
    announce_fulfilled(ticks, fulfilled, now_secs);
    Ok(made)
}

/// Start the backfill on `supervisor`: soon after boot, then hourly.
pub fn spawn_stand_in_links(
    supervisor: &Supervisor,
    db: AcquireDb,
    ticks: Arc<dyn TickSink>,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        LINK_JOB,
        Schedule::new(LINK_INTERVAL)
            .with_initial_delay(Duration::from_secs(120))
            .with_jitter(Duration::from_secs(60)),
        FnTick(move |_| {
            let (db, ticks) = (db.clone(), ticks.clone());
            async move {
                let now = crate::db::to_i64(db.now_epoch());
                match backfill_once(db.db(), ticks.as_ref(), now).await {
                    Ok(0) => {}
                    Ok(made) => tracing::info!(made, "stand-in album links repaired"),
                    Err(error) => tracing::warn!(%error, "stand-in album link backfill failed"),
                }
                Ok(Next::Continue)
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_providers::virtual_ids::{Kind, mint};

    const MB: &str = "ea8e626f-520f-4f54-9fab-8004e702afef";

    #[test]
    fn only_stand_ins_of_the_placed_title_link_to_a_real_group() {
        let typed = mint("deezer", Kind::ReleaseGroup, "1");
        let other = mint("deezer", Kind::ReleaseGroup, "2");
        let asked = [
            (typed.as_str(), "Typewritten (Deluxe)"),
            (other.as_str(), "Another Album"),
            (MB, "Typewritten"),
            (typed.as_str(), "Typewritten"),
        ];
        assert_eq!(
            stand_ins_to_link(asked, MB, "Typewritten"),
            vec![typed.clone()]
        );
        assert!(stand_ins_to_link(asked, &other, "Typewritten").is_empty());
        assert!(stand_ins_to_link(asked, "", "Typewritten").is_empty());
    }
}
