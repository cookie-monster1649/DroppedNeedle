//! Fork: the playlist-folder import. A folder of exported playlists (one
//! top-level folder per playlist, any folders below, tagged audio files)
//! goes into the shared library, each song on the album the same resolver
//! a Spotify playlist sync uses names ([`resolve`]), so a later sync of
//! those playlists finds the songs already there instead of asking for
//! them again.
//!
//! - The source is the `PLAYLIST_IMPORT_PATH` environment variable (a
//!   folder inside the container); unset means off. Reports and the run's
//!   state go to `PLAYLIST_IMPORT_REPORTS`, else `playlist-import` beside
//!   the database.
//! - A dry run reads every file's tags, sets duplicates aside (the same
//!   song in two playlists: its ISRC, else artist, title and length), checks
//!   the library, resolves the rest and writes `report.json` and
//!   `report.txt`. Nothing in the library changes.
//! - The real run does the same, links each Deezer album MusicBrainz knows
//!   to its group, then imports the songs album by album through the
//!   normal intake ([`crate::intake::place`]), as a person's album choice:
//!   the files are copied, the source is never changed, and no quality
//!   rule of the download profile applies (the intake has none; the
//!   global settings are untouched). A file that does not pair stays out
//!   and is listed; nothing goes to the held list.
//! - The state file keeps each file's tags, resolution and import outcome,
//!   so a run stopped by a restart or an outage picks up where it was and
//!   a second run imports nothing twice.

pub mod http;
pub mod report;
pub mod resolve;
pub mod walk;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use self::report::Report;
use self::resolve::Resolved;
use crate::db::AcquireDb;
use crate::drop_import::identify::{self, Outcome as Identified};
use crate::imports::spotify_resolve::Live;
use crate::intake;
use crate::landing::ports::LandingLibrary;
use crate::landing::probe::probe_beneath;
use droppedneedle_kernel::db::fold::fold_text;
use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_library::sources::links::{self, LinkVia};
use droppedneedle_library::tags::TrackFile;
use droppedneedle_providers::virtual_ids::is_virtual;

/// The source folder's environment variable.
pub const SOURCE_ENV: &str = "PLAYLIST_IMPORT_PATH";
/// The report folder's environment variable.
pub const REPORTS_ENV: &str = "PLAYLIST_IMPORT_REPORTS";
/// Files whose tags are read in one blocking step.
const READ_BATCH: usize = 50;
/// The state is written after this many lookups.
const SAVE_EVERY: usize = 25;
/// A pause between two songs' lookups, beside the providers' own limits.
const LOOKUP_GAP: Duration = Duration::from_millis(250);

/// What a file's tags say.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: Option<String>,
    pub isrc: Option<String>,
    pub barcode: Option<String>,
    pub seconds: Option<f64>,
    pub bitrate: Option<u32>,
}

/// One source file in the state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileState {
    pub playlist: String,
    pub size: u64,
    pub modified: i64,
    /// `None`: the tags do not read.
    pub tags: Option<Tags>,
    /// The path (below the source) of the copy this file duplicates.
    pub duplicate_of: Option<String>,
    /// The library already held the song when it was checked.
    pub in_library: bool,
    /// The resolution; `None` until looked up.
    pub resolved: Option<Resolution>,
    /// What the import did: `imported`, `already_owned`, `unmatched`,
    /// `held:<reason>`.
    pub outcome: Option<String>,
}

/// A lookup's answer for a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolution {
    Found(Resolved),
    NotFound,
}

/// The run state, kept in `state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    /// By path below the source.
    pub files: BTreeMap<String, FileState>,
}

/// Which run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    DryRun,
    Import,
}

/// What the import is doing.
#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
pub struct Progress {
    /// A run is going.
    pub running: bool,
    pub mode: Option<Mode>,
    /// `walking`, `reading`, `resolving`, `importing`, `done`, `failed`.
    pub step: String,
    pub done: usize,
    pub total: usize,
    /// How the last run ended.
    pub last: Option<String>,
    /// Where the reports are written.
    pub reports: String,
    /// Whether a source folder is set.
    pub configured: bool,
}

/// Why a run did not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("no source folder is set ({SOURCE_ENV})")]
    Off,
    #[error("a playlist import is already running")]
    Running,
}

/// The playlist-folder import.
pub struct PlaylistImport {
    db: AcquireDb,
    lookups: Arc<Live>,
    library: Arc<dyn LandingLibrary>,
    source: Option<PathBuf>,
    reports: PathBuf,
    progress: Mutex<Progress>,
}

impl PlaylistImport {
    /// The import over the environment's folders.
    pub fn new(db: AcquireDb, lookups: Arc<Live>, library: Arc<dyn LandingLibrary>) -> Self {
        let source = std::env::var_os(SOURCE_ENV)
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty());
        let reports = std::env::var_os(REPORTS_ENV)
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| {
                db.path()
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("playlist-import")
            });
        Self::with_folders(db, lookups, library, source, reports)
    }

    /// The import over the folders given.
    pub fn with_folders(
        db: AcquireDb,
        lookups: Arc<Live>,
        library: Arc<dyn LandingLibrary>,
        source: Option<PathBuf>,
        reports: PathBuf,
    ) -> Self {
        let progress = Progress {
            reports: reports.display().to_string(),
            configured: source.is_some(),
            ..Progress::default()
        };
        Self {
            db,
            lookups,
            library,
            source,
            reports,
            progress: Mutex::new(progress),
        }
    }

    /// What the import is doing.
    pub fn progress(&self) -> Progress {
        self.progress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn set(&self, step: &str, done: usize, total: usize) {
        let mut progress = self.progress.lock().unwrap_or_else(|p| p.into_inner());
        progress.step = step.to_owned();
        progress.done = done;
        progress.total = total;
    }

    /// Start a run in the background.
    ///
    /// # Errors
    ///
    /// No source is set, or a run is going.
    pub fn start(self: &Arc<Self>, mode: Mode) -> Result<(), StartError> {
        let Some(source) = self.source.clone() else {
            return Err(StartError::Off);
        };
        {
            let mut progress = self.progress.lock().unwrap_or_else(|p| p.into_inner());
            if progress.running {
                return Err(StartError::Running);
            }
            progress.running = true;
            progress.mode = Some(mode);
            progress.step = "walking".to_owned();
        }
        let this = self.clone();
        tokio::spawn(async move {
            let ended = match this.run(&source, mode).await {
                Ok(text) => text,
                Err(error) => {
                    tracing::warn!(%error, "playlist import stopped");
                    format!("stopped: {error}")
                }
            };
            let mut progress = this.progress.lock().unwrap_or_else(|p| p.into_inner());
            progress.running = false;
            progress.step = if ended.starts_with("stopped") {
                "failed".to_owned()
            } else {
                "done".to_owned()
            };
            progress.last = Some(ended);
        });
        Ok(())
    }

    fn state_path(&self) -> PathBuf {
        self.reports.join("state.json")
    }

    async fn load(&self) -> State {
        let path = self.state_path();
        tokio::task::spawn_blocking(move || {
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    async fn save(&self, state: &State) -> Result<(), String> {
        let (dir, path) = (self.reports.clone(), self.state_path());
        let bytes = serde_json::to_vec(state).map_err(|error| error.to_string())?;
        tokio::task::spawn_blocking(move || write_atomic(&dir, &path, &bytes))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
    }

    /// One run; answers how it ended, for the status.
    async fn run(&self, source: &Path, mode: Mode) -> Result<String, String> {
        let root = source.to_path_buf();
        let found = tokio::task::spawn_blocking(move || walk::walk(&root))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("the source folder does not list: {error}"))?;
        let mut state = self.load().await;
        // Files gone from the source leave the state; changed files are
        // read again.
        let present: std::collections::HashSet<&str> =
            found.iter().map(|file| file.rel.as_str()).collect();
        state.files.retain(|rel, _| present.contains(rel.as_str()));
        let mut to_read: Vec<walk::Found> = Vec::new();
        for file in &found {
            let fresh = state.files.get(&file.rel).is_some_and(|known| {
                known.size == file.size && known.modified == file.modified && known.tags.is_some()
            });
            if !fresh {
                state.files.insert(
                    file.rel.clone(),
                    FileState {
                        playlist: file.playlist.clone(),
                        size: file.size,
                        modified: file.modified,
                        ..FileState::default()
                    },
                );
                to_read.push(file.clone());
            } else if let Some(known) = state.files.get_mut(&file.rel) {
                known.playlist = file.playlist.clone();
            }
        }
        let total = to_read.len();
        for (index, batch) in to_read.chunks(READ_BATCH).enumerate() {
            self.set("reading", index * READ_BATCH, total);
            let (root, batch) = (source.to_path_buf(), batch.to_vec());
            let read = tokio::task::spawn_blocking(move || read_tags(&root, &batch))
                .await
                .map_err(|error| error.to_string())?;
            for (rel, tags) in read {
                if let Some(file) = state.files.get_mut(&rel) {
                    file.tags = tags;
                }
            }
        }
        mark_duplicates(&mut state);
        self.check_library(&mut state).await?;
        self.save(&state).await?;
        let stopped = self.resolve_all(&mut state, mode).await;
        self.save(&state).await?;
        if let Err(error) = stopped {
            self.write_report(&state, mode, Some(&error)).await?;
            return Err(error);
        }
        if mode == Mode::Import {
            let stopped = self.import_all(source, &mut state).await;
            self.save(&state).await?;
            if let Err(error) = stopped {
                self.write_report(&state, mode, Some(&error)).await?;
                return Err(error);
            }
        }
        let report = self.write_report(&state, mode, None).await?;
        Ok(report.headline())
    }

    /// Mark the songs the library already holds (only songs not imported
    /// by this run).
    async fn check_library(&self, state: &mut State) -> Result<(), String> {
        let asks: Vec<(String, String, String)> = state
            .files
            .iter()
            .filter(|(_, file)| file.duplicate_of.is_none() && file.outcome.is_none())
            .filter_map(|(rel, file)| {
                let tags = file.tags.as_ref()?;
                Some((rel.clone(), tags.artist.clone(), tags.title.clone()))
            })
            .collect();
        let held: Vec<String> = self
            .db
            .db()
            .read("playlist_import.in_library", move |conn| {
                let mut out = Vec::new();
                for (rel, artist, title) in &asks {
                    if droppedneedle_library::identity::reads::song_held(conn, artist, title)? {
                        out.push(rel.clone());
                    }
                }
                Ok::<_, DbError>(out)
            })
            .await
            .map_err(|error| error.to_string())?;
        for file in state.files.values_mut() {
            file.in_library = false;
        }
        for rel in held {
            if let Some(file) = state.files.get_mut(&rel) {
                file.in_library = true;
            }
        }
        Ok(())
    }

    /// Resolve every song not yet looked up. A service that fails stops
    /// the pass; what was found stays in the state.
    async fn resolve_all(&self, state: &mut State, mode: Mode) -> Result<(), String> {
        let todo: Vec<String> = state
            .files
            .iter()
            .filter(|(_, file)| {
                file.duplicate_of.is_none()
                    && !file.in_library
                    && file.outcome.is_none()
                    && file.tags.is_some()
                    && file.resolved.is_none()
            })
            .map(|(rel, _)| rel.clone())
            .collect();
        let total = todo.len();
        for (index, rel) in todo.iter().enumerate() {
            self.set("resolving", index, total);
            let Some(tags) = state.files.get(rel).and_then(|file| file.tags.clone()) else {
                continue;
            };
            let facts = resolve::facts(
                &tags.title,
                &tags.artist,
                &tags.album,
                tags.album_artist.as_deref(),
                tags.isrc.as_deref(),
                tags.barcode.as_deref(),
                tags.seconds,
            );
            let answer = resolve::resolve(&facts, self.lookups.as_ref(), self.lookups.as_ref())
                .await
                .map_err(|error| format!("a lookup failed (run again to go on): {error}"))?;
            if let Some(file) = state.files.get_mut(rel) {
                file.resolved = Some(match answer {
                    Some(found) => Resolution::Found(found),
                    None => Resolution::NotFound,
                });
            }
            if (index + 1) % SAVE_EVERY == 0 {
                self.save(state).await?;
            }
            tokio::time::sleep(LOOKUP_GAP).await;
        }
        if mode == Mode::Import {
            self.link_found(state).await?;
        }
        Ok(())
    }

    /// Link each Deezer album found to be a MusicBrainz group, so its page,
    /// requests and a later playlist sync follow the group.
    async fn link_found(&self, state: &State) -> Result<(), String> {
        let pairs: Vec<(String, String)> = state
            .files
            .values()
            .filter_map(|file| match &file.resolved {
                Some(Resolution::Found(found)) => found
                    .stand_in
                    .as_ref()
                    .filter(|_| !is_virtual(&found.album))
                    .map(|stand_in| (stand_in.clone(), found.album.clone())),
                _ => None,
            })
            .collect();
        if pairs.is_empty() {
            return Ok(());
        }
        let now_ms = crate::db::to_i64(self.db.now_epoch()).saturating_mul(1000);
        self.db
            .db()
            .write(Lane::Background, "playlist_import.link", move |tx| {
                for (stand_in, group) in &pairs {
                    if links::link(tx, stand_in, group, LinkVia::Barcode, now_ms)? {
                        crate::stand_ins::settle_linked(tx, stand_in, group, now_ms)?;
                    }
                }
                Ok::<_, DbError>(())
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// Import the resolved songs, album by album.
    async fn import_all(&self, source: &Path, state: &mut State) -> Result<(), String> {
        let mut albums: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (rel, file) in &state.files {
            if file.duplicate_of.is_some() || file.in_library || file.outcome.is_some() {
                continue;
            }
            if let Some(Resolution::Found(found)) = &file.resolved {
                albums
                    .entry(found.album.to_ascii_lowercase())
                    .or_default()
                    .push(rel.clone());
            }
        }
        let total = albums.len();
        for (index, (album, rels)) in albums.into_iter().enumerate() {
            self.set("importing", index, total);
            let outcomes = self.import_album(source, &album, &rels).await?;
            for (rel, outcome) in outcomes {
                if let Some(file) = state.files.get_mut(&rel) {
                    file.outcome = Some(outcome);
                }
            }
            if (index + 1) % 10 == 0 {
                self.save(state).await?;
            }
        }
        Ok(())
    }

    /// Place one album's files. Answers each file's outcome; an outage
    /// stops the run (the album is tried again on the next).
    async fn import_album(
        &self,
        source: &Path,
        album: &str,
        rels: &[String],
    ) -> Result<Vec<(String, String)>, String> {
        let paths: Vec<PathBuf> = rels.iter().map(|rel| source.join(rel)).collect();
        let beneath = source.to_path_buf();
        let landing =
            tokio::task::spawn_blocking(move || probe_beneath(&paths, &[], Some(&beneath)))
                .await
                .map_err(|error| error.to_string())?;
        let rel_of = |path: &Path| {
            path.strip_prefix(source)
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default()
        };
        let mut out: Vec<(String, String)> = landing
            .unreadable
            .iter()
            .chain(landing.samples.iter())
            .chain(landing.missing.iter())
            .map(|path| (rel_of(path), "unreadable".to_owned()))
            .collect();
        if landing.audio.is_empty() {
            return Ok(out);
        }
        let summary = match identify::assigned(self.library.as_ref(), &landing, album, None).await {
            Identified::Matched(summary) => summary,
            Identified::Review { reason, .. } => {
                out.extend(
                    landing
                        .audio
                        .iter()
                        .map(|file| (rel_of(&file.path), format!("unmatched:{}", reason.code()))),
                );
                return Ok(out);
            }
            Identified::Unavailable(detail) => {
                return Err(format!(
                    "MusicBrainz unavailable (run again to go on): {detail}"
                ));
            }
            Identified::AcoustidUnavailable => {
                return Err("AcoustID unavailable (run again to go on)".to_owned());
            }
            Identified::Busy | Identified::Closed => {
                return Err("the database is busy or closing (run again to go on)".to_owned());
            }
        };
        let Some((_, _, pairing)) =
            crate::drop_import::album_pairing(&summary, landing.audio.len(), true, false)
        else {
            out.extend(
                landing
                    .audio
                    .iter()
                    .map(|file| (rel_of(&file.path), "unmatched:no_album_match".to_owned())),
            );
            return Ok(out);
        };
        let Some(found) = summary.best() else {
            return Ok(out);
        };
        let key = album_key(album);
        let batch = intake::IntakeBatch {
            files: landing
                .audio
                .iter()
                .map(|file| intake::IntakeFile {
                    file: file.clone(),
                    held_for: None,
                })
                .collect(),
            target: intake::Target {
                release: found.release.clone(),
                join: None,
                asked: Vec::new(),
            },
            origin: intake::Origin::PlaylistFolder {
                key,
                folders: vec![source.to_path_buf()],
            },
            gate: intake::Gate::PersonChoseAlbum,
            owner: None,
            sidecars: Vec::new(),
        };
        let placed = match intake::place(self.library.as_ref(), &batch, pairing, None).await {
            Ok(placed) => placed,
            Err(deferred) if deferred.outage => {
                return Err(format!(
                    "the library put the import off (run again to go on): {}",
                    deferred.detail
                ));
            }
            // One album the library will not take is that album's outcome;
            // the run goes on.
            Err(deferred) => {
                tracing::warn!(album, detail = %deferred.detail, "playlist import: album refused");
                let why = format!("refused:{}", deferred.reason.code());
                out.extend(
                    landing
                        .audio
                        .iter()
                        .map(|file| (rel_of(&file.path), why.clone())),
                );
                return Ok(out);
            }
        };
        for (index, file) in landing.audio.iter().enumerate() {
            let outcome = match placed.outcomes.get(index) {
                Some(intake::Outcome::Imported { .. }) => "imported".to_owned(),
                Some(intake::Outcome::AlreadyOwned { .. }) => "already_owned".to_owned(),
                Some(intake::Outcome::Held { reason, .. }) => format!("held:{}", reason.code()),
                Some(intake::Outcome::Untouched) | None => "unmatched:not_paired".to_owned(),
            };
            out.push((rel_of(&file.path), outcome));
        }
        tracing::info!(
            album,
            files = landing.audio.len(),
            "playlist import: album placed"
        );
        Ok(out)
    }

    async fn write_report(
        &self,
        state: &State,
        mode: Mode,
        stopped: Option<&str>,
    ) -> Result<Report, String> {
        let report = Report::of(state, mode, stopped, self.db.now_epoch());
        let dir = self.reports.clone();
        let json = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
        let text = report.text();
        let name = match mode {
            Mode::DryRun => "dry-run",
            Mode::Import => "import",
        };
        tokio::task::spawn_blocking(move || {
            write_atomic(&dir, &dir.join(format!("{name}-report.json")), &json)?;
            write_atomic(
                &dir,
                &dir.join(format!("{name}-report.txt")),
                text.as_bytes(),
            )
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
        Ok(report)
    }
}

/// A short, path-safe key for an album's import staging.
fn album_key(album: &str) -> String {
    album
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(48)
        .collect()
}

fn write_atomic(dir: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)
}

/// Read each file's tags beneath `root`. Blocking.
fn read_tags(root: &Path, files: &[walk::Found]) -> Vec<(String, Option<Tags>)> {
    files
        .iter()
        .map(|file| {
            let landing = probe_beneath(std::slice::from_ref(&file.path), &[], Some(root));
            let tags = landing.audio.first().map(|audio| {
                let (isrc, barcode) = droppedneedle_library::tags::read::read_identifiers(
                    TrackFile::beneath_real(root, &audio.path),
                );
                Tags {
                    title: audio.tag.title.clone(),
                    artist: audio.tag.artist.clone(),
                    album: audio.tag.album.clone(),
                    album_artist: audio.tag.album_artist.clone(),
                    isrc,
                    barcode: barcode.or_else(|| audio.tag.edition.barcode.clone()),
                    seconds: audio.header.duration_seconds,
                    bitrate: audio.header.bitrate_kbps,
                }
            });
            (file.rel.clone(), tags)
        })
        .collect()
}

/// The key two copies of one song share: the ISRC, else artist, title
/// and length to the nearest three seconds.
pub fn song_key(tags: &Tags) -> String {
    if let Some(isrc) = tags.isrc.as_deref().filter(|code| code.len() >= 12) {
        return format!("isrc:{}", isrc.replace('-', "").to_ascii_uppercase());
    }
    let length = tags.seconds.map_or(-1, |s| (s / 3.0).round() as i64);
    format!(
        "name:{}|{}|{length}",
        fold_text(&tags.artist),
        fold_text(&tags.title)
    )
}

/// Set each song's copies but the best (highest bitrate, then first path)
/// aside as duplicates of it.
pub fn mark_duplicates(state: &mut State) {
    let mut best: HashMap<String, (String, u32)> = HashMap::new();
    for (rel, file) in &state.files {
        let Some(tags) = &file.tags else { continue };
        let key = song_key(tags);
        let rate = tags.bitrate.unwrap_or(0);
        // An imported copy stays the one kept.
        let rate = if file.outcome.is_some() {
            u32::MAX
        } else {
            rate
        };
        match best.get(&key) {
            Some((_, kept)) if *kept >= rate => {}
            _ => {
                best.insert(key, (rel.clone(), rate));
            }
        }
    }
    for (rel, file) in state.files.iter_mut() {
        file.duplicate_of = file.tags.as_ref().and_then(|tags| {
            let (kept, _) = best.get(&song_key(tags))?;
            (kept != rel).then(|| kept.clone())
        });
    }
}

#[cfg(test)]
mod tests;
