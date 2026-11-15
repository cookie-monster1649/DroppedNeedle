//! The playlist import's report: counts per playlist, the files nothing
//! resolved, and what the import did.

use std::collections::BTreeMap;

use serde::Serialize;

use super::resolve::Via;
use super::{Mode, Resolution, State};

/// Counts for one playlist, or all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub files: usize,
    pub isrc_musicbrainz: usize,
    pub isrc_deezer: usize,
    pub barcode_musicbrainz: usize,
    pub name_musicbrainz: usize,
    pub name_deezer: usize,
    /// Fork: matched to a public Spotify playlist entry, then resolved.
    pub spotify_playlist: usize,
    /// Fork: matched to a row of the folder's playlist CSV, then resolved.
    #[serde(default)]
    pub playlist_csv: usize,
    /// Fork: matched to an entry, but the entry did not resolve either.
    pub matched_unresolved: usize,
    /// Deezer answers MusicBrainz knows (linked to the group).
    pub deezer_on_musicbrainz: usize,
    pub unresolved: usize,
    /// Not looked up yet (a run that stopped early).
    pub pending: usize,
    pub duplicates: usize,
    pub already_in_library: usize,
    pub unreadable: usize,
    pub imported: usize,
    pub import_already_owned: usize,
    pub import_unmatched: usize,
    pub import_held: usize,
    /// Fork: imported by their tags ([`super::tags_fallback`]).
    #[serde(default)]
    pub tags: usize,
    /// Fork: imported by their tags on an earlier run, resolved since; the
    /// tag-only copy is left as it is.
    #[serde(default)]
    pub tags_resolved_later: usize,
}

/// One file nothing resolved, or the import left out.
#[derive(Debug, Clone, Serialize)]
pub struct Listed {
    pub playlist: String,
    pub path: String,
    pub artist: String,
    pub title: String,
    pub album: String,
    /// Why it is listed.
    pub why: String,
}

/// The whole report.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub mode: Mode,
    /// Seconds since the epoch.
    pub generated_at: u64,
    /// Why the run stopped early, if it did.
    pub stopped: Option<String>,
    pub total: Counts,
    pub playlists: BTreeMap<String, Counts>,
    /// Distinct albums the resolved songs fall on.
    pub albums: usize,
    pub unresolved: Vec<Listed>,
    pub not_imported: Vec<Listed>,
    /// Fork: files imported by their tags that a later run resolved; left
    /// as they are.
    #[serde(default)]
    pub resolved_after_tags: Vec<Listed>,
    /// Fork: the public Spotify playlist match, when it is set.
    pub spotify: Option<SpotifySummary>,
    /// Fork: the "link spotify ids" step, when it ran
    /// ([`super::link_ids`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<super::link_ids::LinkReport>,
}

/// Fork: one folder's public playlist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PlaylistSide {
    /// The Spotify playlist id, `None` when no playlist maps to the folder.
    pub spotify_id: Option<String>,
    /// Entries read.
    pub entries: usize,
    /// Files resolved through the playlist.
    pub matched: usize,
    /// Files still unresolved.
    pub still_unresolved: usize,
}

/// Fork: what the public playlist match did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SpotifySummary {
    /// Fork: where the entries came from: `spotify` (the API) or
    /// `playlist_csv` (the playlist CSV folder).
    pub source: String,
    /// Why the step stopped short, when it did.
    pub note: Option<String>,
    /// Spotify calls the last step made.
    pub calls: u64,
    /// By folder.
    pub folders: BTreeMap<String, PlaylistSide>,
    /// Folders no playlist maps to.
    pub unmapped_folders: Vec<String>,
    /// Playlists no folder maps to, by name.
    pub unmapped_playlists: Vec<String>,
}

impl SpotifySummary {
    fn of(state: &State, playlists: &BTreeMap<String, Counts>) -> Option<Self> {
        let catalogue = state.spotify.as_ref()?;
        let csv = super::csv_source::is_csv(catalogue);
        let mut folders = BTreeMap::new();
        let mut unmapped_playlists = Vec::new();
        for (id, cached) in &catalogue.playlists {
            match &cached.folder {
                Some(folder) => {
                    folders.insert(
                        folder.clone(),
                        PlaylistSide {
                            spotify_id: Some(id.clone()),
                            entries: cached.items.as_ref().map_or(0, Vec::len),
                            ..PlaylistSide::default()
                        },
                    );
                }
                None => unmapped_playlists.push(cached.name.clone()),
            }
        }
        let mut unmapped_folders = Vec::new();
        for (folder, counts) in playlists {
            if !folders.contains_key(folder) {
                unmapped_folders.push(folder.clone());
            }
            let side = folders.entry(folder.clone()).or_default();
            side.matched = if csv {
                counts.playlist_csv
            } else {
                counts.spotify_playlist
            };
            side.still_unresolved = counts.unresolved;
        }
        Some(Self {
            source: if csv { "playlist_csv" } else { "spotify" }.to_owned(),
            note: catalogue.note.clone(),
            calls: catalogue.calls,
            folders,
            unmapped_folders,
            unmapped_playlists,
        })
    }
}

fn add(counts: &mut Counts, f: impl Fn(&mut Counts)) {
    f(counts);
}

impl Report {
    /// The report of `state`.
    pub fn of(state: &State, mode: Mode, stopped: Option<&str>, now: u64) -> Self {
        let mut playlists: BTreeMap<String, Counts> = BTreeMap::new();
        let mut unresolved = Vec::new();
        let mut not_imported = Vec::new();
        let mut resolved_after_tags = Vec::new();
        let mut albums = std::collections::BTreeSet::new();
        for (rel, file) in &state.files {
            let counts = playlists.entry(file.playlist.clone()).or_default();
            counts.files += 1;
            let listed = |why: &str| {
                let tags = file.tags.clone().unwrap_or_default();
                Listed {
                    playlist: file.playlist.clone(),
                    path: rel.clone(),
                    artist: tags.artist,
                    title: tags.title,
                    album: tags.album,
                    why: why.to_owned(),
                }
            };
            if file.tags.is_none() {
                counts.unreadable += 1;
                unresolved.push(listed("tags do not read"));
                continue;
            }
            if file.duplicate_of.is_some() {
                counts.duplicates += 1;
                continue;
            }
            if file.in_library && !super::settled(file.outcome.as_deref()) {
                counts.already_in_library += 1;
                continue;
            }
            let tags_only = super::tags_fallback::is_tags_only(file.outcome.as_deref());
            let where_tags = || {
                format!(
                    "imported by tags (via tags_only) at {}",
                    file.library_path.as_deref().unwrap_or("?")
                )
            };
            match &file.resolved {
                None => counts.pending += 1,
                Some(Resolution::Found(_))
                    if tags_only && super::tags_fallback::was_unresolved(file) =>
                {
                    counts.tags += 1;
                    counts.tags_resolved_later += 1;
                    resolved_after_tags.push(listed(&format!(
                        "resolved since; left as is: {}",
                        where_tags()
                    )));
                    continue;
                }
                Some(Resolution::NotFound | Resolution::Refused { .. }) if tags_only => {
                    counts.unresolved += 1;
                    counts.tags += 1;
                    unresolved.push(listed(&where_tags()));
                    continue;
                }
                Some(Resolution::NotFound) => {
                    counts.unresolved += 1;
                    if file.playlist_match.is_some() {
                        counts.matched_unresolved += 1;
                        unresolved.push(listed("playlist entry found; it did not resolve"));
                    } else {
                        unresolved.push(listed("no confident match"));
                    }
                }
                Some(Resolution::Refused { reason, .. }) => {
                    counts.unresolved += 1;
                    unresolved.push(listed(&format!("lookup refused: {reason}")));
                }
                Some(Resolution::Found(found)) => {
                    albums.insert(found.album.clone());
                    match found.via {
                        Via::IsrcMusicbrainz => counts.isrc_musicbrainz += 1,
                        Via::IsrcDeezer => counts.isrc_deezer += 1,
                        Via::BarcodeMusicbrainz => counts.barcode_musicbrainz += 1,
                        Via::NameMusicbrainz => counts.name_musicbrainz += 1,
                        Via::NameDeezer => counts.name_deezer += 1,
                        Via::SpotifyPlaylist => counts.spotify_playlist += 1,
                        Via::PlaylistCsv => counts.playlist_csv += 1,
                    }
                    if found.stand_in.is_some()
                        && !droppedneedle_providers::virtual_ids::is_virtual(&found.album)
                    {
                        counts.deezer_on_musicbrainz += 1;
                    }
                }
            }
            match file.outcome.as_deref() {
                None => {}
                Some("imported") => counts.imported += 1,
                Some("already_owned") => counts.import_already_owned += 1,
                Some(super::tags_fallback::OUTCOME) => counts.tags += 1,
                Some(other) if other.starts_with("held:") => {
                    counts.import_held += 1;
                    not_imported.push(listed(other));
                }
                Some(other) => {
                    counts.import_unmatched += 1;
                    not_imported.push(listed(other));
                }
            }
        }
        let mut total = Counts::default();
        for counts in playlists.values() {
            add(&mut total, |t| {
                t.files += counts.files;
                t.isrc_musicbrainz += counts.isrc_musicbrainz;
                t.isrc_deezer += counts.isrc_deezer;
                t.barcode_musicbrainz += counts.barcode_musicbrainz;
                t.name_musicbrainz += counts.name_musicbrainz;
                t.name_deezer += counts.name_deezer;
                t.spotify_playlist += counts.spotify_playlist;
                t.playlist_csv += counts.playlist_csv;
                t.matched_unresolved += counts.matched_unresolved;
                t.deezer_on_musicbrainz += counts.deezer_on_musicbrainz;
                t.unresolved += counts.unresolved;
                t.pending += counts.pending;
                t.duplicates += counts.duplicates;
                t.already_in_library += counts.already_in_library;
                t.unreadable += counts.unreadable;
                t.imported += counts.imported;
                t.import_already_owned += counts.import_already_owned;
                t.import_unmatched += counts.import_unmatched;
                t.import_held += counts.import_held;
                t.tags += counts.tags;
                t.tags_resolved_later += counts.tags_resolved_later;
            });
        }
        let spotify = SpotifySummary::of(state, &playlists);
        Self {
            mode,
            generated_at: now,
            spotify,
            stopped: stopped.map(str::to_owned),
            total,
            playlists,
            albums: albums.len(),
            unresolved,
            not_imported,
            resolved_after_tags,
            links: None,
        }
    }

    /// One line for the status.
    pub fn headline(&self) -> String {
        let t = &self.total;
        let resolved = t.isrc_musicbrainz
            + t.isrc_deezer
            + t.barcode_musicbrainz
            + t.name_musicbrainz
            + t.name_deezer
            + t.spotify_playlist
            + t.playlist_csv;
        format!(
            "{} files: {resolved} resolved on {} albums, {} unresolved, {} duplicates, {} already in the library, {} imported, {} imported by tags",
            t.files,
            self.albums,
            t.unresolved,
            t.duplicates,
            t.already_in_library,
            t.imported,
            t.tags
        )
    }

    /// The readable summary.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let mode = match self.mode {
            Mode::DryRun => "dry run",
            Mode::Import => "import",
            Mode::LinkIds => "import, provider links again",
        };
        out.push_str(&format!("Playlist-folder import ({mode})\n"));
        if let Some(stopped) = &self.stopped {
            out.push_str(&format!("STOPPED EARLY: {stopped}\n"));
        }
        out.push_str(&format!("{}\n\n", self.headline()));
        let header = format!(
            "{:<32} {:>6} {:>8} {:>8} {:>8} {:>7} {:>7} {:>7} {:>7} {:>6} {:>6} {:>5} {:>6} {:>8} {:>9} {:>5}\n",
            "playlist",
            "files",
            "isrc-mb",
            "isrc-dz",
            "barcode",
            "name-mb",
            "name-dz",
            "sp-list",
            "pl-csv",
            "unres",
            "dupes",
            "inlib",
            "unread",
            "imported",
            "not-in",
            "tags"
        );
        out.push_str(&header);
        let row = |name: &str, c: &Counts| {
            format!(
                "{:<32} {:>6} {:>8} {:>8} {:>8} {:>7} {:>7} {:>7} {:>7} {:>6} {:>6} {:>5} {:>6} {:>8} {:>9} {:>5}\n",
                name.chars().take(32).collect::<String>(),
                c.files,
                c.isrc_musicbrainz,
                c.isrc_deezer,
                c.barcode_musicbrainz,
                c.name_musicbrainz,
                c.name_deezer,
                c.spotify_playlist,
                c.playlist_csv,
                c.unresolved,
                c.duplicates,
                c.already_in_library,
                c.unreadable,
                c.imported + c.import_already_owned,
                c.import_unmatched + c.import_held,
                c.tags,
            )
        };
        for (name, counts) in &self.playlists {
            out.push_str(&row(name, counts));
        }
        out.push_str(&row("TOTAL", &self.total));
        out.push_str(&format!(
            "\nDeezer answers MusicBrainz knows: {}. Not looked up yet: {}.\n",
            self.total.deezer_on_musicbrainz, self.total.pending
        ));
        if let Some(spotify) = &self.spotify {
            out.push_str(&spotify.text());
        }
        if let Some(links) = &self.links {
            out.push_str(&links.text());
        }
        if !self.unresolved.is_empty() {
            out.push_str(&format!("\nUnresolved ({}):\n", self.unresolved.len()));
            for item in &self.unresolved {
                out.push_str(&format!(
                    "  [{}] {} - {} ({}) :: {} [{}]\n",
                    item.playlist, item.artist, item.title, item.album, item.path, item.why
                ));
            }
        }
        if !self.not_imported.is_empty() {
            out.push_str(&format!("\nNot imported ({}):\n", self.not_imported.len()));
            for item in &self.not_imported {
                out.push_str(&format!(
                    "  [{}] {} - {} :: {} [{}]\n",
                    item.playlist, item.artist, item.title, item.path, item.why
                ));
            }
        }
        if !self.resolved_after_tags.is_empty() {
            out.push_str(&format!(
                "\nImported by tags, resolved since (left as is; match them in the library) ({}):\n",
                self.resolved_after_tags.len()
            ));
            for item in &self.resolved_after_tags {
                out.push_str(&format!(
                    "  [{}] {} - {} :: {} [{}]\n",
                    item.playlist, item.artist, item.title, item.path, item.why
                ));
            }
        }
        out
    }
}

impl SpotifySummary {
    /// The readable part of the report.
    pub fn text(&self) -> String {
        let csv = self.source == "playlist_csv";
        let mut out = String::from(if csv {
            "\nPlaylist CSV match (via playlist_csv; the Spotify API is not used):\n"
        } else {
            "\nPublic Spotify playlist match:\n"
        });
        if let Some(note) = &self.note {
            out.push_str(&format!("  NOTE: {note}\n"));
        }
        if !csv {
            out.push_str(&format!("  Spotify calls this run: {}\n", self.calls));
        }
        out.push_str(&format!(
            "  {:<32} {:>8} {:>8} {:>8}  {}\n",
            "folder",
            "entries",
            "matched",
            "unres",
            if csv { "csv file" } else { "playlist" }
        ));
        for (folder, side) in &self.folders {
            out.push_str(&format!(
                "  {:<32} {:>8} {:>8} {:>8}  {}\n",
                folder.chars().take(32).collect::<String>(),
                side.entries,
                side.matched,
                side.still_unresolved,
                side.spotify_id
                    .as_deref()
                    .map(|id| id
                        .strip_prefix(super::csv_source::ID_PREFIX)
                        .map_or_else(|| id.to_owned(), |name| format!("{name}.csv")))
                    .unwrap_or_else(|| "UNMAPPED".to_owned()),
            ));
        }
        if !self.unmapped_folders.is_empty() {
            out.push_str(&format!(
                "  Folders with no {}: {}\n",
                if csv { "CSV file" } else { "playlist" },
                self.unmapped_folders.join(", ")
            ));
        }
        if !self.unmapped_playlists.is_empty() {
            out.push_str(&format!(
                "  {} with no folder: {}\n",
                if csv { "CSV files" } else { "Playlists" },
                self.unmapped_playlists.join(", ")
            ));
        }
        out
    }
}
