//! Fork: albums the library holds twice, for Check albums' Duplicates list.
//!
//! Two listed albums with the same folded album artist and title are one
//! album split in two (a MusicBrainz-matched half and a half grouped only
//! by its tags) or the same songs kept twice (one copy in FLAC, one in
//! MP3, in two folders). Each group names the album to keep (a MusicBrainz
//! match first, then the most files, then the better files) and the
//! surplus copies: for every song held more than once, every copy but the
//! best one. Tidying a group merges the others into the kept album and
//! sends the surplus copies to the recycle bin, through the existing merge
//! and removal routes, so both can be undone the usual ways.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::Connection;

use crate::edition::sql::{DUPLICATE_ALBUM_FILES, DuplicateFileRow};

/// Whether merging adds songs the kept album lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateKind {
    /// The others hold songs the kept album lacks: one album split in two.
    SameAlbum,
    /// Every song is already in the kept album: the same songs kept twice.
    SameSongs,
}

/// One file of a duplicated album.
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateTrack {
    pub track_id: String,
    pub title: String,
    pub title_folded: String,
    pub file_format: Option<String>,
    pub bit_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub duration_seconds: Option<f64>,
    pub row_revision: i64,
}

/// One album of a group.
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateAlbum {
    pub album_id: String,
    pub title: String,
    pub artist_name: String,
    pub row_revision: i64,
    pub release_group_mbid: Option<String>,
    pub tracks: Vec<DuplicateTrack>,
}

/// Albums that are one album, with what tidying them does.
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateGroup {
    pub kind: DuplicateKind,
    pub keep_album_id: String,
    /// The kept album first.
    pub albums: Vec<DuplicateAlbum>,
    /// Every copy of a song held more than once but its best one.
    pub surplus_track_ids: Vec<String>,
}

const LOSSLESS: &[&str] = &["flac", "alac", "wav", "aiff", "aif", "ape", "wv"];

/// How good a file is: any lossless file beats any lossy one, deeper
/// lossless beats shallower, and a higher bitrate beats a lower one.
pub fn quality(track: &DuplicateTrack) -> i64 {
    let format = track
        .file_format
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    if LOSSLESS.contains(&format.as_str()) {
        1_000_000 + track.bit_depth.unwrap_or(16) * 1_000
    } else {
        track.bit_rate.unwrap_or(0).max(0) / 1_000
    }
}

fn album_quality(album: &DuplicateAlbum) -> i64 {
    album.tracks.iter().map(quality).max().unwrap_or(0)
}

/// Group the files into duplicate groups: albums with one Deezer match
/// shared by another album group by that match (singles of one release),
/// the others by folded artist and title.
pub(super) fn group(rows: Vec<DuplicateFileRow>) -> Vec<DuplicateGroup> {
    let mut sharing: HashMap<&str, HashSet<&str>> = HashMap::new();
    for row in &rows {
        if let Some(id) = row.provider_album_id.as_deref() {
            sharing.entry(id).or_default().insert(row.album_id.as_str());
        }
    }
    let shared: HashSet<String> = sharing
        .into_iter()
        .filter(|(_, albums)| albums.len() > 1)
        .map(|(id, _)| id.to_owned())
        .collect();
    let mut by_key: BTreeMap<(String, String), Vec<DuplicateAlbum>> = BTreeMap::new();
    for row in rows {
        let key = match &row.provider_album_id {
            Some(id) if shared.contains(id) => ("\u{0}provider".to_owned(), id.clone()),
            _ => (row.artist_folded.clone(), row.title_folded.clone()),
        };
        let albums = by_key.entry(key).or_default();
        if albums
            .last()
            .is_none_or(|album| album.album_id != row.album_id)
        {
            albums.push(DuplicateAlbum {
                album_id: row.album_id.clone(),
                title: row.album_title.clone(),
                artist_name: row.artist_name.clone(),
                row_revision: row.album_revision,
                release_group_mbid: row.release_group_mbid.clone(),
                tracks: Vec::new(),
            });
        }
        let title = row.track_title.unwrap_or_default();
        let title_folded = if row.track_title_folded.is_empty() {
            title.to_lowercase()
        } else {
            row.track_title_folded
        };
        if let Some(album) = albums.last_mut() {
            album.tracks.push(DuplicateTrack {
                track_id: row.track_id,
                title,
                title_folded,
                file_format: row.file_format,
                bit_rate: row.bit_rate,
                bit_depth: row.bit_depth,
                duration_seconds: row.duration_seconds,
                row_revision: row.track_revision,
            });
        }
    }
    by_key
        .into_values()
        .filter(|albums| albums.len() > 1)
        .map(one_group)
        .collect()
}

fn one_group(mut albums: Vec<DuplicateAlbum>) -> DuplicateGroup {
    // The kept album: matched first, then the most files, then the best.
    albums.sort_by(|a, b| {
        b.release_group_mbid
            .is_some()
            .cmp(&a.release_group_mbid.is_some())
            .then(b.tracks.len().cmp(&a.tracks.len()))
            .then(album_quality(b).cmp(&album_quality(a)))
            .then(a.album_id.cmp(&b.album_id))
    });
    let kept: Vec<&str> = albums[0]
        .tracks
        .iter()
        .map(|track| track.title_folded.as_str())
        .collect();
    let adds_songs = albums[1..]
        .iter()
        .flat_map(|album| &album.tracks)
        .any(|track| !kept.contains(&track.title_folded.as_str()));
    // Per song, the best copy stays; a tie keeps the copy in the kept album.
    let mut copies: HashMap<&str, Vec<(usize, &DuplicateTrack)>> = HashMap::new();
    for (index, album) in albums.iter().enumerate() {
        for track in &album.tracks {
            copies
                .entry(track.title_folded.as_str())
                .or_default()
                .push((index, track));
        }
    }
    let mut surplus: Vec<String> = Vec::new();
    for mut held in copies.into_values().filter(|held| held.len() > 1) {
        held.sort_by(|(ia, a), (ib, b)| quality(b).cmp(&quality(a)).then(ia.cmp(ib)));
        surplus.extend(held[1..].iter().map(|(_, track)| track.track_id.clone()));
    }
    surplus.sort();
    DuplicateGroup {
        kind: if adds_songs {
            DuplicateKind::SameAlbum
        } else {
            DuplicateKind::SameSongs
        },
        keep_album_id: albums[0].album_id.clone(),
        albums,
        surplus_track_ids: surplus,
    }
}

/// Every duplicate group in the library.
pub fn duplicate_groups(conn: &Connection) -> rusqlite::Result<Vec<DuplicateGroup>> {
    let mut stmt = conn.prepare(DUPLICATE_ALBUM_FILES.text())?;
    let rows = stmt
        .query_map([], DuplicateFileRow::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(group(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn row(
        album: &str,
        mbid: Option<&str>,
        track: &str,
        title: &str,
        format: &str,
        kbps: i64,
    ) -> DuplicateFileRow {
        DuplicateFileRow {
            album_id: album.to_owned(),
            album_title: "One Night".to_owned(),
            artist_name: "Two Another".to_owned(),
            album_revision: 1,
            artist_folded: "two another".to_owned(),
            title_folded: "one night".to_owned(),
            release_group_mbid: mbid.map(str::to_owned),
            track_id: track.to_owned(),
            track_title: Some(title.to_owned()),
            track_title_folded: title.to_lowercase(),
            file_format: Some(format.to_owned()),
            bit_rate: Some(kbps * 1_000),
            bit_depth: None,
            duration_seconds: Some(200.0),
            track_revision: 1,
            provider_album_id: None,
        }
    }

    #[test]
    fn the_same_song_twice_keeps_the_lossless_copy() {
        let groups = group(vec![
            row("a1", None, "t1", "One Night", "flac", 900),
            row("a2", None, "t2", "One Night", "mp3", 320),
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, DuplicateKind::SameSongs);
        assert_eq!(groups[0].keep_album_id, "a1");
        assert_eq!(groups[0].surplus_track_ids, vec!["t2".to_owned()]);
    }

    #[test]
    fn a_split_album_keeps_its_musicbrainz_half_and_the_better_copies() {
        let groups = group(vec![
            row("tag", None, "t1", "Song A", "flac", 900),
            row("tag", None, "t2", "Song C", "flac", 900),
            row("mb", Some("rg"), "t3", "Song A", "mp3", 320),
            row("mb", Some("rg"), "t4", "Song B", "mp3", 320),
        ]);
        assert_eq!(groups[0].kind, DuplicateKind::SameAlbum);
        assert_eq!(groups[0].keep_album_id, "mb");
        assert_eq!(groups[0].albums[0].album_id, "mb");
        // Song A's MP3 goes; its FLAC moves into the kept album.
        assert_eq!(groups[0].surplus_track_ids, vec!["t3".to_owned()]);
    }

    #[test]
    fn singles_of_one_release_group_by_their_match() {
        let mut one = row("s1", None, "t1", "Song A", "flac", 900);
        one.title_folded = "song a".to_owned();
        one.provider_album_id = Some("42".to_owned());
        let mut two = row("s2", None, "t2", "Song B", "flac", 900);
        two.title_folded = "song b".to_owned();
        two.provider_album_id = Some("42".to_owned());
        let groups = group(vec![one, two]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, DuplicateKind::SameAlbum);
        assert!(groups[0].surplus_track_ids.is_empty());
    }

    #[test]
    fn a_lone_album_is_no_group() {
        assert!(group(vec![row("a1", None, "t1", "One Night", "flac", 900)]).is_empty());
    }
}
