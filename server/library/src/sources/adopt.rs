//! Fork: an album confirmed from a source takes its stand-in identity.
//!
//! The album's release group, release, release tracks and recordings
//! become the source's stand-in ids, written as a person's choice through
//! the identity module ([`crate::identity::choose`]), so the album reads
//! as identified everywhere: its page, ownership, the edition picker,
//! management, Subsonic and Jellyfin. Automatic identification keeps a
//! person's choice, so it never undoes this; [`super::promote`] hands the
//! album back when MusicBrainz has a confident match.
//!
//! An album that already has an identity is left alone: a MusicBrainz
//! match wins, and a stand-in identity is never minted twice.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_providers::virtual_ids::Kind;

use super::deezer::{AlbumIds, recording_id, release_track_id};
use super::{mint, sql};
use crate::identity::{self, Offer, Placed};
use crate::jobs::models::OperationError;
use crate::provider_match::{
    DEEZER, ProviderMatch, ProviderMatchState, held_files, held_tracks, match_of,
};

/// What adoption did with an album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adopted {
    /// The album took its stand-in identity.
    Adopted {
        release_group: String,
        placed: usize,
    },
    /// It already has an identity: MusicBrainz's, or this one.
    HasIdentity,
    /// None of its files is on the source's track list.
    NoFileFits,
    /// The match is not a confirmed Deezer match.
    NotConfirmed,
}

/// How long an album no file of fits waits before adoption looks again.
const UNPLACEABLE_RETRY_MS: i64 = 30 * 24 * 3600 * 1000;

/// The album's release group, when it has an identity.
fn current_group(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(sql::ALBUM_GROUP.text())?
        .query_row([album_id], |row| row.get(0))
        .optional()
}

/// Give the album of confirmed match `found` its stand-in identity, as
/// `actor`'s choice (`None` for the backfill), at `now_ms`.
///
/// # Errors
///
/// A store fault, or the identity module refusing the choice for a reason
/// other than no file fitting.
pub fn adopt(
    tx: &Transaction<'_>,
    found: &ProviderMatch,
    actor: Option<&str>,
    now_ms: i64,
) -> Result<Adopted, OperationError> {
    if found.provider != DEEZER || found.state != ProviderMatchState::Confirmed {
        return Ok(Adopted::NotConfirmed);
    }
    if current_group(tx, &found.album_id)?.is_some() {
        return Ok(Adopted::HasIdentity);
    }
    let album = found.provider_album_id.as_str();
    let ids = AlbumIds::read(tx, found)?;
    mint(tx, DEEZER, Kind::ReleaseGroup, album, Some(album), now_ms)?;
    mint(tx, DEEZER, Kind::Release, album, Some(album), now_ms)?;
    if let Some(artist) = &found.provider_artist_id {
        mint(tx, DEEZER, Kind::Artist, artist, Some(album), now_ms)?;
    }
    if let Some(label) = found
        .label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        mint(tx, DEEZER, Kind::Label, label, Some(album), now_ms)?;
    }
    let files = held_files(tx, &found.album_id)?;
    let mut spots = Vec::new();
    let mut missing = Vec::new();
    for held in held_tracks(&files, &found.tracks) {
        let track = &held.track.provider_track_id;
        let release_track = mint(tx, DEEZER, Kind::ReleaseTrack, track, Some(album), now_ms)?;
        let recording = mint(tx, DEEZER, Kind::Recording, track, Some(album), now_ms)?;
        debug_assert_eq!(release_track, release_track_id(track));
        debug_assert_eq!(recording, recording_id(track));
        match held.track_id {
            Some(local_track_id) => spots.push(Placed {
                local_track_id,
                recording_mbid: recording,
                release_track_mbid: release_track,
                disc: u32::try_from(held.track.disc.max(1)).unwrap_or(1),
                position: u32::try_from(held.track.position.max(1)).unwrap_or(1),
            }),
            None => missing.push(release_track),
        }
    }
    if spots.is_empty() {
        tx.prepare_cached(sql::NOTE_CHECK.text())?.execute(params![
            found.album_id,
            now_ms,
            super::CheckOutcome::Unplaceable
        ])?;
        return Ok(Adopted::NoFileFits);
    }
    let offer = Offer {
        release_group_mbid: ids.release_group.clone(),
        release_mbid: ids.release.clone(),
        spots,
        weighed: files.iter().map(|file| file.track_id.clone()).collect(),
        use_anyway: false,
    };
    // The edition's document, as identification keeps a fetched one, so
    // management and the edition view read it without a lookup.
    let wire = serde_json::from_value(super::deezer::release(found, &ids))
        .map_err(|error| OperationError::Store(format!("stand-in release: {error}")))?;
    let document = crate::identify::sources::release_from_wire(wire, Vec::new())
        .map_err(|error| OperationError::Store(format!("stand-in release: {error:?}")))?;
    crate::identify::sqlite::keep_release_in(tx, &document, now_ms as f64 / 1000.0)
        .map_err(|error| OperationError::Store(format!("stand-in release: {error}")))?;
    let candidate_key = format!("{DEEZER}:{album}");
    let chosen = identity::choose(
        tx,
        &found.album_id,
        &offer,
        &candidate_key,
        actor,
        &missing,
        now_ms as f64 / 1000.0,
    );
    match chosen {
        Ok(chosen) => Ok(Adopted::Adopted {
            release_group: chosen.release_group_mbid,
            placed: chosen.placed.len(),
        }),
        Err(identity::IdentityError::Refused(_)) => {
            tx.prepare_cached(sql::NOTE_CHECK.text())?.execute(params![
                found.album_id,
                now_ms,
                super::CheckOutcome::Unplaceable
            ])?;
            Ok(Adopted::NoFileFits)
        }
        Err(error) => Err(error.into()),
    }
}

/// Adopt up to `batch` confirmed albums that have no identity yet, each in
/// its own write. Answers how many took their identity. Blocking.
///
/// # Errors
///
/// A store fault reading the albums due; one album's failure is logged and
/// the rest go on.
pub fn backfill_some(db: &Db, batch: u32, now_ms: i64) -> Result<usize, OperationError> {
    let due: Vec<String> = db.read_blocking("sources.backfill_due", |conn| {
        let mut stmt = conn.prepare_cached(sql::UNADOPTED.text())?;
        let rows = stmt
            .query_map(
                params![DEEZER, batch, now_ms - UNPLACEABLE_RETRY_MS],
                |row| row.get(0),
            )?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok::<_, OperationError>(rows)
    })?;
    let mut adopted = 0;
    for album_id in due {
        let outcome = db.write_blocking(Lane::Background, "sources.backfill", |tx| {
            let Some(found) = match_of(tx, &album_id)? else {
                return Ok(Adopted::NotConfirmed);
            };
            // The person who confirmed the match made the choice.
            let actor = decided_by(tx, &album_id)?;
            adopt(tx, &found, actor.as_deref(), now_ms)
        });
        match outcome {
            Ok(Adopted::Adopted {
                release_group,
                placed,
            }) => {
                adopted += 1;
                tracing::info!(album = %album_id, %release_group, placed, "album took its Deezer identity");
            }
            Ok(other) => tracing::debug!(album = %album_id, ?other, "album not adopted"),
            Err(error) => {
                tracing::warn!(album = %album_id, %error, "album could not take its Deezer identity")
            }
        }
    }
    Ok(adopted)
}

fn decided_by(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(sql::DECIDED_BY.text())?
        .query_row([album_id], |row| row.get(0))
        .optional()
        .map(Option::flatten)
}
