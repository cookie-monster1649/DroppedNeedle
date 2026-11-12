//! Fork: a source artist's whole discography.
//!
//! An artist on a stand-in id lists every album its source has, not only
//! the held ones. Each album the source lists is kept in
//! `source_catalogue` and its release group and release minted, so the
//! artist page can link it and the album page (through the virtual
//! MusicBrainz) can read it. Its track list is read from the source the
//! first time one of its ids is looked up, then kept; a month later it is
//! read again. A request for it is a normal `acquire_edition` on its
//! release, the same one-download path as an adopted album.

use rusqlite::{Connection, OptionalExtension as _, params};

use droppedneedle_providers::preview::{DeezerAlbum, DeezerArtistAlbum};
use droppedneedle_providers::virtual_ids::{self, Kind};

use super::{SourceIdentity, mint, sql};
use crate::provider_match::{DEEZER, MatchTrack, ProviderMatch, ProviderMatchState, deezer_url};

/// How long a kept track list stands before the source is read again.
pub const TRACKS_RECHECK_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// One album of a source artist's discography, as the artist page lists
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueAlbum {
    pub provider_album_id: String,
    pub release_group: String,
    pub title: String,
    pub release_date: Option<String>,
    pub record_type: Option<String>,
    /// A local album holds it (by its release group, or by the album's
    /// confirmed match or edition).
    pub held: bool,
}

impl CatalogueAlbum {
    /// The year of its release date.
    pub fn year(&self) -> Option<i32> {
        self.release_date
            .as_deref()
            .and_then(|date| date.get(..4))
            .and_then(|year| year.parse().ok())
            .filter(|year| *year > 0)
    }
}

/// Keep Deezer artist `artist_id`'s discography `albums` (as listed, track
/// lists untouched) and mint each album's release group and release.
///
/// # Errors
///
/// A store fault.
pub fn keep_listing(
    conn: &Connection,
    artist_id: &str,
    artist_name: &str,
    albums: &[DeezerArtistAlbum],
    now: i64,
) -> rusqlite::Result<()> {
    for album in albums {
        let id = album.id.to_string();
        conn.prepare_cached(sql::KEEP_LISTED.text())?
            .execute(params![
                DEEZER,
                id,
                artist_id,
                artist_name,
                album.title,
                album.release_date,
                album.record_type,
                album.cover_url,
                now,
            ])?;
        mint(conn, DEEZER, Kind::ReleaseGroup, &id, Some(&id), now)?;
        mint(conn, DEEZER, Kind::Release, &id, Some(&id), now)?;
    }
    Ok(())
}

fn tracks_of(album: &DeezerAlbum) -> Vec<MatchTrack> {
    album
        .tracks
        .iter()
        .map(|track| MatchTrack {
            provider_track_id: track.id.to_string(),
            title: track.title.clone(),
            duration_seconds: track.duration.map(|seconds| seconds as f64),
            disc: track.disc,
            position: track.position,
            isrc: track.isrc.clone(),
        })
        .collect()
}

/// Keep Deezer album `album`'s details and track list, and mint its
/// release tracks and recordings. The album keeps the artist it was
/// listed under, so it stays on that artist's page.
///
/// # Errors
///
/// A store fault.
pub fn keep_album(conn: &Connection, album: &DeezerAlbum, now: i64) -> rusqlite::Result<()> {
    let id = album.id.to_string();
    let tracks = tracks_of(album);
    conn.prepare_cached(sql::KEEP_CATALOGUE_ALBUM.text())?
        .execute(params![
            DEEZER,
            id,
            album
                .artist_id
                .map(|artist| artist.to_string())
                .unwrap_or_default(),
            album.artist_name,
            album.title,
            album.release_date,
            album.record_type,
            album.cover_url,
            album.upc,
            album.label,
            serde_json::to_string(&album.genres).unwrap_or_else(|_| "[]".to_owned()),
            serde_json::to_string(&tracks).unwrap_or_else(|_| "[]".to_owned()),
            now,
        ])?;
    mint(conn, DEEZER, Kind::ReleaseGroup, &id, Some(&id), now)?;
    mint(conn, DEEZER, Kind::Release, &id, Some(&id), now)?;
    for track in &tracks {
        mint(
            conn,
            DEEZER,
            Kind::ReleaseTrack,
            &track.provider_track_id,
            Some(&id),
            now,
        )?;
        mint(
            conn,
            DEEZER,
            Kind::Recording,
            &track.provider_track_id,
            Some(&id),
            now,
        )?;
    }
    Ok(())
}

/// The Deezer album a catalogue id names whose track list is missing or
/// older than [`TRACKS_RECHECK_MS`] at `now`. `None` for any other id,
/// and for an album a match or an edition already describes.
///
/// # Errors
///
/// A store fault.
pub fn album_to_read(
    conn: &Connection,
    identity: &SourceIdentity,
    now: i64,
) -> rusqlite::Result<Option<i64>> {
    if identity.provider != DEEZER
        || !matches!(
            identity.kind,
            Kind::ReleaseGroup | Kind::Release | Kind::ReleaseTrack | Kind::Recording
        )
    {
        return Ok(None);
    }
    let Some(album) = identity.provider_album_id.as_deref() else {
        return Ok(None);
    };
    let due: Option<i64> = conn
        .prepare_cached(sql::CATALOGUE_DUE.text())?
        .query_row(params![DEEZER, album, now - TRACKS_RECHECK_MS], |row| {
            row.get(0)
        })
        .optional()?;
    if due.is_none() {
        return Ok(None);
    }
    Ok(album.parse().ok())
}

/// Deezer album `album` from the catalogue, as a match of its own: `None`
/// until it is listed. Its track list is empty until it has been read.
///
/// # Errors
///
/// A store fault.
pub fn stored(conn: &Connection, album: &str) -> rusqlite::Result<Option<ProviderMatch>> {
    type Row = (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
    );
    let row: Option<Row> = conn
        .prepare_cached(sql::CATALOGUE_ALBUM.text())?
        .query_row(params![DEEZER, album], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })
        .optional()?;
    let Some((
        artist_id,
        artist_name,
        title,
        release_date,
        record_type,
        cover_url,
        upc,
        label,
        genres,
        tracks,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(ProviderMatch {
        album_id: String::new(),
        provider: DEEZER.to_owned(),
        provider_album_id: album.to_owned(),
        title,
        artist_name,
        provider_artist_id: Some(artist_id).filter(|id| !id.is_empty()),
        artist_picture_url: None,
        release_date,
        label,
        upc,
        record_type,
        cover_url,
        link_url: Some(deezer_url("album", album)),
        genres: serde_json::from_str(&genres).unwrap_or_default(),
        tracks: tracks
            .and_then(|tracks| serde_json::from_str(&tracks).ok())
            .unwrap_or_default(),
        score: 1.0,
        state: ProviderMatchState::Confirmed,
    }))
}

/// Every catalogue album of Deezer artist `artist_id`, newest first, each
/// with its release group and whether the library holds it.
///
/// # Errors
///
/// A store fault.
pub fn albums_of(conn: &Connection, artist_id: &str) -> rusqlite::Result<Vec<CatalogueAlbum>> {
    let rows: Vec<(String, String, Option<String>, Option<String>)> = conn
        .prepare_cached(sql::CATALOGUE_OF_ARTIST.text())?
        .query_map(params![DEEZER, artist_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (album, title, release_date, record_type) in rows {
        let release_group = virtual_ids::mint(DEEZER, Kind::ReleaseGroup, &album);
        let held: bool = conn
            .prepare_cached(sql::CATALOGUE_HELD.text())?
            .query_row(params![DEEZER, album, release_group], |row| row.get(0))?;
        out.push(CatalogueAlbum {
            provider_album_id: album,
            release_group,
            title,
            release_date,
            record_type,
            held,
        });
    }
    Ok(out)
}

/// The Deezer artist stand-in artist `uuid` names, if it was minted.
///
/// # Errors
///
/// A store fault.
pub fn artist_of(conn: &Connection, uuid: &str) -> rusqlite::Result<Option<String>> {
    Ok(super::identity_of(conn, uuid)?
        .filter(|identity| identity.provider == DEEZER && identity.kind == Kind::Artist)
        .map(|identity| identity.provider_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_year_comes_from_the_date() {
        let album = CatalogueAlbum {
            provider_album_id: "1".into(),
            release_group: String::new(),
            title: "T".into(),
            release_date: Some("2019-04-05".into()),
            record_type: None,
            held: false,
        };
        assert_eq!(album.year(), Some(2019));
        let none = CatalogueAlbum {
            release_date: Some("0000-00-00".into()),
            ..album
        };
        assert_eq!(none.year(), None);
    }
}
