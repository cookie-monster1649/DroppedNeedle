//! Fork: stand-in albums linked to the MusicBrainz group they turned out
//! to be.
//!
//! A Deezer album on a stand-in release group is sometimes a record
//! MusicBrainz knows after all: a person matched its files to the
//! MusicBrainz group, a landing placed them there, or a check before a
//! request found the group by barcode or name. The stand-in is then linked
//! to the group (`source_links`): [`group_owned`](crate::identity::reads::group_owned)
//! counts the stand-in as held when the library holds the group, its
//! album page opens the group's, and its request and wanted watch can
//! settle.
//!
//! [`repairs`] finds the links that are due for albums handled before the
//! links existed: download tasks or held entries for a stand-in whose
//! files sit under (or carry) a MusicBrainz group, albums whose confirmed
//! Deezer match now has a MusicBrainz identity, and stand-ins still asked
//! for whose artist and title a library album on one MusicBrainz group
//! carries.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension as _, params};

use droppedneedle_kernel::db::fold::fold_text;
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use droppedneedle_providers::virtual_ids::{self, Kind, is_virtual};

use super::editions::base_title;
use super::{identity_of, sql};
use crate::provider_match::{DEEZER, ProviderMatch};

/// What showed a stand-in to be a MusicBrainz group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkVia {
    /// Files fetched or held for the stand-in sit under, or carry, it.
    Files,
    /// The Deezer UPC is the barcode of one of its releases.
    Barcode,
    /// A confident artist, title and track count match at MusicBrainz.
    Name,
    /// A library album of the same artist and title holds it.
    Library,
}

droppedneedle_kernel::spelled!(LinkVia {
    Files => "files",
    Barcode => "barcode",
    Name => "name",
    Library => "library",
});

/// One link that is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repair {
    pub stand_in: String,
    pub musicbrainz_id: String,
    pub via: LinkVia,
}

/// A real MusicBrainz id, lower case, or `None`.
fn real(id: Option<&str>) -> Option<String> {
    let id = id?.trim().to_ascii_lowercase();
    (is_valid_mbid(&id) && !is_virtual(&id)).then_some(id)
}

/// Link stand-in release group `stand_in` to MusicBrainz group
/// `musicbrainz_id`. A stand-in keeps its first link. Answers whether a
/// link was made; ids that are not a stand-in and a real MusicBrainz id
/// make none.
///
/// # Errors
///
/// A store fault.
pub fn link(
    conn: &Connection,
    stand_in: &str,
    musicbrainz_id: &str,
    via: LinkVia,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    let stand_in = stand_in.trim().to_ascii_lowercase();
    let Some(group) = real(Some(musicbrainz_id)) else {
        return Ok(false);
    };
    if !is_virtual(&stand_in) {
        return Ok(false);
    }
    let changed = conn
        .prepare_cached(sql::KEEP_LINK.text())?
        .execute(params![stand_in, group, via.as_str(), now_ms])?;
    Ok(changed > 0)
}

/// The MusicBrainz group stand-in `stand_in` is linked to.
///
/// # Errors
///
/// A store fault.
pub fn linked(conn: &Connection, stand_in: &str) -> rusqlite::Result<Option<String>> {
    let stand_in = stand_in.trim().to_ascii_lowercase();
    if !is_virtual(&stand_in) {
        return Ok(None);
    }
    conn.prepare_cached(sql::LINKED.text())?
        .query_row([stand_in], |row| row.get(0))
        .optional()
}

/// The Deezer album a stand-in release group or release names: its match,
/// its edition of a matched album, or its catalogue entry (track list
/// empty until it has been read).
///
/// # Errors
///
/// A store fault.
pub fn album(conn: &Connection, stand_in: &str) -> rusqlite::Result<Option<ProviderMatch>> {
    let Some(identity) = identity_of(conn, stand_in)? else {
        return Ok(None);
    };
    if identity.provider != DEEZER || !matches!(identity.kind, Kind::ReleaseGroup | Kind::Release) {
        return Ok(None);
    }
    let Some(album) = identity.provider_album_id.as_deref() else {
        return Ok(None);
    };
    super::deezer::album_of(conn, album)
}

/// The stand-in releases of stand-in release group `group`: its own, then
/// its Deezer editions. Empty for any other id.
///
/// # Errors
///
/// A store fault.
pub fn releases(conn: &Connection, group: &str) -> rusqlite::Result<Vec<String>> {
    let Some(identity) = identity_of(conn, group)? else {
        return Ok(Vec::new());
    };
    if identity.provider != DEEZER || identity.kind != Kind::ReleaseGroup {
        return Ok(Vec::new());
    }
    let Some(album) = identity.provider_album_id else {
        return Ok(Vec::new());
    };
    let mut out = vec![virtual_ids::mint(DEEZER, Kind::Release, &album)];
    let editions: Vec<String> = conn
        .prepare_cached(sql::EDITIONS_OF.text())?
        .query_map(params![DEEZER, album], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    out.extend(
        editions
            .iter()
            .map(|edition| virtual_ids::mint(DEEZER, Kind::Release, edition)),
    );
    Ok(out)
}

/// The group a library album's files sit under: the album's MusicBrainz
/// identity, else (an album with no identity yet) the group the files'
/// tags carry. An album on a stand-in identity sits under none.
fn files_group(identity: Option<&str>, embedded: Option<&str>) -> Option<String> {
    match identity {
        Some(id) if is_virtual(id) => None,
        Some(id) => real(Some(id)),
        None => real(embedded),
    }
}

/// Whether two album titles name the same album, edition notes aside.
pub fn same_title(one: &str, other: &str) -> bool {
    let one = base_title(one);
    !one.is_empty() && one == base_title(other)
}

/// One row of [`sql::TASK_FILE_GROUPS`] or [`sql::HELD_FILE_GROUPS`]: the
/// stand-in, the title it was asked as, the files' album's title, its
/// group (if any), the group the files carry.
type FileRow = (String, String, String, Option<String>, Option<String>);

/// Add each row's group to its stand-in, when the files' album carries the
/// title the stand-in was asked as (files of another album are no proof).
fn collect_files(rows: Vec<FileRow>, into: &mut BTreeMap<String, BTreeSet<String>>) {
    for (stand_in, asked, album, identity, embedded) in rows {
        if !same_title(&asked, &album) {
            continue;
        }
        if let Some(group) = files_group(identity.as_deref(), embedded.as_deref()) {
            into.entry(stand_in.to_ascii_lowercase())
                .or_default()
                .insert(group);
        }
    }
}

fn file_rows(conn: &Connection, text: &str) -> rusqlite::Result<Vec<FileRow>> {
    conn.prepare_cached(text)?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect()
}

/// Linked stand-ins with an open album request or a wanted watch, which
/// settle once the library holds the group they are linked to.
///
/// # Errors
///
/// A store fault.
pub fn linked_open(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(sql::LINKED_OPEN.text())?
        .query_map([], |row| row.get(0))?
        .collect()
}

/// Every stand-in with exactly one group in `found`, not yet linked.
fn single(
    found: BTreeMap<String, BTreeSet<String>>,
    via: LinkVia,
    out: &mut Vec<Repair>,
    linked: &mut BTreeSet<String>,
) {
    for (stand_in, groups) in found {
        if !is_virtual(&stand_in) || linked.contains(&stand_in) || groups.len() != 1 {
            continue;
        }
        if let Some(group) = groups.into_iter().next() {
            linked.insert(stand_in.clone());
            out.push(Repair {
                stand_in,
                musicbrainz_id: group,
                via,
            });
        }
    }
}

/// The MusicBrainz group of the one live library album, holding files, by
/// artist `artist` titled `title` (edition notes aside); `None` when there
/// is none or more than one group.
///
/// # Errors
///
/// A store fault.
pub fn library_group(
    conn: &Connection,
    artist: &str,
    title: &str,
) -> rusqlite::Result<Option<String>> {
    let (artist, key) = (fold_text(artist), base_title(title));
    if artist.is_empty() || key.is_empty() {
        return Ok(None);
    }
    let rows: Vec<(String, String)> = conn
        .prepare_cached(sql::ARTIST_ALBUM_GROUPS.text())?
        .query_map([artist], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let groups: BTreeSet<String> = rows
        .into_iter()
        .filter(|(album_title, _)| {
            let found = base_title(album_title);
            !found.is_empty() && found == key
        })
        .filter_map(|(_, group)| real(Some(&group)))
        .collect();
    Ok((groups.len() == 1)
        .then(|| groups.into_iter().next())
        .flatten())
}

/// The links that are due, by rule:
///
/// 1. an album download for a stand-in whose files are in the library on
///    an album of the same title under one MusicBrainz group (the album's
///    identity, or the group the files carry on an album with none);
/// 2. a held entry for a stand-in whose files went in from a download,
///    the same way;
/// 3. an album whose confirmed Deezer match was minted a stand-in group
///    and whose identity is now a MusicBrainz group;
/// 4. a stand-in still asked for (an open request, a wanted watch, a
///    download task or an open held entry) whose Deezer artist and title
///    one library album carries, on one MusicBrainz group.
///
/// A stand-in that two groups answer is left alone.
///
/// # Errors
///
/// A store fault.
pub fn repairs(conn: &Connection) -> rusqlite::Result<Vec<Repair>> {
    let mut out = Vec::new();
    let mut linked: BTreeSet<String> = conn
        .prepare_cached(sql::ALL_LINKED.text())?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut files: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    collect_files(file_rows(conn, sql::TASK_FILE_GROUPS.text())?, &mut files);
    collect_files(file_rows(conn, sql::HELD_FILE_GROUPS.text())?, &mut files);
    single(files, LinkVia::Files, &mut out, &mut linked);
    let matched: Vec<(String, String)> = conn
        .prepare_cached(sql::MATCHED_GROUPS.text())?
        .query_map([DEEZER], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut promoted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (album, group) in matched {
        let stand_in = virtual_ids::mint(DEEZER, Kind::ReleaseGroup, &album);
        if identity_of(conn, &stand_in)?.is_none() {
            continue;
        }
        if let Some(group) = real(Some(&group)) {
            promoted.entry(stand_in).or_default().insert(group);
        }
    }
    single(promoted, LinkVia::Library, &mut out, &mut linked);
    let asked: Vec<String> = conn
        .prepare_cached(sql::ASKED_STAND_INS.text())?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for stand_in in asked {
        let stand_in = stand_in.to_ascii_lowercase();
        if !is_virtual(&stand_in) || linked.contains(&stand_in) {
            continue;
        }
        let Some(found) = album(conn, &stand_in)? else {
            continue;
        };
        if let Some(group) = library_group(conn, &found.artist_name, &found.title)? {
            linked.insert(stand_in.clone());
            out.push(Repair {
                stand_in,
                musicbrainz_id: group,
                via: LinkVia::Library,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: &str = "ea8e626f-520f-4f54-9fab-8004e702afef";
    const OTHER: &str = "1dbbabcb-6902-40ec-8a7f-bcb1aa15d6bd";

    #[test]
    fn titles_match_with_edition_notes_aside() {
        assert!(same_title("Typewritten (Deluxe)", "Typewritten"));
        assert!(!same_title("Typewritten", "Other"));
        assert!(!same_title("", ""));
    }

    #[test]
    fn files_sit_under_the_album_identity_else_what_they_carry() {
        let stand_in = virtual_ids::mint(DEEZER, Kind::ReleaseGroup, "1");
        assert_eq!(files_group(Some(MB), Some(OTHER)), Some(MB.to_owned()));
        assert_eq!(files_group(None, Some(OTHER)), Some(OTHER.to_owned()));
        assert_eq!(files_group(Some(&stand_in), Some(OTHER)), None);
        assert_eq!(files_group(None, Some(&stand_in)), None);
        assert_eq!(files_group(None, Some("not-an-id")), None);
    }

    #[test]
    fn only_a_stand_in_one_group_answers_is_linked() {
        let one = virtual_ids::mint(DEEZER, Kind::ReleaseGroup, "1");
        let two = virtual_ids::mint(DEEZER, Kind::ReleaseGroup, "2");
        let three = virtual_ids::mint(DEEZER, Kind::ReleaseGroup, "3");
        let mut found = BTreeMap::new();
        found.insert(one.clone(), BTreeSet::from([MB.to_owned()]));
        found.insert(
            two.clone(),
            BTreeSet::from([MB.to_owned(), OTHER.to_owned()]),
        );
        found.insert(three.clone(), BTreeSet::from([OTHER.to_owned()]));
        let mut linked = BTreeSet::from([three]);
        let mut out = Vec::new();
        single(found, LinkVia::Files, &mut out, &mut linked);
        assert_eq!(
            out,
            vec![Repair {
                stand_in: one,
                musicbrainz_id: MB.to_owned(),
                via: LinkVia::Files,
            }]
        );
    }
}
