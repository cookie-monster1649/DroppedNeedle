//! The source tree: one top-level folder per playlist, any folders below
//! it, audio files anywhere beneath. NAS housekeeping (`@eaDir`,
//! `.DS_Store`, `*@Syno*`) and hidden names are never entered.

use std::path::{Path, PathBuf};

/// Audio the walk takes, by extension.
const AUDIO: &[&str] = &["mp3", "m4a", "flac", "ogg", "opus", "wav", "aac"];
/// How deep below a playlist folder the walk goes.
const MAX_DEPTH: usize = 8;

/// One audio file of the source tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The playlist: the top-level folder's name.
    pub playlist: String,
    /// The file's path below the source root, `/`-separated.
    pub rel: String,
    /// The file's full path.
    pub path: PathBuf,
    pub size: u64,
    /// Modification time, seconds since the epoch.
    pub modified: i64,
}

/// Whether a name is NAS or desktop housekeeping, or hidden.
pub fn ignored(name: &str) -> bool {
    name.starts_with('.') || name == "@eaDir" || name.contains("@Syno") || name == "#recycle"
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| AUDIO.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Every audio file below `root`, by playlist, in path order. Links are
/// never followed. Blocking.
///
/// # Errors
///
/// The root cannot be listed.
pub fn walk(root: &Path) -> std::io::Result<Vec<Found>> {
    let mut out = Vec::new();
    let mut playlists: Vec<(String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if ignored(&name) || !entry.file_type()?.is_dir() {
            continue;
        }
        playlists.push((name, entry.path()));
    }
    playlists.sort();
    for (playlist, dir) in playlists {
        below(root, &playlist, &dir, 0, &mut out);
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn below(root: &Path, playlist: &str, dir: &Path, depth: usize, out: &mut Vec<Found>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        tracing::warn!(dir = %dir.display(), "playlist import: a folder does not list");
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if ignored(&name) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let path = entry.path();
        if meta.is_dir() {
            below(root, playlist, &path, depth + 1, out);
        } else if meta.is_file() && is_audio(&path) {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel = rel
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let modified = meta
                .modified()
                .ok()
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |at| i64::try_from(at.as_secs()).unwrap_or(i64::MAX));
            out.push(Found {
                playlist: playlist.to_owned(),
                rel,
                path,
                size: meta.len(),
                modified,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_walk_takes_audio_by_playlist_and_skips_housekeeping() {
        let root = std::env::temp_dir().join(format!("dn-pi-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in [
            "12AM/12AM Part 1/deep",
            "12AM/@eaDir/x",
            "D&B",
            "@eaDir",
            "Psy@SynoResource",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "12AM/12AM Part 1/a.mp3",
            "12AM/12AM Part 1/deep/b.M4A",
            "12AM/12AM Part 1/cover.jpg",
            "12AM/.DS_Store",
            "12AM/@eaDir/x/c.mp3",
            "12AM/d@SynoEAStream.mp3",
            "D&B/e.flac",
            "@eaDir/f.mp3",
            "Psy@SynoResource/g.mp3",
            "loose.mp3",
        ] {
            std::fs::write(root.join(file), b"x").unwrap();
        }
        let found = walk(&root).unwrap();
        let got: Vec<(&str, &str)> = found
            .iter()
            .map(|f| (f.playlist.as_str(), f.rel.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("12AM", "12AM/12AM Part 1/a.mp3"),
                ("12AM", "12AM/12AM Part 1/deep/b.M4A"),
                ("D&B", "D&B/e.flac"),
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
