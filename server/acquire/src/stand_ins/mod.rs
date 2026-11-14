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
//! existed, checks open stand-ins at MusicBrainz, and settles what was
//! asked of every linked stand-in ([`settle_linked`]):
//!
//! - its group in the library: queued and retrying downloads stop as
//!   already in the library, its request and watch settle, and its held
//!   files are decided as already owned (the copies stay where they are);
//! - its group not in the library: queued downloads and the watch move to
//!   the group, and held files are pointed at it and matched by the held
//!   list itself when the match is clear.
//!
//! A retry of a download for a linked stand-in is never spawned for the
//! stand-in (see the download worker's retry pass).

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
use crate::held::store::{FileState, HeldRows};
use crate::reason::Reason;
use rusqlite::params;

pub(crate) mod sql;
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
    let mut fulfilled = settle_if_held(conn, stand_in, now_ms)?;
    if let Some(group) = links::linked(conn, stand_in)? {
        let stand_in = stand_in.trim().to_ascii_lowercase();
        let (more, _) = settle_linked(conn, &stand_in, &group, now_ms)?;
        fulfilled.extend(more);
    }
    Ok(fulfilled)
}

/// What [`settle_linked`] did for one stand-in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settled {
    /// Downloads stopped as already in the library.
    pub tasks_stopped: usize,
    /// Downloads pointed at the MusicBrainz group.
    pub tasks_moved: usize,
    /// Wanted watches moved to the group.
    pub watches_moved: usize,
    /// Held entries decided as already owned.
    pub held_owned: usize,
    /// Held entries pointed at the group for the held list's own match.
    pub held_moved: usize,
}

impl Settled {
    fn add(&mut self, other: Self) {
        self.tasks_stopped += other.tasks_stopped;
        self.tasks_moved += other.tasks_moved;
        self.watches_moved += other.watches_moved;
        self.held_owned += other.held_owned;
        self.held_moved += other.held_moved;
    }

    /// Whether anything moved.
    pub fn any(&self) -> bool {
        *self != Self::default()
    }
}

/// Settle what is still asked of stand-in `stand_in`, linked to
/// MusicBrainz group `group` (see the module notes). Held copies are never
/// removed: an owned entry's files are only decided. Answers the requests
/// that reached the library and what moved.
///
/// # Errors
///
/// A store fault.
pub fn settle_linked(
    conn: &Connection,
    stand_in: &str,
    group: &str,
    now_ms: i64,
) -> rusqlite::Result<(Vec<Fulfilled>, Settled)> {
    let mut done = Settled::default();
    let mut fulfilled = Vec::new();
    let entries: Vec<i64> = conn
        .prepare_cached(sql::HELD_ENTRIES.text())?
        .query_map([stand_in], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if group_owned(conn, group)? {
        done.tasks_stopped =
            conn.prepare_cached(sql::STOP_OWNED_TASKS.text())?
                .execute(params![
                    stand_in,
                    now_ms,
                    Reason::AlreadyInLibrary.code(),
                    "stopped: the library holds the MusicBrainz album this Deezer album is",
                ])?;
        fulfilled.extend(on_album_in_library(
            conn,
            stand_in,
            WatchOutcome::InLibrary,
            now_ms,
        )?);
        let rows = HeldRows::new(conn);
        for entry in entries {
            if !rows.claim(entry)? {
                continue;
            }
            let files: Vec<i64> = conn
                .prepare_cached(sql::HELD_FILE_IDS.text())?
                .query_map([entry], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for file in &files {
                rows.resolve_claimed(*file, FileState::AlreadyOwned, now_ms)?;
            }
            rows.release(entry, now_ms)?;
            done.held_owned += 1;
        }
    } else {
        done.tasks_moved = conn
            .prepare_cached(sql::REPOINT_TASKS.text())?
            .execute(params![stand_in, group, now_ms])?;
        let copied = conn
            .prepare_cached(sql::COPY_WATCH.text())?
            .execute(params![stand_in, group])?;
        let stopped = conn
            .prepare_cached(sql::STOP_STAND_IN_WATCH.text())?
            .execute(params![stand_in, group])?;
        done.watches_moved = copied.max(stopped);
        for entry in entries {
            if conn
                .prepare_cached(sql::REPOINT_ENTRY.text())?
                .execute(params![entry, group])?
                == 0
            {
                continue;
            }
            conn.prepare_cached(sql::RECHECK_FILES.text())?
                .execute([entry])?;
            done.held_moved += 1;
        }
    }
    if done.any() {
        tracing::info!(
            stand_in,
            musicbrainz_id = group,
            tasks_stopped = done.tasks_stopped,
            tasks_moved = done.tasks_moved,
            watches_moved = done.watches_moved,
            held_owned = done.held_owned,
            held_moved = done.held_moved,
            "linked stand-in album settled"
        );
    }
    Ok((fulfilled, done))
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
                let linked = links::linked(tx, &link_in)?;
                if let Some(group) = &linked {
                    // Fork: what is already asked of the stand-in follows
                    // the link at once (the request about to be made is
                    // made for the group).
                    settle_linked(tx, &link_in, group, now_ms)?;
                }
                Ok::<_, DbError>(linked)
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

/// How many open stand-ins one backfill pass checks at MusicBrainz.
const CHECKS_PER_PASS: usize = 25;
/// The pause between two of those checks, beside the providers' own
/// rate limits: the backfill never crowds out a person's search.
const CHECK_GAP: Duration = Duration::from_secs(2);
/// How long a stand-in MusicBrainz did not know waits before it is
/// checked again.
const MISS_RECHECK_SECS: i64 = 7 * 24 * 60 * 60;

/// Stand-ins MusicBrainz did not know, with when they were checked
/// (seconds), so the hourly pass does not ask again every hour.
pub type Misses = std::sync::Mutex<std::collections::HashMap<String, i64>>;

/// What one backfill pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pass {
    /// Links made from what the library and the downloads show.
    pub repaired: usize,
    /// Links made by the MusicBrainz check.
    pub checked: usize,
    /// What the links settled.
    pub settled: Settled,
}

/// One backfill pass: make every link that is due, check open stand-ins
/// nobody linked at MusicBrainz (barcode, then a confident artist, title
/// and track-count match; at most [`CHECKS_PER_PASS`], paced), then settle
/// what is asked of every linked stand-in ([`settle_linked`]).
///
/// # Errors
///
/// A store fault.
pub async fn backfill_once(
    db: &Db,
    ticks: &dyn TickSink,
    check: &dyn StandInCheck,
    misses: &Misses,
    now_secs: i64,
) -> Result<Pass, DbError> {
    let repairs = db
        .read("stand_ins.repairs", |conn| {
            links::repairs(conn).map_err(DbError::from)
        })
        .await?;
    let mut pass = Pass {
        repaired: repairs.len(),
        ..Pass::default()
    };
    let now_ms = now_secs.saturating_mul(1000);
    db.write(Lane::Background, "stand_ins.backfill", move |tx| {
        for repair in &repairs {
            links::link(
                tx,
                &repair.stand_in,
                &repair.musicbrainz_id,
                repair.via,
                now_ms,
            )?;
        }
        Ok::<_, DbError>(())
    })
    .await?;
    let open = db
        .read("stand_ins.open_unlinked", |conn| {
            links::open_unlinked(conn).map_err(DbError::from)
        })
        .await?;
    let due: Vec<String> = {
        let mut misses = misses.lock().unwrap_or_else(|p| p.into_inner());
        misses.retain(|_, at| now_secs.saturating_sub(*at) < MISS_RECHECK_SECS);
        open.into_iter()
            .filter(|stand_in| !misses.contains_key(stand_in))
            .take(CHECKS_PER_PASS)
            .collect()
    };
    let mut found: Vec<(String, String, LinkVia)> = Vec::new();
    for (index, stand_in) in due.iter().enumerate() {
        if index > 0 {
            tokio::time::sleep(CHECK_GAP).await;
        }
        match tokio::time::timeout(CHECK_DEADLINE, check.musicbrainz_of(stand_in)).await {
            Ok(Ok(Some((group, via)))) => found.push((stand_in.clone(), group, via)),
            Ok(Ok(None)) => {
                misses
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(stand_in.clone(), now_secs);
            }
            Ok(Err(error)) => {
                tracing::info!(%stand_in, %error, "MusicBrainz check of a stand-in album failed");
                break;
            }
            Err(_) => {
                tracing::info!(%stand_in, "MusicBrainz check of a stand-in album ran out of time");
                break;
            }
        }
    }
    let (fulfilled, checked, settled) = db
        .write(Lane::Background, "stand_ins.settle", move |tx| {
            let mut checked = 0;
            for (stand_in, group, via) in &found {
                if links::link(tx, stand_in, group, *via, now_ms)? {
                    checked += 1;
                }
            }
            let mut fulfilled = Vec::new();
            let mut settled = Settled::default();
            for (stand_in, group) in links::every_link(tx)? {
                let (more, done) = settle_linked(tx, &stand_in, &group, now_ms)?;
                fulfilled.extend(more);
                settled.add(done);
            }
            for stand_in in links::linked_open(tx)? {
                fulfilled.extend(settle_if_held(tx, &stand_in, now_ms)?);
            }
            Ok::<_, DbError>((fulfilled, checked, settled))
        })
        .await?;
    announce_fulfilled(ticks, fulfilled, now_secs);
    pass.checked = checked;
    pass.settled = settled;
    Ok(pass)
}

/// Start the backfill on `supervisor`: soon after boot, then hourly.
/// `check` is the MusicBrainz check open stand-ins get.
pub fn spawn_stand_in_links(
    supervisor: &Supervisor,
    db: AcquireDb,
    ticks: Arc<dyn TickSink>,
    check: Arc<dyn StandInCheck>,
) -> Result<(), AlreadyRunning> {
    let misses: Arc<Misses> = Arc::default();
    supervisor.periodic(
        LINK_JOB,
        Schedule::new(LINK_INTERVAL)
            .with_initial_delay(Duration::from_secs(120))
            .with_jitter(Duration::from_secs(60)),
        FnTick(move |_| {
            let (db, ticks, check, misses) =
                (db.clone(), ticks.clone(), check.clone(), misses.clone());
            async move {
                let now = crate::db::to_i64(db.now_epoch());
                match backfill_once(db.db(), ticks.as_ref(), check.as_ref(), &misses, now).await {
                    Ok(pass) if pass.repaired + pass.checked == 0 && !pass.settled.any() => {}
                    Ok(pass) => tracing::info!(
                        repaired = pass.repaired,
                        checked = pass.checked,
                        tasks_stopped = pass.settled.tasks_stopped,
                        tasks_moved = pass.settled.tasks_moved,
                        watches_moved = pass.settled.watches_moved,
                        held_owned = pass.settled.held_owned,
                        held_moved = pass.settled.held_moved,
                        "stand-in album links repaired and settled"
                    ),
                    Err(error) => tracing::warn!(%error, "stand-in album link backfill failed"),
                }
                Ok(Next::Continue)
            }
        }),
    )
}

#[cfg(test)]
mod tests;
