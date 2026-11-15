//! Fork: the import's last step, "link spotify ids". Every file the import
//! placed (whatever resolved it, or by its tags) and every file it found
//! already in the library gets provider links on its library track, so a
//! later Spotify playlist sync links each entry straight to the file
//! (the catalogue's `LINK_LIBRARY`: same recording, else a `spotify`
//! `track` link with the entry's Spotify id, else an `isrc` `recording`
//! link with its ISRC).
//!
//! - Files pair to the rows of the playlist CSVs, in order: the file's ISRC
//!   is the row's ([`PairedBy::Isrc`]); the row the import's own CSV match
//!   recorded ([`PairedBy::Recorded`]); a row of the folder's own CSV with
//!   the same folded title, a shared first artist and a length within
//!   [`LENGTH_SLACK`] ([`PairedBy::Name`]); last, the same with the titles
//!   cut at their first dash or bracket, for tags that leave the version off
//!   ("Song" against "Song - Someone Remix", [`PairedBy::BaseName`]).
//! - A row never pairs to two different songs (two [`song_key`]s): a row
//!   two songs want at one step pairs to neither, and a row an earlier step
//!   gave one song is not given to another. Copies of one song share rows.
//! - Each file's track gets `spotify`/`track`/<id> for every Spotify id its
//!   rows carry, and `isrc`/`recording`/<ISRC> for the rows' ISRCs, else the
//!   file's own tag ISRC (so folders with no CSV still link by ISRC). The
//!   links are written as the want-identity links are
//!   ([`droppedneedle_library::identity::link_track`]): `automatic`, a link
//!   the track already has left alone. Nothing in the library removes a
//!   track's source links; they go only with the track itself.
//! - A duplicate links its kept copy's track; a file found in the library
//!   links the copy it was found as. A tag-only copy links once the scan
//!   has indexed it: the step runs again on the next pass
//!   ([`super::spawn_link_loop`]) and after every import.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;

use super::spotify_match::{self, Catalogue, Item, LENGTH_SLACK};
use super::{FileState, State, song_key};
use crate::requests::identities::{WantIdentityProvider, normalize_isrc, normalize_spotify_track};
use droppedneedle_library::matching::SourceLink;

/// A CSV row: its playlist's id in the catalogue and its place there.
pub type RowKey = (String, usize);

/// How a file paired to a row, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedBy {
    Isrc,
    Recorded,
    Name,
    BaseName,
}

impl PairedBy {
    fn label(self) -> &'static str {
        match self {
            Self::Isrc => "isrc",
            Self::Recorded => "recorded match",
            Self::Name => "title, artist, length",
            Self::BaseName => "title without version, artist, length",
        }
    }
}

/// Which rows each file paired to.
#[derive(Debug, Default, Clone)]
pub struct Pairing {
    /// By file path below the source.
    pub by_file: BTreeMap<String, Vec<(RowKey, PairedBy)>>,
    /// The song ([`song_key`]) each paired row went to.
    pub claimed: BTreeMap<RowKey, String>,
    /// Rows two songs wanted at one step: paired to neither.
    pub contested: BTreeSet<RowKey>,
}

impl Pairing {
    /// The rows `rel` paired to.
    pub fn rows_of(&self, rel: &str) -> &[(RowKey, PairedBy)] {
        self.by_file.get(rel).map_or(&[], Vec::as_slice)
    }

    /// One step's candidates (file, row) go in: a row is taken by one song
    /// only.
    fn step(
        &mut self,
        songs: &HashMap<&str, String>,
        candidates: Vec<(&str, RowKey)>,
        by: PairedBy,
    ) {
        let mut wanting: HashMap<&RowKey, BTreeSet<&str>> = HashMap::new();
        for (rel, row) in &candidates {
            if let Some(song) = songs.get(rel) {
                wanting.entry(row).or_default().insert(song.as_str());
            }
        }
        let fresh_contested: Vec<RowKey> = wanting
            .iter()
            .filter(|(row, wanted)| wanted.len() > 1 && !self.claimed.contains_key(**row))
            .map(|(row, _)| (*row).clone())
            .collect();
        self.contested.extend(fresh_contested);
        for (rel, row) in candidates {
            let Some(song) = songs.get(rel) else { continue };
            if self.contested.contains(&row) {
                continue;
            }
            match self.claimed.get(&row) {
                Some(owner) if owner != song => continue,
                Some(_) => {}
                None => {
                    self.claimed.insert(row.clone(), song.clone());
                }
            }
            let rows = self.by_file.entry(rel.to_owned()).or_default();
            if !rows.iter().any(|(known, _)| *known == row) {
                rows.push((row, by));
            }
        }
    }
}

/// The title before its first dash or bracket, folded: "Song - X Remix",
/// "Song (X Remix)" and "Song" are one.
pub fn base_title(title: &str) -> String {
    let cut = [" - ", "(", "["]
        .iter()
        .filter_map(|mark| title.find(mark))
        .min()
        .unwrap_or(title.len());
    spotify_match::normalize_title(&title[..cut])
}

fn lengths_agree(file: Option<f64>, row: Option<f64>) -> bool {
    matches!((file, row), (Some(a), Some(b)) if (a - b).abs() <= LENGTH_SLACK)
}

/// Pair the state's files to the catalogue's rows ([`PairedBy`] order).
pub fn pair(state: &State, catalogue: &Catalogue) -> Pairing {
    let songs: HashMap<&str, String> = state
        .files
        .iter()
        .filter_map(|(rel, file)| Some((rel.as_str(), song_key(file.tags.as_ref()?))))
        .collect();
    let rows: Vec<(RowKey, &Item, Option<&str>)> = catalogue
        .playlists
        .iter()
        .flat_map(|(id, cached)| {
            cached
                .items
                .iter()
                .flatten()
                .enumerate()
                .map(move |(at, item)| ((id.clone(), at), item, cached.folder.as_deref()))
        })
        .collect();
    let mut by_isrc: HashMap<String, Vec<&RowKey>> = HashMap::new();
    for (key, item, _) in &rows {
        if let Some(isrc) = item.isrc.as_deref().and_then(normalize_isrc) {
            by_isrc.entry(isrc).or_default().push(key);
        }
    }
    let mut pairing = Pairing::default();
    let tagged = || {
        state
            .files
            .iter()
            .filter_map(|(rel, file)| Some((rel.as_str(), file, file.tags.as_ref()?)))
    };

    let isrc: Vec<(&str, RowKey)> = tagged()
        .filter_map(|(rel, _, tags)| Some((rel, tags.isrc.as_deref().and_then(normalize_isrc)?)))
        .flat_map(|(rel, code)| {
            by_isrc
                .get(&code)
                .into_iter()
                .flatten()
                .map(move |key| (rel, (*key).clone()))
        })
        .collect();
    pairing.step(&songs, isrc, PairedBy::Isrc);

    let recorded: Vec<(&str, RowKey)> = tagged()
        .filter_map(|(rel, file, _)| Some((rel, file.playlist_match.as_ref()?)))
        .flat_map(|(rel, found)| {
            rows.iter()
                .filter(move |((id, _), item, _)| {
                    *id == found.playlist_id && item.id == found.track_id
                })
                .map(move |(key, _, _)| (rel, key.clone()))
        })
        .collect();
    pairing.step(&songs, recorded, PairedBy::Recorded);

    let by_name = |pairing: &Pairing, matches: &dyn Fn(&super::Tags, &Item) -> bool| {
        tagged()
            .filter(|(rel, _, _)| pairing.rows_of(rel).is_empty())
            .flat_map(|(rel, file, tags)| {
                rows.iter()
                    .filter(|(_, item, folder)| {
                        *folder == Some(file.playlist.as_str()) && matches(tags, item)
                    })
                    .map(move |(key, _, _)| (rel, key.clone()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let named = by_name(&pairing, &|tags, item| spotify_match::is_match(tags, item));
    pairing.step(&songs, named, PairedBy::Name);
    let based = by_name(&pairing, &|tags, item| {
        let title = base_title(&tags.title);
        lengths_agree(tags.seconds, item.seconds)
            && !title.is_empty()
            && title == base_title(&item.title)
            && spotify_match::artists_overlap(&tags.artist, &item.artists)
    });
    pairing.step(&songs, based, PairedBy::BaseName);
    pairing
}

/// The links one file's track gets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileLinks {
    pub spotify: Vec<String>,
    pub isrc: Vec<String>,
}

impl FileLinks {
    /// The source links, as the library stores them.
    pub fn source_links(&self) -> Vec<SourceLink> {
        let link = |provider: WantIdentityProvider, id: &String| SourceLink {
            provider: provider.as_str().to_owned(),
            entity_type: provider.entity_type().to_owned(),
            external_id: id.clone(),
            canonical_url: provider.canonical_url(id),
        };
        self.spotify
            .iter()
            .map(|id| link(WantIdentityProvider::Spotify, id))
            .chain(
                self.isrc
                    .iter()
                    .map(|id| link(WantIdentityProvider::Isrc, id)),
            )
            .collect()
    }
}

/// What `file` links: the Spotify ids of its rows, and their ISRCs, else
/// its own tag ISRC.
pub fn links_of(file: &FileState, rows: &[(RowKey, PairedBy)], catalogue: &Catalogue) -> FileLinks {
    let mut spotify = BTreeSet::new();
    let mut isrc = BTreeSet::new();
    for ((id, at), _) in rows {
        let Some(item) = catalogue
            .playlists
            .get(id)
            .and_then(|cached| cached.items.as_ref()?.get(*at))
        else {
            continue;
        };
        if let Some(track) = normalize_spotify_track(&item.id) {
            spotify.insert(track);
        }
        if let Some(code) = item.isrc.as_deref().and_then(normalize_isrc) {
            isrc.insert(code);
        }
    }
    if isrc.is_empty()
        && let Some(code) = file
            .tags
            .as_ref()
            .and_then(|tags| tags.isrc.as_deref())
            .and_then(normalize_isrc)
    {
        isrc.insert(code);
    }
    FileLinks {
        spotify: spotify.into_iter().collect(),
        isrc: isrc.into_iter().collect(),
    }
}

/// Whether the import handled `file`: placed it, found it owned, copied it
/// by its tags, or found it in the library.
fn handled(file: &FileState) -> bool {
    file.in_library
        || matches!(
            file.outcome.as_deref(),
            Some("imported" | "already_owned" | super::tags_fallback::OUTCOME)
        )
}

/// Where the library keeps one handled file: what to look its track up by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The source file's path, as the import recorded it on the track.
    pub source_path: String,
    /// The tag-only copy's path below its library folder.
    pub library_path: Option<String>,
    /// The song as tagged, for a file the library already held.
    pub held: Option<(String, String)>,
}

/// The handled files (a duplicate stands for its kept copy) and what to
/// look their tracks up by, under the source folder `source`.
pub fn asks(state: &State, source: &Path) -> BTreeMap<String, Ask> {
    let mut out = BTreeMap::new();
    for (rel, file) in &state.files {
        let kept_rel = file.duplicate_of.as_deref().unwrap_or(rel);
        let Some(kept) = state.files.get(kept_rel) else {
            continue;
        };
        if file.tags.is_none() || !handled(kept) {
            continue;
        }
        let held = (kept.in_library || kept.outcome.as_deref() == Some("already_owned"))
            .then_some(kept.tags.as_ref())
            .flatten()
            .map(|tags| (tags.artist.clone(), tags.title.clone()));
        out.insert(
            rel.clone(),
            Ask {
                source_path: source.join(kept_rel).to_string_lossy().into_owned(),
                library_path: kept.library_path.clone(),
                held,
            },
        );
    }
    out
}

/// Each asked file's indexed track, when the library has it.
///
/// # Errors
///
/// The library does not read.
pub fn tracks(
    conn: &Connection,
    asks: &BTreeMap<String, Ask>,
) -> rusqlite::Result<BTreeMap<String, String>> {
    let mut by_source: HashMap<String, String> = HashMap::new();
    let mut by_relative: HashMap<String, String> = HashMap::new();
    let mut statement = conn.prepare_cached(super::sql::INDEXED_TRACK_PATHS.text())?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        if let Some(source) = row.get::<_, Option<String>>(1)? {
            by_source.entry(source).or_insert_with(|| id.clone());
        }
        by_relative.entry(row.get(2)?).or_insert(id);
    }
    let mut out = BTreeMap::new();
    for (rel, ask) in asks {
        let mut track = by_source.get(&ask.source_path).cloned();
        if track.is_none() {
            track = ask
                .library_path
                .as_ref()
                .and_then(|path| by_relative.get(path).cloned());
        }
        if track.is_none()
            && let Some((artist, title)) = &ask.held
        {
            track = droppedneedle_library::identity::reads::held_copy(conn, artist, title)?;
        }
        if let Some(track) = track {
            out.insert(rel.clone(), track);
        }
    }
    Ok(out)
}

/// Write each track's links; ones it has stay as they are.
///
/// # Errors
///
/// The library does not write.
pub fn write(
    conn: &Connection,
    plan: &[(String, FileLinks)],
    now_secs: f64,
) -> rusqlite::Result<()> {
    for (track, links) in plan {
        droppedneedle_library::identity::link_track(conn, track, &links.source_links(), now_secs)?;
    }
    Ok(())
}

/// One playlist's link counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LinkCounts {
    pub by_spotify: usize,
    pub isrc_only: usize,
    pub no_link: usize,
}

/// A handled file that got no link, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unlinked {
    pub playlist: String,
    pub path: String,
    pub artist: String,
    pub title: String,
    pub why: String,
}

/// A CSV row no file paired to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnpairedRow {
    pub csv: String,
    pub artist: String,
    pub title: String,
    pub spotify_id: Option<String>,
    pub why: String,
}

/// The report's "link spotify ids" section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LinkReport {
    /// Handled files whose track has a Spotify id.
    pub by_spotify: usize,
    /// Handled files whose track has an ISRC and no Spotify id.
    pub isrc_only: usize,
    /// Handled files with no link (listed in [`Self::unlinked`]).
    pub no_link: usize,
    /// Of those, files whose copy the scan has not indexed yet (tried
    /// again on the next pass).
    pub waiting: usize,
    /// Files the import did not put in the library (not counted above).
    pub not_in_library: usize,
    pub playlists: BTreeMap<String, LinkCounts>,
    pub csv_rows: usize,
    pub rows_paired: usize,
    /// Rows paired, by the step that paired them.
    pub rows_by_step: BTreeMap<String, usize>,
    pub unlinked: Vec<Unlinked>,
    pub unpaired_rows: Vec<UnpairedRow>,
}

impl LinkReport {
    /// The readable part of the report.
    pub fn text(&self) -> String {
        let mut out = String::from("\nLink spotify ids (provider links on the library tracks):\n");
        out.push_str(&format!(
            "  {} files linked by Spotify id, {} by ISRC only, {} with no link ({} not indexed yet); {} files not in the library.\n",
            self.by_spotify, self.isrc_only, self.no_link, self.waiting, self.not_in_library
        ));
        out.push_str(&format!(
            "  CSV rows paired to a file: {} of {}",
            self.rows_paired, self.csv_rows
        ));
        let steps: Vec<String> = self
            .rows_by_step
            .iter()
            .map(|(step, count)| format!("{step}: {count}"))
            .collect();
        if !steps.is_empty() {
            out.push_str(&format!(" ({})", steps.join(", ")));
        }
        out.push('\n');
        out.push_str(&format!(
            "  {:<32} {:>8} {:>9} {:>8}\n",
            "playlist", "spotify", "isrc-only", "no-link"
        ));
        for (name, counts) in &self.playlists {
            out.push_str(&format!(
                "  {:<32} {:>8} {:>9} {:>8}\n",
                name.chars().take(32).collect::<String>(),
                counts.by_spotify,
                counts.isrc_only,
                counts.no_link
            ));
        }
        if !self.unlinked.is_empty() {
            out.push_str(&format!("\n  No link ({}):\n", self.unlinked.len()));
            for item in &self.unlinked {
                out.push_str(&format!(
                    "    [{}] {} - {} :: {} [{}]\n",
                    item.playlist, item.artist, item.title, item.path, item.why
                ));
            }
        }
        if !self.unpaired_rows.is_empty() {
            out.push_str(&format!(
                "\n  CSV rows no file paired to ({}):\n",
                self.unpaired_rows.len()
            ));
            for row in &self.unpaired_rows {
                out.push_str(&format!(
                    "    [{}] {} - {} ({}) [{}]\n",
                    row.csv,
                    row.artist,
                    row.title,
                    row.spotify_id.as_deref().unwrap_or("no id"),
                    row.why
                ));
            }
        }
        out
    }
}

/// How one handled file came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Spotify,
    Isrc,
    Waiting,
    None,
}

/// The step's whole answer: what to write and what to report.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Track and its links, one entry per track (links merged).
    pub writes: Vec<(String, FileLinks)>,
    pub report: LinkReport,
}

/// Plan the links: pair the files, find their tracks' links, and count.
pub fn plan(
    state: &State,
    catalogue: &Catalogue,
    asks: &BTreeMap<String, Ask>,
    tracks: &BTreeMap<String, String>,
) -> Plan {
    let pairing = pair(state, catalogue);
    let mapped: HashSet<&str> = catalogue
        .playlists
        .values()
        .filter_map(|cached| cached.folder.as_deref())
        .collect();
    let mut report = LinkReport::default();
    // First the links each track gets (copies of one file merge), then
    // each file counted by its track's links.
    let mut merged: BTreeMap<String, FileLinks> = BTreeMap::new();
    for (rel, file) in &state.files {
        let Some(track) = tracks.get(rel).filter(|_| file.tags.is_some()) else {
            continue;
        };
        let links = links_of(file, pairing.rows_of(rel), catalogue);
        let entry = merged.entry(track.clone()).or_default();
        for id in links.spotify {
            if !entry.spotify.contains(&id) {
                entry.spotify.push(id);
            }
        }
        for code in links.isrc {
            if !entry.isrc.contains(&code) {
                entry.isrc.push(code);
            }
        }
    }
    for (rel, file) in &state.files {
        let Some(tags) = &file.tags else { continue };
        let Some(ask) = asks.get(rel) else {
            report.not_in_library += 1;
            continue;
        };
        let links = tracks.get(rel).and_then(|track| merged.get(track));
        let (kind, why) = match links {
            None if ask.held.is_some() && ask.library_path.is_none() => (
                Kind::None,
                "the library copy it was found as is no longer indexed",
            ),
            None if ask.library_path.is_some() => (
                Kind::Waiting,
                "the scan has not indexed the tag-only copy yet (tried again later)",
            ),
            None => (
                Kind::Waiting,
                "no indexed track for the placed file yet (tried again later)",
            ),
            Some(links) if !links.spotify.is_empty() => (Kind::Spotify, ""),
            Some(links) if !links.isrc.is_empty() => (Kind::Isrc, ""),
            Some(_) if mapped.contains(file.playlist.as_str()) => {
                (Kind::None, "no CSV row pairs and the tags carry no ISRC")
            }
            Some(_) => (
                Kind::None,
                "no playlist CSV for this folder and the tags carry no ISRC",
            ),
        };
        let counts = report.playlists.entry(file.playlist.clone()).or_default();
        match kind {
            Kind::Spotify => {
                counts.by_spotify += 1;
                report.by_spotify += 1;
            }
            Kind::Isrc => {
                counts.isrc_only += 1;
                report.isrc_only += 1;
            }
            Kind::Waiting | Kind::None => {
                counts.no_link += 1;
                report.no_link += 1;
                if kind == Kind::Waiting {
                    report.waiting += 1;
                }
                report.unlinked.push(Unlinked {
                    playlist: file.playlist.clone(),
                    path: rel.clone(),
                    artist: tags.artist.clone(),
                    title: tags.title.clone(),
                    why: why.to_owned(),
                });
            }
        }
    }
    merged.retain(|_, links| !links.spotify.is_empty() || !links.isrc.is_empty());
    for (id, cached) in &catalogue.playlists {
        for (at, item) in cached.items.iter().flatten().enumerate() {
            report.csv_rows += 1;
            let key = (id.clone(), at);
            if pairing.claimed.contains_key(&key) {
                report.rows_paired += 1;
                continue;
            }
            let spotify_id = normalize_spotify_track(&item.id);
            let why = if spotify_id.is_none() {
                "a local file (no Spotify id)"
            } else if pairing.contested.contains(&key) {
                "two different songs pair to it; left to neither"
            } else if cached.folder.is_none() {
                "no folder maps to this CSV, and no file shares its ISRC"
            } else {
                "no file in the folder agrees on title, artist and length, nor shares its ISRC"
            };
            report.unpaired_rows.push(UnpairedRow {
                csv: id
                    .strip_prefix(super::csv_source::ID_PREFIX)
                    .unwrap_or(id)
                    .to_owned(),
                artist: item.artists.join("; "),
                title: item.title.clone(),
                spotify_id,
                why: why.to_owned(),
            });
        }
    }
    let mut steps: BTreeMap<PairedBy, BTreeSet<&RowKey>> = BTreeMap::new();
    for rows in pairing.by_file.values() {
        for (key, by) in rows {
            steps.entry(*by).or_default().insert(key);
        }
    }
    report.rows_by_step = steps
        .into_iter()
        .map(|(by, keys)| (by.label().to_owned(), keys.len()))
        .collect();
    Plan {
        writes: merged.into_iter().collect(),
        report,
    }
}

#[cfg(test)]
mod tests;
