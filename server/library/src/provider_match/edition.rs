//! Fork: an accepted Deezer match as an edition Library Management can
//! write tags from.
//!
//! Library Management writes tags only from a release. An album with an
//! accepted Deezer match and no MusicBrainz link gets a stand-in release
//! built from the match, the way a custom edition gets one from its
//! sealed manifest. Its id is [`EDITION_PREFIX`] and the Deezer album id,
//! and its tracks are keyed by library track id. The tag write that uses
//! it names only [`FIELDS`], so no MusicBrainz id is ever written:
//!
//! - album, album artist, date and label from the match;
//! - title, track number and ISRC from the Deezer track a file is, a file
//!   no Deezer track accounts for keeping its own title and number;
//! - artist from the match only where the file's own is empty or is the
//!   old album artist, so a featured artist is kept.
//!
//! The scan reads the new tags back. It carries the album row over when
//! all of its files are retagged together, so the album keeps its id, its
//! match row and its links.

use rusqlite::Connection;

use crate::matching::{CreditedArtist, Release, ReleaseMedium, ReleaseTrack};

use super::sql::{EDITION_FILES, EditionFileRow};
use super::{HeldFile, ProviderMatch, ProviderMatchState, held_tracks, match_of};

/// Stand-in release ids start with this, followed by the Deezer album id.
pub const EDITION_PREFIX: &str = "deezer:";

/// The tags a Deezer tag write manages, by managed-field name.
pub const FIELDS: &[&str] = &[
    "album",
    "album_artist",
    "artist",
    "title",
    "track_number",
    "date",
    "isrc",
    "label",
];

/// True for a stand-in release id built from a Deezer match.
pub fn is_edition_id(release_mbid: &str) -> bool {
    release_mbid.starts_with(EDITION_PREFIX)
}

/// The stand-in release id of a match.
pub fn edition_id(found: &ProviderMatch) -> String {
    format!("{EDITION_PREFIX}{}", found.provider_album_id)
}

/// One file of the album as its tags stand.
#[derive(Debug, Clone, PartialEq)]
pub struct EditionFile {
    pub track_id: String,
    pub title: String,
    pub artist_name: Option<String>,
    pub album_artist_name: Option<String>,
    pub disc_number: i64,
    pub track_number: i64,
    pub duration_seconds: Option<f64>,
}

impl From<EditionFileRow> for EditionFile {
    fn from(row: EditionFileRow) -> Self {
        Self {
            track_id: row.track_id,
            title: row.title,
            artist_name: row.artist_name,
            album_artist_name: row.album_artist_name,
            disc_number: row.disc_number,
            track_number: row.track_number,
            duration_seconds: row.duration_seconds,
        }
    }
}

fn credited(name: &str) -> CreditedArtist {
    CreditedArtist {
        id: String::new(),
        name: name.to_owned(),
        sort_name: None,
        join: String::new(),
        disambiguation: None,
    }
}

fn folded(value: &str) -> String {
    droppedneedle_providers::preview::norm(value)
}

/// The stand-in release for `found` over the album's `files`.
pub fn release_from(found: &ProviderMatch, files: &[EditionFile]) -> Release {
    let held: Vec<HeldFile> = files
        .iter()
        .map(|file| HeldFile {
            track_id: file.track_id.clone(),
            title: file.title.clone(),
            duration_seconds: file.duration_seconds,
            isrc: None,
        })
        .collect();
    let pairs = held_tracks(&held, &found.tracks);
    let tracks: Vec<ReleaseTrack> = files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let matched = pairs
                .iter()
                .find(|pair| pair.track_id.as_deref() == Some(file.track_id.as_str()))
                .map(|pair| &pair.track);
            let own_artist = file
                .artist_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty());
            let old_album_artist = file.album_artist_name.as_deref().map(folded);
            let artist = match own_artist {
                Some(name) if old_album_artist.as_deref() != Some(folded(name).as_str()) => {
                    name.to_owned()
                }
                _ => found.artist_name.clone(),
            };
            let (title, disc, position) = match matched {
                Some(track) => (track.title.clone(), track.disc, track.position),
                None => (file.title.clone(), file.disc_number, file.track_number),
            };
            ReleaseTrack {
                id: file.track_id.clone(),
                title,
                artists: vec![credited(&artist)],
                disc: u32::try_from(disc).unwrap_or(1).max(1),
                position: u32::try_from(position).unwrap_or(0),
                absolute_position: u32::try_from(index + 1).unwrap_or(u32::MAX),
                length_ms: file
                    .duration_seconds
                    .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                    .map(|seconds| (seconds * 1000.0).round() as u64),
                isrcs: matched
                    .and_then(|track| track.isrc.clone())
                    .into_iter()
                    .collect(),
                ..ReleaseTrack::default()
            }
        })
        .collect();
    let mut discs: Vec<u32> = tracks.iter().map(|track| track.disc).collect();
    discs.sort_unstable();
    discs.dedup();
    let media = discs
        .iter()
        .map(|disc| ReleaseMedium {
            position: *disc,
            track_count: tracks.iter().filter(|track| track.disc == *disc).count() as u32,
            ..ReleaseMedium::default()
        })
        .collect();
    Release {
        id: edition_id(found),
        title: found.title.clone(),
        artists: vec![credited(&found.artist_name)],
        date: found.release_date.clone().filter(|date| !date.is_empty()),
        labels: found.label.clone().into_iter().collect(),
        media,
        tracks,
        ..Release::default()
    }
}

/// The album's accepted Deezer match, when it has one.
pub fn confirmed(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<ProviderMatch>> {
    Ok(match_of(conn, album_id)?.filter(|found| found.state == ProviderMatchState::Confirmed))
}

/// The album's stand-in release, when it has an accepted Deezer match and
/// indexed files. The caller checks it has no MusicBrainz link.
pub fn edition_release(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Release>> {
    let Some(found) = confirmed(conn, album_id)? else {
        return Ok(None);
    };
    let mut stmt = conn.prepare_cached(EDITION_FILES.text())?;
    let files: Vec<EditionFile> = stmt
        .query_map([album_id], EditionFileRow::read)?
        .map(|row| row.map(Into::into))
        .collect::<rusqlite::Result<_>>()?;
    if files.is_empty() {
        return Ok(None);
    }
    Ok(Some(release_from(&found, &files)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_match::MatchTrack;

    fn found() -> ProviderMatch {
        ProviderMatch {
            album_id: "a1".into(),
            provider: "deezer".into(),
            provider_album_id: "42".into(),
            title: "Real Title".into(),
            artist_name: "Real Artist".into(),
            provider_artist_id: None,
            artist_picture_url: None,
            release_date: Some("2021-05-07".into()),
            label: Some("Label".into()),
            upc: None,
            record_type: None,
            cover_url: None,
            link_url: None,
            genres: Vec::new(),
            tracks: vec![MatchTrack {
                provider_track_id: "t1".into(),
                title: "Song One".into(),
                duration_seconds: Some(200.0),
                disc: 1,
                position: 3,
                isrc: Some("GBAAA2100001".into()),
            }],
            score: 0.9,
            state: ProviderMatchState::Confirmed,
        }
    }

    fn file(id: &str, title: &str, artist: Option<&str>, number: i64) -> EditionFile {
        EditionFile {
            track_id: id.into(),
            title: title.into(),
            artist_name: artist.map(str::to_owned),
            album_artist_name: Some("real artist".into()),
            disc_number: 1,
            track_number: number,
            duration_seconds: Some(201.0),
        }
    }

    #[test]
    fn the_stand_in_takes_the_matchs_names_and_keeps_a_featured_artist() {
        let release = release_from(
            &found(),
            &[
                file("f1", "song one", Some("Real Artist"), 1),
                file("f2", "Bonus", Some("Guest Singer"), 9),
            ],
        );
        assert_eq!(release.id, "deezer:42");
        assert!(is_edition_id(&release.id));
        assert_eq!(release.title, "Real Title");
        assert_eq!(release.artists[0].name, "Real Artist");
        assert!(release.artists[0].id.is_empty());
        assert_eq!(release.labels, vec!["Label".to_owned()]);
        assert_eq!(release.date.as_deref(), Some("2021-05-07"));
        let one = &release.tracks[0];
        assert_eq!(one.id, "f1");
        assert_eq!(one.title, "Song One");
        assert_eq!(one.position, 3);
        assert_eq!(one.isrcs, vec!["GBAAA2100001".to_owned()]);
        assert_eq!(one.artists[0].name, "Real Artist");
        // A file no Deezer track accounts for keeps its title, number and
        // its own artist.
        let two = &release.tracks[1];
        assert_eq!(two.title, "Bonus");
        assert_eq!(two.position, 9);
        assert!(two.isrcs.is_empty());
        assert_eq!(two.artists[0].name, "Guest Singer");
    }

    #[test]
    fn a_file_with_no_artist_gets_the_matchs() {
        let release = release_from(&found(), &[file("f1", "Song One", None, 1)]);
        assert_eq!(release.tracks[0].artists[0].name, "Real Artist");
    }
}
