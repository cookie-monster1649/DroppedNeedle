//! Fork: live playlist folders.
//!
//! Every user's playlists kept as folders of hardlinks to the library
//! files, `<root>/<user>/<playlist>/<NN - Artist - Title.ext>`, for a file
//! share (Nextcloud) to show without a copy of anything. The root is the
//! [`PlaylistFolders`] setting; empty is off.
//!
//! The loop polls instead of hooking every playlist edit, so no path that
//! changes a playlist can be missed: create, edit, reorder, rename,
//! delete, Spotify sync, an import linking a wanted track and an upgrade
//! replacing a file all change the rows the fingerprint covers. Each
//! [`TICK`] reads every user's fingerprint; a user whose fingerprint moved
//! and then held still for one tick (or has waited [`MAX_WAIT_TICKS`]) is
//! reconciled. Every [`FULL_EVERY_TICKS`] ticks everyone is reconciled,
//! which also repairs anything changed on disk.
//!
//! Before the first link in a root the loop makes a probe link from a
//! library file. A filesystem that refuses (EXDEV, EPERM, ENOTSUP) turns
//! the job off until the setting changes; it never falls back to copies.
//!
//! After a user's folder changes, `<root>/.rescan-<folder>` is written for
//! a host helper to ask the file share to rescan that folder.

mod sql;
pub mod tree;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use self::tree::{ProbeFailure, WantedEntry, WantedPlaylist, WantedUser};
use super::schedule::Schedule;
use super::supervisor::{AlreadyRunning, Next, Supervisor, TickError};
use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::runtime_config::Live;
use crate::runtime_config::secret_sections::TypedLibrary;
use crate::runtime_config::sections::PlaylistFolders;

/// Name of the loop.
pub const JOB_NAME: &str = "fork-playlist-folders";
/// Poll cadence, and the debounce step.
pub const TICK: Duration = Duration::from_secs(30);
/// A user who keeps changing is reconciled after this many ticks anyway.
pub const MAX_WAIT_TICKS: u32 = 6;
/// Everyone is reconciled every this many ticks (10 minutes).
pub const FULL_EVERY_TICKS: u64 = 20;
/// Row code of a failed tick.
pub const FAILED: &str = "PLAYLIST_FOLDERS_FAILED";

/// Where the root stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// No root set.
    Off,
    /// The root works.
    Ready(PathBuf),
    /// The root cannot be used, and why.
    Disabled(PathBuf, String),
}

#[derive(Default)]
struct State {
    /// The root the probe last passed or failed for.
    probed: Option<(PathBuf, Result<(), String>)>,
    ticks: u64,
    /// Fingerprint each user was last reconciled at.
    done: HashMap<String, u64>,
    /// Users waiting: last fingerprint seen and ticks waited.
    pending: HashMap<String, (u64, u32)>,
}

/// The loop's backend.
pub struct PlaylistFolderJob {
    db: Db,
    setting: Live<PlaylistFolders>,
    library: Live<TypedLibrary>,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

/// What one tick did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    /// Users whose files were reconciled.
    pub reconciled: Vec<String>,
    /// Changes per user id, summed with the root's own.
    pub changes: BTreeMap<String, tree::Changes>,
    /// Rescan requests written, by folder.
    pub rescans: Vec<String>,
}

impl PlaylistFolderJob {
    /// The job over the database, the settings and the clock.
    pub fn new(
        db: Db,
        setting: Live<PlaylistFolders>,
        library: Live<TypedLibrary>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            db,
            setting,
            library,
            clock,
            state: Mutex::default(),
        }
    }

    /// The configured root, checked: absolute, a folder, and clear of every
    /// library root.
    fn root(&self) -> Result<Option<PathBuf>, String> {
        let Some(setting) = self.setting.read() else {
            return Ok(None);
        };
        let configured = setting.path.trim();
        if configured.is_empty() {
            return Ok(None);
        }
        let real = std::fs::canonicalize(configured)
            .map_err(|error| format!("{configured} is not reachable: {error}"))?;
        if !real.is_dir() {
            return Err(format!("{configured} is not a folder"));
        }
        let library = self
            .library
            .read()
            .ok_or_else(|| "the library settings do not read".to_owned())?;
        for root in &library.library_roots {
            let Ok(lib) = std::fs::canonicalize(root.path.trim()) else {
                continue;
            };
            if real.starts_with(&lib) || lib.starts_with(&real) {
                return Err(format!(
                    "{} overlaps the library folder {}",
                    real.display(),
                    lib.display()
                ));
            }
        }
        Ok(Some(real))
    }

    /// Where the job stands now.
    pub fn status(&self) -> Status {
        match self.root() {
            Ok(None) => Status::Off,
            Err(why) => Status::Disabled(PathBuf::new(), why),
            Ok(Some(root)) => {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                match &state.probed {
                    Some((probed, Err(why))) if *probed == root => {
                        Status::Disabled(root, why.clone())
                    }
                    _ => Status::Ready(root),
                }
            }
        }
    }

    /// One tick, blocking. `force` reconciles everyone.
    pub fn tick(&self, force: bool) -> Result<TickReport, String> {
        let root = match self.root() {
            Ok(Some(root)) => root,
            Ok(None) => {
                *self.state.lock().unwrap_or_else(|e| e.into_inner()) = State::default();
                return Ok(TickReport::default());
            }
            Err(why) => return Err(why),
        };
        if !self.probe(&root)? {
            return Ok(TickReport::default());
        }
        let users = self.read_users().map_err(|error| error.to_string())?;
        let prints: HashMap<String, u64> = users
            .iter()
            .map(|user| (user.user_id.clone(), fingerprint(user)))
            .collect();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.ticks += 1;
        let full = force || state.ticks % FULL_EVERY_TICKS == 1 || state.done.is_empty();
        let mut due: HashSet<String> = HashSet::new();
        let known: HashSet<String> = state.done.keys().cloned().collect();
        for id in known.iter().chain(prints.keys()) {
            let now = prints.get(id).copied().unwrap_or(0);
            if state.done.get(id) == Some(&now) {
                state.pending.remove(id);
                continue;
            }
            let ready = match state.pending.get(id) {
                Some((seen, waited)) => *seen == now || *waited + 1 >= MAX_WAIT_TICKS,
                None => false,
            };
            if ready {
                due.insert(id.clone());
            } else {
                let waited = state.pending.get(id).map_or(0, |(_, waited)| waited + 1);
                state.pending.insert(id.clone(), (now, waited));
            }
        }
        if !full && due.is_empty() {
            return Ok(TickReport::default());
        }
        let only = (!full).then_some(&due);
        let (per_user, root_changes) =
            tree::reconcile(&root, &users, only).map_err(|error| error.to_string())?;
        let mut report = TickReport::default();
        let stamp = self.clock.now_ms();
        for user in &users {
            let id = &user.user_id;
            if !full && !due.contains(id) {
                continue;
            }
            state.pending.remove(id);
            state.done.insert(id.clone(), prints[id]);
            report.reconciled.push(id.clone());
        }
        for id in known {
            if !prints.contains_key(&id) && (full || due.contains(&id)) {
                state.done.remove(&id);
                state.pending.remove(&id);
            }
        }
        drop(state);
        for (id, (folder, changes)) in &per_user {
            if changes.any() || root_changes.any() {
                match tree::request_rescan(&root, folder, stamp) {
                    Ok(()) => report.rescans.push(folder.clone()),
                    Err(error) => tracing::warn!(%error, %folder, "playlist folders: rescan marker not written"),
                }
            }
            if changes.any() || changes.failures > 0 {
                tracing::info!(user = %id, %folder, ?changes, "playlist folders reconciled");
            }
            report.changes.insert(id.clone(), changes.clone());
        }
        if root_changes.any() {
            tracing::info!(changes = ?root_changes, "playlist folders: user folders changed");
            report.changes.insert(String::new(), root_changes);
        }
        Ok(report)
    }

    /// Probe the root once per root. `Ok(false)` while it is disabled.
    fn probe(&self, root: &Path) -> Result<bool, String> {
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((probed, outcome)) = &state.probed
                && probed == root
            {
                return Ok(outcome.is_ok());
            }
        }
        let source: Option<String> = self
            .db
            .read_blocking("playlist-folders.probe", |conn| {
                use rusqlite::OptionalExtension;
                conn.prepare_cached(sql::PROBE_FILE.text())?
                    .query_row([], |row| row.get(0))
                    .optional()
                    .map_err(DbError::from)
            })
            .map_err(|error: DbError| error.to_string())?;
        let Some(source) = source else {
            return Ok(false);
        };
        let outcome = match tree::probe(root, Path::new(&source)) {
            Ok(()) => {
                tracing::info!(root = %root.display(), "playlist folders: hardlinks work here; on");
                Ok(())
            }
            Err(ProbeFailure::Unsupported(why)) => {
                tracing::error!(root = %root.display(), %why, "playlist folders: hardlinks refused here; off until the setting changes (never copies)");
                Err(why)
            }
            Err(ProbeFailure::Transient(why)) => return Err(why),
        };
        let ok = outcome.is_ok();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        *state = State {
            probed: Some((root.to_path_buf(), outcome)),
            ..State::default()
        };
        Ok(ok)
    }

    fn read_users(&self) -> Result<Vec<WantedUser>, DbError> {
        let rows = self
            .db
            .read_blocking("playlist-folders.entries", |conn| {
                conn.prepare_cached(sql::FOLDER_ENTRIES.text())?
                    .query_map([], sql::FolderEntryRow::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(DbError::from)
            })?;
        Ok(group(rows))
    }
}

/// Rows (ordered by user, playlist, position) into users.
fn group(rows: Vec<sql::FolderEntryRow>) -> Vec<WantedUser> {
    let mut users: Vec<WantedUser> = Vec::new();
    for row in rows {
        if users.last().is_none_or(|user| user.user_id != row.user_id) {
            users.push(WantedUser {
                user_id: row.user_id.clone(),
                name: row.display_name.clone(),
                playlists: Vec::new(),
            });
        }
        let user = users.last_mut().expect("pushed above");
        if user
            .playlists
            .last()
            .is_none_or(|playlist| playlist.playlist_id != row.playlist_id)
        {
            user.playlists.push(WantedPlaylist {
                playlist_id: row.playlist_id.clone(),
                name: row.playlist_name.clone(),
                entries: Vec::new(),
            });
        }
        let playlist = user.playlists.last_mut().expect("pushed above");
        if row.position.is_none() {
            continue;
        }
        playlist.entries.push(row.file_path.map(|path| WantedEntry {
            artist: row.artist_name.unwrap_or_default(),
            title: row.track_name.unwrap_or_default(),
            source: PathBuf::from(path),
            revision: (
                row.file_size_bytes.unwrap_or(0),
                row.file_mtime_ns.unwrap_or(0),
            ),
        }));
    }
    users
}

/// A user's fingerprint: everything their folder is built from, the
/// library file's size and mtime included so an upgrade shows.
fn fingerprint(user: &WantedUser) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    user.hash(&mut hasher);
    hasher.finish()
}

/// Every 30 seconds, after one tick's delay.
pub fn default_schedule() -> Schedule {
    Schedule::new(TICK)
        .with_initial_delay(TICK)
        .with_backoff(Duration::from_secs(30), Duration::from_secs(600))
}

/// Start the loop on `supervisor`.
pub fn spawn_on(
    supervisor: &Supervisor,
    job: Arc<PlaylistFolderJob>,
    schedule: Schedule,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic_blocking(JOB_NAME, schedule, move |_| {
        job.tick(false)
            .map(|_| Next::Continue)
            .map_err(|why| TickError::new(FAILED, why))
    })
}
