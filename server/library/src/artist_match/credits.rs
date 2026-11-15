//! Fork: settle unlinked artists from what the library already holds.
//!
//! Two passes, both store-only (no provider is asked), run by the
//! `artist-credit-splits` backfill a batch at a time, so a pass stopped
//! midway goes on where it was. Each artist's turn is one transaction and
//! is recorded on its suggestion row, so a turn is never repeated until
//! [`EVIDENCE_RECHECK_MS`] or [`SPLIT_RECHECK_MS`] has passed.
//!
//! - **Album evidence.** An unlinked artist credited on albums or tracks
//!   matched to a MusicBrainz release, whose release credits name one
//!   MusicBrainz artist under the artist's name (as credited), is linked
//!   to it. Albums naming two different artists of that name link
//!   nothing. MusicBrainz's special-purpose artists (Various Artists and
//!   the like) never count. A person's link, or a suggestion a person
//!   turned down, is never touched.
//! - **Joined credits.** An unlinked artist whose name joins several
//!   artists ([`super::split`]) has its album and track credits re-credited
//!   to them (the first leads, the rest are additional credits) and is
//!   retired into the first through the artist merge, which moves anything
//!   else that pointed at it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{Connection, Transaction, params};

use droppedneedle_kernel::db::{Db, DbError, Lane, fold_text};

use super::split::split_credit;
use super::sql;
use crate::identify::artist_links::{
    ArtistLinkError, LINK_FROM_ALBUMS, ProviderLinkOutcome, mark_stale, placeholder,
    set_from_provider_link,
};
use crate::matching::model::Release;

/// The backfill's job name.
pub const SPLIT_JOB: &str = "artist-credit-splits";
/// How long an artist whose albums settled nothing waits to be read again.
pub const EVIDENCE_RECHECK_MS: i64 = 24 * 3600 * 1000;
/// How long a name left whole waits to be tried again.
pub const SPLIT_RECHECK_MS: i64 = 7 * 24 * 3600 * 1000;

/// A name as the passes compare it: folded, letters and digits only.
pub fn name_key(name: &str) -> String {
    fold_text(name)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Whether a live artist with a MusicBrainz link has folded name `folded`.
pub(crate) fn known_folded(conn: &Connection, folded: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(sql::KNOWN_FOLDED.text())?
        .query_row(params![folded], |row| row.get(0))
}

/// What album evidence says about one artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    /// No matched album credits a MusicBrainz artist under its name.
    Nothing,
    /// One MusicBrainz artist, named by this many albums.
    One { mbid: String, albums: usize },
    /// Albums name different MusicBrainz artists of the name.
    Disagree,
}

/// The verdict of `credits`: per album, the MusicBrainz artists its
/// release credits name (id, name as credited).
pub fn judge(name: &str, credits: &BTreeMap<String, Vec<(String, String)>>) -> Evidence {
    let key = name_key(name);
    if key.is_empty() {
        return Evidence::Nothing;
    }
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut albums = 0;
    for credited in credits.values() {
        let found: BTreeSet<String> = credited
            .iter()
            .filter(|(id, credited_as)| {
                let id = id.to_ascii_lowercase();
                id.len() == 36
                    && uuid::Uuid::parse_str(&id).is_ok()
                    && !placeholder(&id)
                    && name_key(credited_as) == key
            })
            .map(|(id, _)| id.to_ascii_lowercase())
            .collect();
        if !found.is_empty() {
            albums += 1;
            ids.extend(found);
        }
    }
    match ids.len() {
        0 => Evidence::Nothing,
        1 => Evidence::One {
            mbid: ids.into_iter().next().unwrap_or_default(),
            albums,
        },
        _ => Evidence::Disagree,
    }
}

/// The release credits of artist `artist`'s matched albums and tracks.
fn credits_of(
    conn: &Connection,
    artist: &str,
    releases: &mut HashMap<String, Option<Release>>,
) -> rusqlite::Result<BTreeMap<String, Vec<(String, String)>>> {
    let mut release = |mbid: &str| -> rusqlite::Result<Option<Release>> {
        let key = mbid.trim().to_ascii_lowercase();
        if let Some(found) = releases.get(&key) {
            return Ok(found.clone());
        }
        let found = crate::identity::reads::stored_release(conn, &key, 0.0)?;
        releases.insert(key, found.clone());
        Ok(found)
    };
    let mut out: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let albums = conn
        .prepare_cached(sql::ALBUM_RELEASES.text())?
        .query_map(params![artist], sql::AlbumReleaseRow::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for row in albums {
        if let Some(found) = release(&row.release_mbid)? {
            out.entry(row.album_id).or_default().extend(
                found
                    .artists
                    .iter()
                    .map(|credit| (credit.id.clone(), credit.name.clone())),
            );
        }
    }
    let tracks = conn
        .prepare_cached(sql::TRACK_RELEASES.text())?
        .query_map(params![artist], sql::TrackReleaseRow::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for row in tracks {
        let Some(found) = release(&row.release_mbid)? else {
            continue;
        };
        if let Some(track) = found
            .tracks
            .iter()
            .find(|track| track.id.eq_ignore_ascii_case(&row.release_track_mbid))
        {
            out.entry(row.album_id).or_default().extend(
                track
                    .artists
                    .iter()
                    .map(|credit| (credit.id.clone(), credit.name.clone())),
            );
        }
    }
    Ok(out)
}

/// Offer artist `artist` and the records already holding `mbid` as one
/// artist, the way two records holding one id are.
pub(super) fn offer_shared(
    tx: &Transaction<'_>,
    artist: &str,
    others: &[(String, String)],
    mbid: &str,
    now: i64,
) -> rusqlite::Result<()> {
    for (other, _) in others {
        let (left, right) = if artist < other.as_str() {
            (artist, other.as_str())
        } else {
            (other.as_str(), artist)
        };
        tx.prepare_cached(sql::OFFER_MERGE.text())?
            .execute(params![
                format!("provider:{left}:{right}"),
                left,
                right,
                now,
            ])?;
    }
    if let Some((target, _)) = others.first() {
        tx.prepare_cached(sql::SUGGEST_DEEZER_MERGE.text())?
            .execute(params![artist, target, mbid, now])?;
    }
    Ok(())
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassTurn {
    pub looked: usize,
    pub linked: usize,
    pub merges: usize,
    pub split: usize,
}

/// Album evidence for one artist, in its own transaction.
fn evidence_one(
    tx: &Transaction<'_>,
    artist: &str,
    name: &str,
    now: i64,
) -> Result<ProviderLinkOutcome, ArtistLinkError> {
    tx.prepare_cached(sql::MARK_EVIDENCE.text())?
        .execute(params![artist, now])?;
    let mut releases = HashMap::new();
    let credits = credits_of(tx, artist, &mut releases)?;
    let Evidence::One { mbid, .. } = judge(name, &credits) else {
        return Ok(ProviderLinkOutcome::Skipped);
    };
    let outcome = set_from_provider_link(tx, artist, &mbid, LINK_FROM_ALBUMS, now)?;
    if let ProviderLinkOutcome::Shared(others) = &outcome {
        offer_shared(tx, artist, others, &mbid, now)?;
    }
    Ok(outcome)
}

/// Link up to `batch` unlinked artists their matched albums settle.
/// Blocking.
///
/// # Errors
///
/// A store fault.
pub fn accept_from_albums(db: &Db, batch: u32, now: i64) -> Result<PassTurn, String> {
    let pending = db
        .read_blocking("artist_match.evidence_pending", |conn| {
            conn.prepare_cached(sql::EVIDENCE_PENDING.text())?
                .query_map(
                    params![i64::from(batch), now - EVIDENCE_RECHECK_MS],
                    sql::NamedArtistRow::read,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DbError::from)
        })
        .map_err(|error: DbError| error.to_string())?;
    let mut turn = PassTurn::default();
    for row in pending {
        let outcome = db
            .write_blocking(Lane::Background, "artist_match.evidence", |tx| {
                evidence_one(tx, &row.artist_id, &row.name, now)
            })
            .map_err(|error| error.to_string())?;
        turn.looked += 1;
        match outcome {
            ProviderLinkOutcome::Linked => turn.linked += 1,
            ProviderLinkOutcome::Shared(_) => turn.merges += 1,
            ProviderLinkOutcome::Skipped => {}
        }
    }
    if turn.linked + turn.merges > 0 {
        tracing::info!(
            looked = turn.looked,
            linked = turn.linked,
            merges = turn.merges,
            "artists linked to musicbrainz by their albums' release credits"
        );
    }
    Ok(turn)
}

/// One credit row, as both credit tables hold it.
struct CreditRow {
    artist: String,
    role: String,
    credited_name: Option<String>,
    join_phrase: String,
    artist_mbid: Option<String>,
}

/// `credits` with every credit of `joined` replaced by `parts` (row id,
/// name, join phrase), the first in its place and the rest after it,
/// leaving out an artist the subject credits already.
fn recredit(
    credits: Vec<CreditRow>,
    joined: &str,
    parts: &[(String, String, String)],
) -> Vec<CreditRow> {
    let mut out: Vec<CreditRow> = Vec::new();
    for credit in credits {
        if credit.artist != joined {
            out.push(credit);
            continue;
        }
        let count = parts.len();
        for (index, (artist, name, join)) in parts.iter().enumerate() {
            out.push(CreditRow {
                artist: artist.clone(),
                role: credit.role.clone(),
                credited_name: Some(name.clone()),
                join_phrase: if index + 1 == count {
                    credit.join_phrase.clone()
                } else {
                    join.clone()
                },
                artist_mbid: None,
            });
        }
    }
    let mut seen = BTreeSet::new();
    out.retain(|credit| seen.insert(credit.artist.clone()));
    out
}

fn read_credits(
    tx: &Transaction<'_>,
    statement: droppedneedle_kernel::db::statement::Statement,
    subject: &str,
) -> rusqlite::Result<Vec<CreditRow>> {
    tx.prepare_cached(statement.text())?
        .query_map(params![subject], |row| {
            Ok(CreditRow {
                artist: row.get(1)?,
                role: row.get(2)?,
                credited_name: row.get(3)?,
                join_phrase: row.get(4)?,
                artist_mbid: row.get(5)?,
            })
        })?
        .collect()
}

fn write_credits(
    tx: &Transaction<'_>,
    delete: droppedneedle_kernel::db::statement::Statement,
    insert: droppedneedle_kernel::db::statement::Statement,
    subject: &str,
    credits: &[CreditRow],
) -> rusqlite::Result<()> {
    tx.prepare_cached(delete.text())?
        .execute(params![subject])?;
    for (position, credit) in credits.iter().enumerate() {
        tx.prepare_cached(insert.text())?.execute(params![
            subject,
            position as i64,
            credit.artist,
            credit.role,
            credit.credited_name,
            credit.join_phrase,
            credit.artist_mbid,
        ])?;
    }
    Ok(())
}

/// Split joined credit row `joined` (named `name`), in its own
/// transaction. Answers whether it was split.
fn split_one(tx: &Transaction<'_>, joined: &str, name: &str, now: i64) -> rusqlite::Result<bool> {
    tx.prepare_cached(sql::MARK_SPLIT.text())?
        .execute(params![joined, now])?;
    let parts = split_credit(name, &|folded: &str| {
        known_folded(tx, folded).unwrap_or(true)
    });
    if parts.len() < 2 {
        return Ok(false);
    }
    let now_secs = now as f64 / 1000.0;
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for part in &parts {
        let row = crate::scan::sqlite_store::artists::row_for_name(tx, &part.name, now_secs)?;
        if row == joined {
            return Ok(false);
        }
        if rows.iter().all(|(known, _, _)| *known != row) {
            rows.push((row, part.name.clone(), part.join_phrase.clone()));
        }
    }
    if rows.len() < 2 {
        return Ok(false);
    }
    let mut albums: BTreeSet<String> = BTreeSet::new();
    let tracks = tx
        .prepare_cached(sql::TRACKS_CREDITING.text())?
        .query_map(params![joined], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    albums.extend(
        tx.prepare_cached(sql::TRACK_ALBUMS_CREDITING.text())?
            .query_map(params![joined], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    for track in tracks {
        let credits = recredit(read_credits(tx, sql::TRACK_CREDITS, &track)?, joined, &rows);
        write_credits(
            tx,
            sql::DELETE_TRACK_CREDITS,
            sql::INSERT_TRACK_CREDIT,
            &track,
            &credits,
        )?;
    }
    let credited_albums = tx
        .prepare_cached(sql::ALBUMS_CREDITING.text())?
        .query_map(params![joined], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for album in &credited_albums {
        let credits = recredit(read_credits(tx, sql::ALBUM_CREDITS, album)?, joined, &rows);
        write_credits(
            tx,
            sql::DELETE_ALBUM_CREDITS,
            sql::INSERT_ALBUM_CREDIT,
            album,
            &credits,
        )?;
    }
    albums.extend(credited_albums);
    tx.prepare_cached(sql::REPOINT_LEAD.text())?
        .execute(params![joined, rows[0].0, now])?;
    for album in &albums {
        mark_stale(tx, album)?;
    }
    let ids: Vec<String> = rows.iter().map(|(id, _, _)| id.clone()).collect();
    crate::corrections::retire_split_credit(tx, name, &ids, joined, now_secs)
}

/// Split up to `batch` joined credit rows. Blocking.
///
/// # Errors
///
/// A store fault.
pub fn split_some(db: &Db, batch: u32, now: i64) -> Result<PassTurn, String> {
    let pending = db
        .read_blocking("artist_match.split_pending", |conn| {
            conn.prepare_cached(sql::SPLIT_PENDING.text())?
                .query_map(
                    params![i64::from(batch), now - SPLIT_RECHECK_MS],
                    sql::NamedArtistRow::read,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DbError::from)
        })
        .map_err(|error: DbError| error.to_string())?;
    let mut turn = PassTurn::default();
    for row in pending {
        let split = db
            .write_blocking(Lane::Background, "artist_match.split", |tx| {
                split_one(tx, &row.artist_id, &row.name, now).map_err(DbError::from)
            })
            .map_err(|error: DbError| error.to_string())?;
        turn.looked += 1;
        turn.split += usize::from(split);
    }
    if turn.split > 0 {
        tracing::info!(
            looked = turn.looked,
            split = turn.split,
            "joined artist credits split"
        );
    }
    Ok(turn)
}

/// One backfill turn: album evidence first (a joined name MusicBrainz
/// knows as one artist is linked, and so kept whole), then splits.
///
/// # Errors
///
/// A store fault.
pub fn backfill_turn(db: &Db, now: i64) -> Result<PassTurn, String> {
    let evidence = accept_from_albums(db, 200, now)?;
    let split = split_some(db, 50, now)?;
    Ok(PassTurn {
        looked: evidence.looked + split.looked,
        linked: evidence.linked,
        merges: evidence.merges,
        split: split.split,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "5fdbe54d-7a81-410b-b734-85de65dba41b";
    const B: &str = "68cac857-3147-43d0-879e-c63dc9c82014";
    const VA: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

    fn albums(list: &[(&str, &[(&str, &str)])]) -> BTreeMap<String, Vec<(String, String)>> {
        list.iter()
            .map(|(album, credits)| {
                (
                    (*album).to_owned(),
                    credits
                        .iter()
                        .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn agreeing_albums_settle() {
        let got = judge(
            "Mind Against",
            &albums(&[
                ("x", &[(A, "Mind Against")]),
                ("y", &[(A, "MIND AGAINST"), (B, "Somebody")]),
                ("z", &[(B, "Somebody")]),
            ]),
        );
        assert_eq!(
            got,
            Evidence::One {
                mbid: A.to_owned(),
                albums: 2
            }
        );
    }

    #[test]
    fn disagreeing_albums_block() {
        let got = judge(
            "Avalon",
            &albums(&[("x", &[(A, "Avalon")]), ("y", &[(B, "Avalon")])]),
        );
        assert_eq!(got, Evidence::Disagree);
    }

    #[test]
    fn placeholders_and_other_names_prove_nothing() {
        let got = judge(
            "Café del Mar",
            &albums(&[("x", &[(VA, "Café del Mar")]), ("y", &[(A, "Somebody")])]),
        );
        assert_eq!(got, Evidence::Nothing);
    }
}
