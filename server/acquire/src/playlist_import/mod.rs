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
//! - Fork: last, every file still left (nothing resolved it, or it did
//!   not pair) is copied into the library as it is tagged and left to the
//!   scan, which groups it by its album artist and album and flags it
//!   unmatched when identification finds nothing ([`tags_fallback`]; on
//!   unless `PLAYLIST_IMPORT_TAGS_FALLBACK=false`).
//! - Fork: last of all, every file the import put in the library (or found
//!   there) gets provider links on its track, its Spotify id from the
//!   playlist CSV row it pairs to and its ISRC, so a later Spotify playlist
//!   sync links each entry straight to the file ([`link_ids`]). The step
//!   also runs on its own: on a loop after start (over the state an earlier
//!   import left, and again while tag-only copies wait for the scan) and
//!   when an admin asks for it ([`Mode::LinkIds`]).
//! - The state file keeps each file's tags, resolution and import outcome,
//!   so a run stopped by a restart or an outage picks up where it was and
//!   a second run imports nothing twice.

pub mod csv_source;
pub mod hold;
pub mod http;
pub mod link_ids;
pub mod report;
pub mod resolve;
pub mod spotify_match;
pub(crate) mod sql;
pub mod tags_fallback;
pub mod walk;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use self::hold::{HoldStatus, PlaylistHold};
use self::report::Report;
use self::resolve::Resolved;
use self::spotify_match::{Catalogue, PlaylistMatch, PublicMatch, PublicSource, StepEnd};
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
/// Fork: how a run an admin stopped ends.
const STOPPED_BY_ADMIN: &str = "stopped by an admin (state kept; run again to go on)";

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
    /// Fork: the public Spotify playlist entry the file matched, when the
    /// normal resolver found nothing.
    #[serde(default)]
    pub playlist_match: Option<PlaylistMatch>,
    /// Fork: how the file went in when nothing resolved it: `tags_only`
    /// ([`tags_fallback`]).
    #[serde(default)]
    pub via: Option<String>,
    /// Fork: the tag-only copy's path below the library folder.
    #[serde(default)]
    pub library_path: Option<String>,
    /// Fork: why the file went in by its tags: `not_found`, `refused`, or
    /// the import outcome it had (`unmatched:…`, `refused:…`).
    #[serde(default)]
    pub tags_reason: Option<String>,
}

/// A lookup's answer for a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolution {
    Found(Resolved),
    NotFound,
    /// Fork: the lookup itself was refused (one request a service will not
    /// answer); `tries` counts the runs that tried, and a re-run tries once
    /// more.
    Refused {
        reason: String,
        tries: u8,
    },
}

/// Fork: the runs that try a refused song before it is left as refused.
const REFUSED_TRIES: u8 = 2;
/// Fork: refused lookups in a row that stop the run: the service is
/// refusing everything, not one song.
const REFUSED_IN_A_ROW: usize = 20;
/// Fork: waits for a rate limit in a row before the run stops.
const LIMITED_WAITS: u32 = 6;
/// Fork: the first wait for a rate limit with no wait named; doubled with
/// each limit in a row, up to [`LIMITED_WAIT_MAX`].
const LIMITED_WAIT_BASE: Duration = Duration::from_secs(30);
const LIMITED_WAIT_MAX: Duration = Duration::from_secs(600);

/// Whether a file still wants a lookup: never looked up, or refused by
/// fewer runs than [`REFUSED_TRIES`].
fn wants_lookup(resolved: Option<&Resolution>) -> bool {
    match resolved {
        None => true,
        Some(Resolution::Refused { tries, .. }) => *tries < REFUSED_TRIES,
        Some(_) => false,
    }
}

/// The refusal a file records after `previous`.
fn refused(previous: Option<&Resolution>, reason: String) -> Resolution {
    let tries = match previous {
        Some(Resolution::Refused { tries, .. }) => tries.saturating_add(1),
        _ => 1,
    };
    Resolution::Refused { reason, tries }
}

/// Fork: what the run does after a lookup failed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Next {
    /// Record the song as refused, with this reason, and go on.
    SetAside(String),
    /// Wait this long, then ask for the same song again.
    Wait(Duration),
    /// Stop the run with this line.
    Stop(String),
}

/// Fork: the failures in a row a run has seen, so one refused song is set
/// aside while a service refusing or limiting everything stops the run.
#[derive(Debug, Default)]
struct Guard {
    refused: usize,
    limited: u32,
}

impl Guard {
    fn answered(&mut self) {
        self.refused = 0;
        self.limited = 0;
    }

    fn failed(&mut self, failure: resolve::LookupFailure) -> Next {
        use resolve::LookupFailure as F;
        match failure {
            F::Refused(message) => {
                self.limited = 0;
                self.refused += 1;
                if self.refused >= REFUSED_IN_A_ROW {
                    Next::Stop(format!(
                        "{REFUSED_IN_A_ROW} lookups in a row were refused (run again to go on): {message}"
                    ))
                } else {
                    Next::SetAside(message)
                }
            }
            F::Limited { message, wait } => {
                self.limited += 1;
                if self.limited > LIMITED_WAITS {
                    return Next::Stop(format!(
                        "still rate limited after {LIMITED_WAITS} waits (run again to go on): {message}"
                    ));
                }
                let doubled = LIMITED_WAIT_BASE
                    .saturating_mul(1 << (self.limited - 1).min(10))
                    .min(LIMITED_WAIT_MAX);
                Next::Wait(wait.map_or(doubled, |asked| asked.max(doubled)))
            }
            F::Down(message) => {
                Next::Stop(format!("a lookup failed (run again to go on): {message}"))
            }
        }
    }
}

/// The run state, kept in `state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    /// By path below the source.
    pub files: BTreeMap<String, FileState>,
    /// Fork: the public Spotify playlists and their entries, as last read.
    #[serde(default)]
    pub spotify: Option<Catalogue>,
}

/// Which run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    DryRun,
    Import,
    /// Fork: only the "link spotify ids" step, over the state an earlier
    /// import left ([`link_ids`]).
    LinkIds,
}

/// What the import is doing.
#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
pub struct Progress {
    /// A run is going.
    pub running: bool,
    pub mode: Option<Mode>,
    /// `walking`, `reading`, `resolving`, `spotify`, `matching`,
    /// `importing`, `tags` (fork: the import by tags), `linking` (fork: the
    /// provider links), `done`, `stopped`, `failed`.
    pub step: String,
    /// Fork: what the step is waiting on, when it waits (Spotify's rate
    /// limit).
    pub note: Option<String>,
    pub done: usize,
    pub total: usize,
    /// How the last run ended.
    pub last: Option<String>,
    /// Where the reports are written.
    pub reports: String,
    /// Whether a source folder is set.
    pub configured: bool,
    /// Fork: whether the public playlist match is set (the Spotify API or
    /// the playlist CSV folder).
    pub spotify_match: bool,
    /// Fork: the playlist CSV folder, when set (it takes the Spotify API's
    /// place).
    pub playlist_csvs: Option<String>,
    /// Fork: the download hold on synced playlists the import covers.
    pub hold: HoldStatus,
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
    /// Fork: the public Spotify playlist match, when set.
    public: Option<PublicMatch>,
    /// Fork: an admin asked the run to stop.
    stop: AtomicBool,
    /// Fork: the download hold.
    hold: Arc<PlaylistHold>,
    /// Fork: asks for the songs the hold kept, once it lifts.
    releaser: Arc<crate::imports::spotify_sync::SpotifySync>,
    /// Fork: the last "link spotify ids" step's report section.
    links: Mutex<Option<link_ids::LinkReport>>,
    /// Fork: the state file's change time and the files still waiting for
    /// the scan, as the last link step saw them; the link loop runs again
    /// only when either says there is something new.
    link_seen: Mutex<Option<(Option<std::time::SystemTime>, usize)>>,
}

/// Fork: the Spotify side of the import: the public-playlist match's
/// client and the download hold shared with the playlist sync.
pub struct SpotifyParts {
    pub service: Arc<crate::imports::spotify::SpotifyImportService>,
    pub library: Arc<crate::imports::spotify_library::SpotifyLibrary>,
    pub sync: Arc<crate::imports::spotify_sync::SpotifySync>,
    pub hold: Arc<PlaylistHold>,
}

/// Where the reports and state go: `PLAYLIST_IMPORT_REPORTS`, else
/// `playlist-import` beside the database.
pub fn reports_dir(db: &AcquireDb) -> PathBuf {
    std::env::var_os(REPORTS_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| {
            db.path()
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("playlist-import")
        })
}

impl PlaylistImport {
    /// Fork: the playlist CSV folder, when there is one (it takes the
    /// Spotify match's place): `PLAYLIST_IMPORT_PLAYLIST_CSVS`, else the
    /// source folder when it holds top-level `.csv` files.
    pub(crate) fn csv_dir(&self) -> Option<PathBuf> {
        csv_source::dir_for(self.source.as_deref())
    }

    /// The import over the environment's folders; the public-playlist
    /// match is on when the environment names a source.
    pub fn new(
        db: AcquireDb,
        lookups: Arc<Live>,
        library: Arc<dyn LandingLibrary>,
        spotify: SpotifyParts,
    ) -> Self {
        let source = std::env::var_os(SOURCE_ENV)
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty());
        let reports = reports_dir(&db);
        let progress = Progress {
            reports: reports.display().to_string(),
            configured: source.is_some(),
            ..Progress::default()
        };
        let public = PublicSource::from_env().map(|source| PublicMatch {
            source,
            service: spotify.service,
            library: spotify.library,
        });
        Self {
            db,
            lookups,
            library,
            source,
            reports,
            progress: Mutex::new(progress),
            public,
            stop: AtomicBool::new(false),
            hold: spotify.hold,
            releaser: spotify.sync,
            links: Mutex::new(None),
            link_seen: Mutex::new(None),
        }
    }

    /// Fork: the download hold.
    pub fn hold(&self) -> &Arc<PlaylistHold> {
        &self.hold
    }

    /// Fork: ask a running run to stop at the next file; what it did is
    /// kept. Answers whether a run was going.
    pub fn stop(&self) -> bool {
        let running = self.progress().running;
        if running {
            self.stop.store(true, Ordering::SeqCst);
        }
        running
    }

    /// Fork: release the download hold now; answers how many songs were
    /// asked for.
    pub async fn release_hold(&self) -> usize {
        let deferred = self.hold.release();
        self.ask_deferred(deferred).await
    }

    async fn ask_deferred(&self, deferred: Vec<hold::Deferred>) -> usize {
        self.releaser.ask_deferred(deferred).await
    }

    fn set_note(&self, note: &str) {
        let mut progress = self.progress.lock().unwrap_or_else(|p| p.into_inner());
        progress.note = Some(note.to_owned());
    }

    /// What the import is doing.
    pub fn progress(&self) -> Progress {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let csvs = self.csv_dir();
        progress.spotify_match = self.public.is_some() || csvs.is_some();
        progress.playlist_csvs = csvs.map(|dir| dir.display().to_string());
        progress.hold = self.hold.status();
        progress
    }

    fn set(&self, step: &str, done: usize, total: usize) {
        let mut progress = self.progress.lock().unwrap_or_else(|p| p.into_inner());
        progress.step = step.to_owned();
        progress.note = None;
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
            progress.note = None;
            self.stop.store(false, Ordering::SeqCst);
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
            let by_admin = this.stop.swap(false, Ordering::SeqCst);
            let finished = mode == Mode::Import && !ended.starts_with("stopped");
            {
                let mut progress = this.progress.lock().unwrap_or_else(|p| p.into_inner());
                progress.running = false;
                progress.note = None;
                progress.step = if by_admin && ended.starts_with("stopped") {
                    "stopped".to_owned()
                } else if ended.starts_with("stopped") {
                    "failed".to_owned()
                } else {
                    "done".to_owned()
                };
                progress.last = Some(ended);
            }
            // The real run is done: the songs the hold kept are asked for.
            if finished {
                let deferred = this.hold.finish();
                let asked = this.ask_deferred(deferred).await;
                tracing::info!(asked, "playlist import finished; download hold lifted");
            }
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
        if mode == Mode::LinkIds {
            return self.link_only(source).await;
        }
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
        let stopped = self.resolve_all(&mut state).await;
        self.save(&state).await?;
        if let Err(error) = stopped {
            self.write_report(&state, mode, Some(&error)).await?;
            return Err(error);
        }
        // Fork: what the resolver left, matched to the public playlists.
        let matched = self.public_match(&mut state).await;
        self.save(&state).await?;
        let stopped = match matched {
            Ok(StepEnd::Done) => None,
            Ok(StepEnd::Stopped) => Some(STOPPED_BY_ADMIN.to_owned()),
            Err(error) => Some(error),
        };
        if let Some(error) = stopped {
            self.write_report(&state, mode, Some(&error)).await?;
            return Err(error);
        }
        if mode == Mode::Import {
            self.link_found(&state).await?;
        }
        if mode == Mode::Import {
            let stopped = self.import_all(source, &mut state).await;
            self.save(&state).await?;
            if let Err(error) = stopped {
                self.write_report(&state, mode, Some(&error)).await?;
                return Err(error);
            }
            // Fork: what is still left goes in by its tags.
            if tags_fallback::enabled() {
                self.check_library(&mut state).await?;
                let stopped = self.import_by_tags(source, &mut state).await;
                self.save(&state).await?;
                if let Err(error) = stopped {
                    self.write_report(&state, mode, Some(&error)).await?;
                    return Err(error);
                }
            }
            // Fork: last, the provider links of every file put in.
            self.set("linking", 0, 0);
            if let Err(error) = self.link_ids(source, &state).await {
                tracing::warn!(%error, "playlist import: provider links not written (the link loop tries again)");
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
            .filter(|(_, file)| file.duplicate_of.is_none() && !settled(file.outcome.as_deref()))
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
    async fn resolve_all(&self, state: &mut State) -> Result<(), String> {
        let todo: Vec<String> = state
            .files
            .iter()
            .filter(|(_, file)| {
                file.duplicate_of.is_none()
                    && !file.in_library
                    && file.outcome.is_none()
                    && file.tags.is_some()
                    && wants_lookup(file.resolved.as_ref())
            })
            .map(|(rel, _)| rel.clone())
            .collect();
        let total = todo.len();
        let mut guard = Guard::default();
        for (index, rel) in todo.iter().enumerate() {
            if self.stop.load(Ordering::SeqCst) {
                return Err(STOPPED_BY_ADMIN.to_owned());
            }
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
            let answer = loop {
                match resolve::resolve(&facts, self.lookups.as_ref(), self.lookups.as_ref()).await {
                    Ok(answer) => {
                        guard.answered();
                        break Ok(answer);
                    }
                    Err(failure) => {
                        if let Some(reason) = self.after_failure(&mut guard, rel, failure).await? {
                            break Err(reason);
                        }
                    }
                }
            };
            if let Some(file) = state.files.get_mut(rel) {
                file.resolved = Some(match answer {
                    Ok(Some(found)) => Resolution::Found(found),
                    Ok(None) => Resolution::NotFound,
                    Err(reason) => refused(file.resolved.as_ref(), reason),
                });
            }
            if (index + 1) % SAVE_EVERY == 0 {
                self.save(state).await?;
            }
            tokio::time::sleep(LOOKUP_GAP).await;
        }
        Ok(())
    }

    /// Fork: act on a failed lookup for `rel`. `Ok(Some(reason))` sets the
    /// song aside, `Ok(None)` asks again after a wait, `Err` stops the run.
    async fn after_failure(
        &self,
        guard: &mut Guard,
        rel: &str,
        failure: resolve::LookupFailure,
    ) -> Result<Option<String>, String> {
        match guard.failed(failure) {
            Next::SetAside(reason) => {
                tracing::warn!(file = rel, %reason, "playlist import: lookup refused; song set aside");
                Ok(Some(reason))
            }
            Next::Wait(wait) => {
                tracing::warn!(file = rel, ?wait, "playlist import: rate limited; waiting");
                let until = tokio::time::Instant::now() + wait;
                while tokio::time::Instant::now() < until {
                    if self.stop.load(Ordering::SeqCst) {
                        return Err(STOPPED_BY_ADMIN.to_owned());
                    }
                    tokio::time::sleep(Duration::from_secs(1).min(wait)).await;
                }
                Ok(None)
            }
            Next::Stop(line) => Err(line),
        }
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
        let mut recordings: HashMap<String, String> = HashMap::new();
        for (rel, file) in &state.files {
            // Fork: an `unmatched:` outcome is tried again.
            if file.duplicate_of.is_some() || file.in_library || settled(file.outcome.as_deref()) {
                continue;
            }
            if let Some(Resolution::Found(found)) = &file.resolved {
                albums
                    .entry(found.album.to_ascii_lowercase())
                    .or_default()
                    .push(rel.clone());
                if !found.recording.is_empty() {
                    recordings.insert(rel.clone(), found.recording.to_ascii_lowercase());
                }
            }
        }
        let total = albums.len();
        for (index, (album, rels)) in albums.into_iter().enumerate() {
            if self.stop.load(Ordering::SeqCst) {
                return Err(STOPPED_BY_ADMIN.to_owned());
            }
            self.set("importing", index, total);
            let outcomes = self
                .import_album(source, &album, &rels, &recordings)
                .await?;
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
    ///
    /// Fork: the files the first try leaves unpaired (the edition carrying
    /// their recording was not among the editions read, or the edition
    /// listing came back empty) are tried once more, guided by the
    /// recording the resolver found ([`Self::guide`]).
    async fn import_album(
        &self,
        source: &Path,
        album: &str,
        rels: &[String],
        recordings: &HashMap<String, String>,
    ) -> Result<Vec<(String, String)>, String> {
        let mut outcomes = self.place_album(source, album, rels, None, None).await?;
        let unsettled = |outcomes: &[(String, String)]| -> Vec<String> {
            outcomes
                .iter()
                .filter(|(_, outcome)| !settled(Some(outcome)))
                .map(|(rel, _)| rel.clone())
                .collect()
        };
        let mut retry = unsettled(&outcomes);
        if retry.is_empty() {
            return Ok(outcomes);
        }
        let Some(guide) = self.guide(album, &retry, recordings).await else {
            return Ok(outcomes);
        };
        tracing::info!(
            album,
            files = retry.len(),
            editions = guide.asked.len(),
            "playlist import: unpaired files tried again on the editions of their recordings"
        );
        let tries: Vec<Option<&str>> = if guide.asked.is_empty() {
            vec![None]
        } else {
            guide.asked.iter().map(|id| Some(id.as_str())).collect()
        };
        for asked in tries {
            if retry.is_empty() {
                break;
            }
            let again = self
                .place_album(source, album, &retry, Some(&guide.recordings), asked)
                .await?;
            for (rel, outcome) in &again {
                if let Some(known) = outcomes.iter_mut().find(|(known, _)| known == rel) {
                    known.1 = outcome.clone();
                } else {
                    outcomes.push((rel.clone(), outcome.clone()));
                }
            }
            retry = unsettled(&again);
        }
        Ok(outcomes)
    }

    /// Fork: the guide for files of `album` that did not pair: the
    /// editions of the group that carry their recordings (read from the
    /// recordings themselves, not from the group's edition listing), then
    /// the editions the group's own page lists, at most [`GUIDE_TRIES`];
    /// and the recordings, set on the files that name none. No edition is
    /// asked when the library keeps the album on one already (that edition
    /// is followed). `None` when there is nothing to go on. A
    /// failed read is no guide (the files stay unmatched and a re-run
    /// tries again).
    async fn guide(
        &self,
        album: &str,
        rels: &[String],
        recordings: &HashMap<String, String>,
    ) -> Option<Guide> {
        let virtual_album = is_virtual(album);
        // A stand-in recording only pairs on a stand-in album.
        let known: HashMap<String, String> = rels
            .iter()
            .filter_map(|rel| Some((rel.clone(), recordings.get(rel)?.clone())))
            .filter(|(_, recording)| is_virtual(recording) == virtual_album)
            .collect();
        let mut asked: Vec<String> = Vec::new();
        let kept = matches!(self.library.chosen_edition(album).await, Ok(Some(_)));
        if !virtual_album && !kept {
            let view = match self.lookups.musicbrainz() {
                Ok(view) => view,
                Err(error) => {
                    tracing::warn!(%error, "playlist import: no MusicBrainz for the edition guide");
                    return None;
                }
            };
            let mut mb: Vec<&String> = known.values().collect();
            mb.sort();
            mb.dedup();
            let mut carried: Vec<Vec<String>> = Vec::new();
            for recording in mb.into_iter().take(GUIDE_RECORDINGS) {
                match view
                    .lookup_recording(
                        recording,
                        &["releases", "release-groups"],
                        droppedneedle_providers::musicbrainz::Criticality::IdentityCritical,
                    )
                    .await
                {
                    Ok(Some(found)) => carried.push(
                        found
                            .entity
                            .releases
                            .iter()
                            .filter(|release| {
                                release
                                    .release_group
                                    .as_ref()
                                    .is_some_and(|group| group.id.eq_ignore_ascii_case(album))
                            })
                            .map(|release| release.id.to_ascii_lowercase())
                            .collect(),
                    ),
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%error, recording, "playlist import: recording did not read");
                        return None;
                    }
                }
            }
            asked = pick_editions(&carried);
            if asked.len() < GUIDE_TRIES {
                match view
                    .lookup_release_group(
                        album,
                        &["releases"],
                        droppedneedle_providers::musicbrainz::Criticality::IdentityCritical,
                    )
                    .await
                {
                    Ok(Some(group)) => {
                        for release in &group.entity.releases {
                            let id = release.id.to_ascii_lowercase();
                            if !asked.contains(&id) {
                                asked.push(id);
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%error, album, "playlist import: release group did not read");
                        return None;
                    }
                }
            }
        }
        asked.truncate(GUIDE_TRIES);
        (!asked.is_empty() || !known.is_empty()).then_some(Guide {
            asked,
            recordings: known,
        })
    }

    /// Place `rels` on `album`, with `asked` tried first and the
    /// `recordings` set on files that name none.
    async fn place_album(
        &self,
        source: &Path,
        album: &str,
        rels: &[String],
        recordings: Option<&HashMap<String, String>>,
        asked: Option<&str>,
    ) -> Result<Vec<(String, String)>, String> {
        let paths: Vec<PathBuf> = rels.iter().map(|rel| source.join(rel)).collect();
        let beneath = source.to_path_buf();
        let mut landing =
            tokio::task::spawn_blocking(move || probe_beneath(&paths, &[], Some(&beneath)))
                .await
                .map_err(|error| error.to_string())?;
        let rel_of = |path: &Path| {
            path.strip_prefix(source)
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default()
        };
        if let Some(recordings) = recordings {
            for file in &mut landing.audio {
                if file.tag.musicbrainz_recording_id.is_none()
                    && let Some(recording) = recordings.get(&rel_of(&file.path))
                {
                    file.tag.musicbrainz_recording_id = Some(recording.clone());
                }
            }
        }
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
        let summary =
            match identify::assigned(self.library.as_ref(), &landing, album, asked).await {
                Identified::Matched(summary) => summary,
                Identified::Review { reason, .. } => {
                    out.extend(
                        landing.audio.iter().map(|file| {
                            (rel_of(&file.path), format!("unmatched:{}", reason.code()))
                        }),
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
                    set_aside: false,
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

    /// Fork: copy every file [`tags_fallback::takes`] into the library
    /// folder as it is tagged, for the scan to index and flag
    /// ([`tags_fallback`]). A file that will not copy keeps its outcome
    /// and is tried on the next run.
    async fn import_by_tags(&self, source: &Path, state: &mut State) -> Result<(), String> {
        let todo: Vec<String> = state
            .files
            .iter()
            .filter(|(_, file)| tags_fallback::takes(file))
            .map(|(rel, _)| rel.clone())
            .collect();
        if todo.is_empty() {
            return Ok(());
        }
        let library = self.library.clone();
        let root = tokio::task::spawn_blocking(move || {
            (!library.library_off())
                .then(|| library.import_folder())
                .flatten()
        })
        .await
        .map_err(|error| error.to_string())?;
        let Some(root) = root else {
            tracing::warn!(
                files = todo.len(),
                "playlist import: no library folder takes imports; nothing imported by tags"
            );
            return Ok(());
        };
        let total = todo.len();
        for (index, rel) in todo.iter().enumerate() {
            if self.stop.load(Ordering::SeqCst) {
                return Err(STOPPED_BY_ADMIN.to_owned());
            }
            self.set("tags", index, total);
            let Some(tags) = state.files.get(rel).and_then(|file| file.tags.clone()) else {
                continue;
            };
            let name = Path::new(rel)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let wanted = tags_fallback::destination(&tags, &name);
            let (root, from, key) = (root.clone(), source.join(rel), tags_fallback::key_of(rel));
            let placed = tokio::task::spawn_blocking(move || {
                tags_fallback::place(&root, &from, &wanted, &key)
            })
            .await
            .map_err(|error| error.to_string())?;
            match placed {
                Ok(path) => {
                    if let Some(file) = state.files.get_mut(rel) {
                        file.tags_reason = Some(tags_fallback::reason(file));
                        file.outcome = Some(tags_fallback::OUTCOME.to_owned());
                        file.via = Some(tags_fallback::VIA.to_owned());
                        file.library_path = Some(path.to_string_lossy().replace('\\', "/"));
                    }
                }
                Err(error) => {
                    tracing::warn!(file = rel, %error, "playlist import: tag-only copy failed");
                }
            }
            if (index + 1) % SAVE_EVERY == 0 {
                self.save(state).await?;
            }
        }
        tracing::info!(
            files = total,
            "playlist import: files imported by their tags"
        );
        Ok(())
    }

    /// Fork: the "link spotify ids" step over `state` ([`link_ids`]): pair
    /// the files to the playlist CSV rows, find their tracks, write the
    /// links. Answers the report section, also kept for the report.
    async fn link_ids(&self, source: &Path, state: &State) -> Result<link_ids::LinkReport, String> {
        let catalogue = self.link_catalogue(state).await?;
        let asks = link_ids::asks(state, source);
        let asked = asks.clone();
        let tracks = self
            .db
            .db()
            .read("playlist_import.link_tracks", move |conn| {
                Ok::<_, DbError>(link_ids::tracks(conn, &asked)?)
            })
            .await
            .map_err(|error| error.to_string())?;
        let plan = link_ids::plan(state, &catalogue, &asks, &tracks);
        let writes = plan.writes;
        if !writes.is_empty() {
            let now = self.db.now_epoch() as f64;
            self.db
                .db()
                .write(Lane::Background, "playlist_import.link_ids", move |tx| {
                    Ok::<_, DbError>(link_ids::write(tx, &writes, now)?)
                })
                .await
                .map_err(|error| error.to_string())?;
        }
        let report = plan.report;
        tracing::info!(
            by_spotify = report.by_spotify,
            isrc_only = report.isrc_only,
            no_link = report.no_link,
            waiting = report.waiting,
            rows_paired = report.rows_paired,
            "playlist import: provider links written"
        );
        *self.links.lock().unwrap_or_else(|p| p.into_inner()) = Some(report.clone());
        let seen = (state_changed(&self.state_path()), report.waiting);
        *self.link_seen.lock().unwrap_or_else(|p| p.into_inner()) = Some(seen);
        Ok(report)
    }

    /// Fork: the playlist rows the link step pairs to: the playlist CSVs
    /// read again (each mapped to its folder as the CSV match does), else
    /// the catalogue the state keeps.
    async fn link_catalogue(&self, state: &State) -> Result<Catalogue, String> {
        let Some(dir) = self.csv_dir() else {
            return Ok(state.spotify.clone().unwrap_or_default());
        };
        let folders: std::collections::BTreeSet<String> = state
            .files
            .values()
            .map(|file| file.playlist.clone())
            .collect();
        let source = format!("{}{}", csv_source::ID_PREFIX, dir.display());
        let read = tokio::task::spawn_blocking(move || csv_source::read_dir(&dir))
            .await
            .map_err(|error| error.to_string())??;
        let mut playlists = read.playlists;
        spotify_match::map_folders(&mut playlists, &folders);
        Ok(Catalogue {
            source,
            playlists,
            ..Catalogue::default()
        })
    }

    /// Fork: a [`Mode::LinkIds`] run: the link step alone over the saved
    /// state, then the import report again with its section.
    async fn link_only(&self, source: &Path) -> Result<String, String> {
        let state = self.load().await;
        self.set("linking", 0, 0);
        let links = self.link_ids(source, &state).await?;
        self.write_report(&state, Mode::LinkIds, None).await?;
        Ok(format!(
            "provider links: {} files by Spotify id, {} by ISRC only, {} with no link ({} not indexed yet); {} of {} CSV rows paired",
            links.by_spotify,
            links.isrc_only,
            links.no_link,
            links.waiting,
            links.rows_paired,
            links.csv_rows
        ))
    }

    /// Fork: one turn of the link loop: a [`Mode::LinkIds`] run when a
    /// source is set, a state exists, no run is going or stopped short,
    /// and there is something new (first turn since start, the state
    /// changed, or files still wait for the scan).
    fn link_tick(self: &Arc<Self>) {
        if self.source.is_none() {
            return;
        }
        let path = self.state_path();
        if !path.exists() {
            return;
        }
        let progress = self.progress();
        if progress.running
            || progress
                .last
                .as_deref()
                .is_some_and(|last| last.starts_with("stopped"))
        {
            return;
        }
        let seen = *self.link_seen.lock().unwrap_or_else(|p| p.into_inner());
        let due = match seen {
            None => true,
            Some((changed, waiting)) => waiting > 0 || changed != state_changed(&path),
        };
        if due && let Err(error) = self.start(Mode::LinkIds) {
            tracing::debug!(%error, "playlist import: link pass not started");
        }
    }

    async fn write_report(
        &self,
        state: &State,
        mode: Mode,
        stopped: Option<&str>,
    ) -> Result<Report, String> {
        let mut report = Report::of(state, mode, stopped, self.db.now_epoch());
        if mode != Mode::DryRun {
            report.links = self.links.lock().unwrap_or_else(|p| p.into_inner()).clone();
        }
        let dir = self.reports.clone();
        let json = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
        let text = report.text();
        let name = match mode {
            Mode::DryRun => "dry-run",
            Mode::Import | Mode::LinkIds => "import",
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

/// Fork: recordings read per album for the edition guide.
const GUIDE_RECORDINGS: usize = 4;
/// Fork: editions a second try asks, one after another, at most.
const GUIDE_TRIES: usize = 4;

/// Fork: what a second try of an album's unpaired files goes on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Guide {
    /// The editions to ask, in turn.
    asked: Vec<String>,
    /// The resolver's recording, by file path below the source.
    recordings: HashMap<String, String>,
}

/// Fork: whether an import outcome is final. An `unmatched:` one is not:
/// the files did not pair with the editions read (often the edition that
/// carries them was not among them, or the edition listing came back
/// empty), so a re-run tries them again.
pub fn settled(outcome: Option<&str>) -> bool {
    outcome.is_some_and(|outcome| !outcome.starts_with("unmatched:"))
}

/// Fork: the editions carrying the files' recordings, the one carrying
/// the most first (each entry is the editions of the group one recording
/// is on); the first listed wins a tie.
fn pick_editions(carried: &[Vec<String>]) -> Vec<String> {
    let mut votes: Vec<(String, usize)> = Vec::new();
    for editions in carried {
        let mut seen = std::collections::HashSet::new();
        for edition in editions {
            if !seen.insert(edition) {
                continue;
            }
            match votes.iter_mut().find(|(known, _)| known == edition) {
                Some((_, count)) => *count += 1,
                None => votes.push((edition.clone(), 1)),
            }
        }
    }
    // A stable sort keeps the first listed ahead in a tie.
    votes.sort_by(|a, b| b.1.cmp(&a.1));
    votes.into_iter().map(|(edition, _)| edition).collect()
}

/// Fork: the link loop's name on the supervisor.
pub const LINK_JOB: &str = "playlist-import-links";
/// Fork: how often the link loop looks for something to link.
pub const LINK_EVERY: Duration = Duration::from_secs(30 * 60);

/// Fork: start the link loop: a few minutes after start, then every
/// [`LINK_EVERY`], the "link spotify ids" step runs over the saved state
/// when there is something new ([`PlaylistImport::link_tick`]), so files
/// imported before the step existed get their links without a re-import,
/// and tag-only copies get theirs once the scan has indexed them.
///
/// # Errors
///
/// The loop is already running.
pub fn spawn_link_loop(
    supervisor: &droppedneedle_kernel::jobs::Supervisor,
    import: Arc<PlaylistImport>,
) -> Result<(), droppedneedle_kernel::jobs::AlreadyRunning> {
    use droppedneedle_kernel::jobs::{FnTick, Next, Schedule};
    supervisor.periodic(
        LINK_JOB,
        Schedule::new(LINK_EVERY)
            .with_initial_delay(Duration::from_secs(300))
            .with_jitter(Duration::from_secs(60)),
        FnTick(move |_| {
            let import = import.clone();
            async move {
                import.link_tick();
                Ok(Next::Continue)
            }
        }),
    )
}

/// Fork: when the state file last changed, `None` when it does not read.
fn state_changed(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
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
        let rate = if settled(file.outcome.as_deref()) {
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
