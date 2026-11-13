//! Fork: the file side of live playlist folders. Blocking; the loop runs
//! it off the async workers.
//!
//! Layout: `<root>/<user>/<playlist>/<NN - Artist - Title.ext>`, each file
//! a hardlink to the library file. A folder this module made carries a
//! marker file naming its user or playlist id, so a rename follows the id
//! and nothing without a marker is ever removed. Files are made under a
//! temporary name and renamed into place. Nothing is ever copied.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Marker in a user folder: the user id.
pub const USER_MARKER: &str = ".droppedneedle-user";
/// Marker in a playlist folder: the playlist id.
pub const PLAYLIST_MARKER: &str = ".droppedneedle-playlist";
/// Folder of rescan requests the host helper picks up, one file per user
/// folder: `<root>/.rescan/<folder>`.
pub const RESCAN_DIR: &str = ".rescan";
/// Prefix of names in flight.
const TEMP_PREFIX: &str = ".dn-tmp-";
/// The startup link probe's name.
const PROBE_NAME: &str = ".dn-link-probe";
/// Longest name, in bytes, extension included.
const MAX_NAME_BYTES: usize = 150;

/// One user's wanted tree.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WantedUser {
    /// User id.
    pub user_id: String,
    /// Display name, unsanitised.
    pub name: String,
    /// Playlists in a stable order.
    pub playlists: Vec<WantedPlaylist>,
}

/// One playlist's wanted folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WantedPlaylist {
    /// Playlist id.
    pub playlist_id: String,
    /// Playlist name, unsanitised.
    pub name: String,
    /// Every entry in playlist order; `None` where it has no local file.
    pub entries: Vec<Option<WantedEntry>>,
}

/// One entry with a local file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WantedEntry {
    /// Artist shown in the name.
    pub artist: String,
    /// Title shown in the name.
    pub title: String,
    /// The library file.
    pub source: PathBuf,
    /// The library file's size and mtime as indexed, so a replaced file
    /// changes the user's fingerprint.
    pub revision: (i64, i64),
}

/// What one reconcile changed for one user.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changes {
    /// New links.
    pub linked: u64,
    /// Links replaced because the library file changed.
    pub relinked: u64,
    /// Links removed.
    pub removed_files: u64,
    /// Folders made.
    pub created_dirs: u64,
    /// Folders removed.
    pub removed_dirs: u64,
    /// Folders renamed.
    pub renamed_dirs: u64,
    /// Entries whose library file is not there.
    pub missing_sources: u64,
    /// Operations that failed (logged).
    pub failures: u64,
}

impl Changes {
    /// Whether anything on disk changed.
    pub fn any(&self) -> bool {
        self.linked
            + self.relinked
            + self.removed_files
            + self.created_dirs
            + self.removed_dirs
            + self.renamed_dirs
            > 0
    }
}

/// Why links cannot be made in the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    /// The filesystem refuses hardlinks here (EXDEV, EPERM or ENOTSUP):
    /// the job stays off.
    Unsupported(String),
    /// Something else went wrong; try again later.
    Transient(String),
}

/// Whether `source` can be hardlinked into `root`: a link is made, its
/// inode compared, and removed.
pub fn probe(root: &Path, source: &Path) -> Result<(), ProbeFailure> {
    probe_with(root, source, |source, target| fs::hard_link(source, target))
}

/// [`probe`] with the link made by `link`.
pub fn probe_with(
    root: &Path,
    source: &Path,
    link: fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), ProbeFailure> {
    let target = root.join(PROBE_NAME);
    let _ = fs::remove_file(&target);
    if let Err(error) = link(source, &target) {
        let text = format!("link {} -> {}: {error}", source.display(), target.display());
        return Err(if refuses_links(&error) {
            ProbeFailure::Unsupported(text)
        } else {
            ProbeFailure::Transient(text)
        });
    }
    let same = match (fs::metadata(source), fs::symlink_metadata(&target)) {
        (Ok(a), Ok(b)) => a.ino() == b.ino() && a.dev() == b.dev(),
        _ => false,
    };
    let _ = fs::remove_file(&target);
    if same {
        Ok(())
    } else {
        Err(ProbeFailure::Unsupported(
            "the probe link has a different inode: links are copies here".to_owned(),
        ))
    }
}

/// Whether a failed link means the filesystem refuses hardlinks here
/// (EXDEV, EPERM, ENOTSUP or EMLINK), not a passing fault.
pub fn refuses_links(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EXDEV | libc::EPERM | libc::ENOTSUP | libc::EMLINK)
    )
}

/// A name safe on every share: NFC (Nextcloud refuses decomposed names), no `/ \ : * ? " < > |` or control
/// characters, no leading dots, no trailing dots or spaces, trimmed and
/// capped at `max` bytes. Empty becomes `fallback`.
pub fn sanitise(raw: &str, fallback: &str, max: usize) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    let cleaned: String = raw
        .nfc()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut name = tidy(&cleaned);
    if name.len() > max {
        let mut cut = max;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name = tidy(&name[..cut]);
    }
    if name.is_empty() {
        fallback.to_owned()
    } else {
        name
    }
}

fn tidy(name: &str) -> String {
    name.trim()
        .trim_start_matches('.')
        .trim_end_matches(['.', ' '])
        .trim()
        .to_owned()
}

/// Give each name a unique spelling (case-insensitively, and apart from
/// `taken`) by adding ` (2)`, ` (3)`… before the extension.
pub fn dedupe(names: &[String], taken: &HashSet<String>, keep_extension: bool) -> Vec<String> {
    let mut used: HashSet<String> = taken.iter().map(|name| name.to_lowercase()).collect();
    names
        .iter()
        .map(|name| {
            let (stem, ext) = match name.rfind('.').filter(|_| keep_extension) {
                Some(at) if at > 0 => (&name[..at], &name[at..]),
                _ => (name.as_str(), ""),
            };
            let mut candidate = name.clone();
            let mut n = 2;
            while used.contains(&candidate.to_lowercase()) {
                let suffix = format!(" ({n}){ext}");
                let room = MAX_NAME_BYTES.saturating_sub(suffix.len());
                let mut cut = stem.len().min(room);
                while !stem.is_char_boundary(cut) {
                    cut -= 1;
                }
                candidate = format!("{}{suffix}", &stem[..cut]);
                n += 1;
            }
            used.insert(candidate.to_lowercase());
            candidate
        })
        .collect()
}

/// The file names of one playlist: `NN - Artist - Title.ext`, NN its place
/// in the playlist (at least two digits), only for entries with a file.
pub fn file_names(entries: &[Option<WantedEntry>]) -> Vec<(String, PathBuf)> {
    let width = entries.len().to_string().len().max(2);
    let mut names = Vec::new();
    let mut sources = Vec::new();
    for (at, entry) in entries.iter().enumerate() {
        let Some(entry) = entry else { continue };
        let ext = entry
            .source
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| sanitise(&ext.to_lowercase(), "", 10))
            .filter(|ext| !ext.is_empty())
            .map(|ext| format!(".{ext}"))
            .unwrap_or_default();
        let stem = sanitise(
            &format!("{:0width$} - {} - {}", at + 1, entry.artist, entry.title),
            &format!("{:0width$}", at + 1),
            MAX_NAME_BYTES - ext.len(),
        );
        names.push(format!("{stem}{ext}"));
        sources.push(entry.source.clone());
    }
    dedupe(&names, &HashSet::new(), true)
        .into_iter()
        .zip(sources)
        .collect()
}

/// A folder's children: marked folders by id, other visible names.
struct Listing {
    owned: BTreeMap<String, String>,
    foreign: HashSet<String>,
}

fn read_marker(dir: &Path, marker: &str) -> Option<String> {
    fs::read_to_string(dir.join(marker))
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|id| !id.is_empty())
}

fn list(dir: &Path, marker: &str, changes: &mut Changes) -> io::Result<Listing> {
    let mut listing = Listing {
        owned: BTreeMap::new(),
        foreign: HashSet::new(),
    };
    for item in fs::read_dir(dir)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        let path = item.path();
        if name.starts_with(TEMP_PREFIX) {
            remove_owned_tree(&path, changes);
            let _ = fs::remove_file(&path);
            continue;
        }
        if name.starts_with('.') {
            continue;
        }
        let is_dir = item.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        match is_dir.then(|| read_marker(&path, marker)).flatten() {
            Some(id) if !listing.owned.contains_key(&id) => {
                listing.owned.insert(id, name);
            }
            _ => {
                listing.foreign.insert(name);
            }
        }
    }
    Ok(listing)
}

fn write_marker(dir: &Path, marker: &str, id: &str) -> io::Result<()> {
    let temp = dir.join(format!("{TEMP_PREFIX}{marker}"));
    fs::write(&temp, format!("{id}\n"))?;
    fs::rename(&temp, dir.join(marker))
}

/// Remove a folder this module made: links, markers and our sub-folders.
/// Anything else stays, and so does the folder then.
fn remove_owned_tree(dir: &Path, changes: &mut Changes) {
    let Ok(items) = fs::read_dir(dir) else { return };
    let owned = dir.join(USER_MARKER).exists()
        || dir.join(PLAYLIST_MARKER).exists()
        || dir
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(TEMP_PREFIX));
    if !owned {
        return;
    }
    for item in items.flatten() {
        let path = item.path();
        match item.file_type() {
            Ok(kind) if kind.is_dir() => remove_owned_tree(&path, changes),
            Ok(_) => {
                let marker = item.file_name() == USER_MARKER || item.file_name() == PLAYLIST_MARKER;
                if fs::remove_file(&path).is_ok() && !marker {
                    changes.removed_files += 1;
                }
            }
            Err(_) => {}
        }
    }
    if fs::remove_dir(dir).is_ok() {
        changes.removed_dirs += 1;
    }
}

/// Bring folders marked with `marker` under `parent` in line with
/// `wanted` (id, unsanitised name): gone ids removed, the rest renamed to
/// their deduplicated names or made. Returns each id's folder path; the
/// names of folders removed go into `gone`.
fn place_dirs(
    parent: &Path,
    marker: &str,
    wanted: &[(String, String)],
    changes: &mut Changes,
    gone: &mut Vec<String>,
) -> io::Result<HashMap<String, PathBuf>> {
    let listing = list(parent, marker, changes)?;
    let wanted_ids: HashSet<&str> = wanted.iter().map(|(id, _)| id.as_str()).collect();
    for (id, name) in &listing.owned {
        if !wanted_ids.contains(id.as_str()) {
            remove_owned_tree(&parent.join(name), changes);
            gone.push(name.clone());
        }
    }
    let names: Vec<String> = wanted
        .iter()
        .map(|(_, name)| sanitise(name, "Untitled", MAX_NAME_BYTES))
        .collect();
    let names = dedupe(&names, &listing.foreign, false);
    // Move every misnamed folder aside first so two can swap names.
    let mut aside: HashMap<String, PathBuf> = HashMap::new();
    for ((id, _), name) in wanted.iter().zip(&names) {
        if let Some(current) = listing.owned.get(id)
            && current != name
        {
            let temp = parent.join(format!("{TEMP_PREFIX}d-{}", aside.len()));
            fs::rename(parent.join(current), &temp)?;
            aside.insert(id.clone(), temp);
        }
    }
    let mut placed = HashMap::new();
    for ((id, _), name) in wanted.iter().zip(&names) {
        let path = parent.join(name);
        if let Some(temp) = aside.remove(id) {
            fs::rename(&temp, &path)?;
            changes.renamed_dirs += 1;
        } else if !listing.owned.contains_key(id) {
            fs::create_dir(&path)?;
            write_marker(&path, marker, id)?;
            changes.created_dirs += 1;
        }
        placed.insert(id.clone(), path);
    }
    Ok(placed)
}

/// Bring one playlist folder's links in line with `files`.
fn place_files(dir: &Path, files: &[(String, PathBuf)], changes: &mut Changes) -> io::Result<()> {
    let wanted: HashMap<&str, &Path> = files
        .iter()
        .map(|(name, source)| (name.as_str(), source.as_path()))
        .collect();
    for item in fs::read_dir(dir)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with(TEMP_PREFIX) {
            let _ = fs::remove_file(item.path());
            continue;
        }
        if name.starts_with('.') || !item.file_type()?.is_file() {
            continue;
        }
        if !wanted.contains_key(name.as_str()) {
            match fs::remove_file(item.path()) {
                Ok(()) => changes.removed_files += 1,
                Err(error) => {
                    tracing::warn!(%error, file = %item.path().display(), "playlist folders: could not remove a link");
                    changes.failures += 1;
                }
            }
        }
    }
    for (name, source) in files {
        let target = dir.join(name);
        let existing = fs::symlink_metadata(&target).ok();
        let Ok(library) = fs::metadata(source) else {
            changes.missing_sources += 1;
            if existing.is_some() && fs::remove_file(&target).is_ok() {
                changes.removed_files += 1;
            }
            continue;
        };
        if let Some(existing) = &existing
            && existing.ino() == library.ino()
            && existing.dev() == library.dev()
        {
            continue;
        }
        let temp = dir.join(format!("{TEMP_PREFIX}{name}"));
        let _ = fs::remove_file(&temp);
        let made = fs::hard_link(source, &temp).and_then(|()| fs::rename(&temp, &target));
        match made {
            Ok(()) if existing.is_some() => changes.relinked += 1,
            Ok(()) => changes.linked += 1,
            Err(error) => {
                let _ = fs::remove_file(&temp);
                tracing::warn!(%error, file = %target.display(), "playlist folders: could not link");
                changes.failures += 1;
            }
        }
    }
    Ok(())
}

/// The users that get a folder: those with at least one linkable entry,
/// each with only such playlists.
fn linkable(users: &[WantedUser]) -> Vec<WantedUser> {
    users
        .iter()
        .map(|user| WantedUser {
            playlists: user
                .playlists
                .iter()
                .filter(|playlist| playlist.entries.iter().any(Option::is_some))
                .cloned()
                .collect(),
            ..user.clone()
        })
        .filter(|user| !user.playlists.is_empty())
        .collect()
}

/// What one reconcile did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// Per user id: its folder name and what changed in it.
    pub users: BTreeMap<String, (String, Changes)>,
    /// What changed at the root itself (user folders made, renamed or
    /// removed).
    pub root: Changes,
    /// User folders removed, by name.
    pub gone: Vec<String>,
}

/// Reconcile the whole root against `users` (every user with playlists).
/// With `only`, the file work is limited to those user ids; folder names
/// and removals of gone users always cover everyone. Returns the changes
/// per user id and each user's folder name, plus the root's own changes.
pub fn reconcile(
    root: &Path,
    users: &[WantedUser],
    only: Option<&HashSet<String>>,
) -> io::Result<Reconciled> {
    let users = linkable(users);
    let mut root_changes = Changes::default();
    let wanted: Vec<(String, String)> = users
        .iter()
        .map(|user| (user.user_id.clone(), user.name.clone()))
        .collect();
    let mut gone = Vec::new();
    let placed = place_dirs(root, USER_MARKER, &wanted, &mut root_changes, &mut gone)?;
    let mut out = BTreeMap::new();
    for user in &users {
        let Some(dir) = placed.get(&user.user_id) else {
            continue;
        };
        let folder = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut changes = Changes::default();
        if only.is_none_or(|only| only.contains(&user.user_id)) {
            let wanted: Vec<(String, String)> = user
                .playlists
                .iter()
                .map(|playlist| (playlist.playlist_id.clone(), playlist.name.clone()))
                .collect();
            match place_dirs(dir, PLAYLIST_MARKER, &wanted, &mut changes, &mut Vec::new()) {
                Ok(dirs) => {
                    for playlist in &user.playlists {
                        let Some(path) = dirs.get(&playlist.playlist_id) else {
                            continue;
                        };
                        if let Err(error) =
                            place_files(path, &file_names(&playlist.entries), &mut changes)
                        {
                            tracing::warn!(%error, dir = %path.display(), "playlist folders: playlist failed");
                            changes.failures += 1;
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, dir = %dir.display(), "playlist folders: user failed");
                    changes.failures += 1;
                }
            }
        }
        out.insert(user.user_id.clone(), (folder, changes));
    }
    Ok(Reconciled {
        users: out,
        root: root_changes,
        gone,
    })
}

/// Ask the host helper to rescan `folder`: `<root>/.rescan/<folder>`
/// holding `stamp`, written via a temporary name.
pub fn request_rescan(root: &Path, folder: &str, stamp: i64) -> io::Result<()> {
    let dir = root.join(RESCAN_DIR);
    fs::create_dir_all(&dir)?;
    let temp = dir.join(format!("{TEMP_PREFIX}{folder}"));
    fs::write(&temp, format!("{stamp}\n"))?;
    fs::rename(&temp, dir.join(folder))
}

/// The users manifest's name under the root.
pub const MANIFEST: &str = ".users.json";

/// Write `<root>/.users.json` via a temporary name, only when its bytes
/// change. Returns whether it was written.
pub fn write_manifest<T: serde::Serialize>(root: &Path, manifest: &T) -> io::Result<bool> {
    let mut text = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    text.push(b'\n');
    let path = root.join(MANIFEST);
    if fs::read(&path).is_ok_and(|old| old == text) {
        return Ok(false);
    }
    let temp = root.join(format!("{TEMP_PREFIX}{MANIFEST}"));
    fs::write(&temp, &text)?;
    fs::rename(&temp, &path)?;
    Ok(true)
}
