//! Fork: the import's last step, "import by tags". Every file still left
//! after the resolver, the playlist match and the edition retries (not a
//! duplicate, not already in the library, tags read) goes into the
//! library as it is tagged, so every song of the folder ends up there.
//!
//! The library's own path for files with no MusicBrainz identity is used:
//! the file is copied into a library folder at
//! `<album artist, else artist>/<album>/<file name>` and the scan the
//! folder watcher asks for indexes it. The scan groups such files by their
//! album artist and album tags (the scan's `tag:` album grouping key) into a
//! local album, identification tries it like any other album, and one it
//! cannot match keeps its own tags under the library's `unmatched` match
//! flag, where a person matches it later. Nothing goes to the held list.
//!
//! The copy is staged under the library root's
//! `.droppedneedle-management-` prefix (the scan skips it) and renamed into
//! place, so the scan never sees half a file. The source is never changed.
//! The file's outcome is `tags_only` and its library path is kept in the
//! state, so a re-run never copies it twice; a later run that resolves the
//! file properly leaves the tag-only copy as it is and the report says so.
//!
//! On by default; `PLAYLIST_IMPORT_TAGS_FALLBACK=false` turns it off.

use std::path::{Path, PathBuf};

use super::{FileState, Resolution};

/// The switch's environment variable.
pub const ENV: &str = "PLAYLIST_IMPORT_TAGS_FALLBACK";
/// The outcome of a file imported by its tags.
pub const OUTCOME: &str = "tags_only";
/// What the state and report record as the file's way in.
pub const VIA: &str = "tags_only";
/// The staging folder inside the library root (the scan skips it).
const STAGING: &str = ".droppedneedle-management-playlist-tags";

/// Whether the step is on: unset or anything but a "false" word.
pub fn enabled() -> bool {
    on(std::env::var(ENV).ok().as_deref())
}

fn on(value: Option<&str>) -> bool {
    !matches!(
        value
            .map(|text| text.trim().to_ascii_lowercase())
            .as_deref(),
        Some("false" | "0" | "no" | "off")
    )
}

/// Whether `outcome` is a tag-only import.
pub fn is_tags_only(outcome: Option<&str>) -> bool {
    outcome == Some(OUTCOME)
}

/// Whether the step takes the file: tags read, not a duplicate, not in the
/// library, looked up and left unresolved (nothing found, refused) or
/// resolved but not placed (`unmatched:`/`refused:` import outcomes).
pub fn takes(file: &FileState) -> bool {
    if file.tags.is_none() || file.duplicate_of.is_some() || file.in_library {
        return false;
    }
    let outcome = file.outcome.as_deref();
    let open = outcome
        .is_none_or(|outcome| outcome.starts_with("unmatched:") || outcome.starts_with("refused:"));
    open && match &file.resolved {
        None => false,
        Some(Resolution::NotFound | Resolution::Refused { .. }) => true,
        Some(Resolution::Found(_)) => outcome.is_some(),
    }
}

/// Why a file [`takes`] took goes in by its tags: its import outcome,
/// else `not_found` or `refused`.
pub fn reason(file: &FileState) -> String {
    match (&file.outcome, &file.resolved) {
        (Some(outcome), _) => outcome.clone(),
        (None, Some(Resolution::Refused { .. })) => "refused".to_owned(),
        _ => "not_found".to_owned(),
    }
}

/// Whether a tag-only file was unresolved when it went in (not a resolved
/// file that did not pair).
pub fn was_unresolved(file: &FileState) -> bool {
    matches!(file.tags_reason.as_deref(), Some("not_found" | "refused"))
}

/// A path part safe on every filesystem the library sits on.
fn part(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    let short: String = trimmed.chars().take(120).collect();
    if short.trim().is_empty() {
        fallback.to_owned()
    } else {
        short.trim().to_owned()
    }
}

/// Where a file goes below the library root:
/// `<album artist, else artist>/<album>/<file name>`.
pub fn destination(tags: &super::Tags, file_name: &str) -> PathBuf {
    let artist = tags
        .album_artist
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&tags.artist);
    PathBuf::from(part(artist, "Unknown Artist"))
        .join(part(&tags.album, "Unknown Album"))
        .join(part(file_name, "track"))
}

/// Copy `source` into `root` at `wanted` (below the root), staged and
/// renamed. A file already there of the same size is taken as this one
/// (a run cut off after the rename); another file there gives the copy a
/// ` (2)`, ` (3)`… name. Answers the path below the root. Blocking.
pub fn place(root: &Path, source: &Path, wanted: &Path, key: &str) -> std::io::Result<PathBuf> {
    let size = std::fs::metadata(source)?.len();
    let mut rel = wanted.to_path_buf();
    let stem = wanted
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = wanted
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    let mut n = 1;
    loop {
        match std::fs::metadata(root.join(&rel)) {
            Ok(there) if there.len() == size => return Ok(rel),
            Ok(_) => {
                n += 1;
                rel = wanted.with_file_name(format!("{stem} ({n}){ext}"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error),
        }
    }
    let staging = root.join(STAGING);
    std::fs::create_dir_all(&staging)?;
    let staged = staging.join(format!("{key}.part"));
    std::fs::copy(source, &staged)?;
    let dest = root.join(&rel);
    if let Some(parent) = dest.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&staged, &dest) {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    let _ = std::fs::remove_dir(&staging);
    Ok(rel)
}

/// A short staging key for a file: hex of a hash of its path.
pub fn key_of(rel: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    rel.hash(&mut hasher);
    format!("{:016x}-{}", hasher.finish(), std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist_import::Tags;
    use crate::playlist_import::resolve::{Resolved, Via};

    fn tags() -> Tags {
        Tags {
            title: "Song".into(),
            artist: "Guest feat. X".into(),
            album: "An: Album?".into(),
            album_artist: Some("Main Act".into()),
            ..Tags::default()
        }
    }

    #[test]
    fn the_switch_is_on_unless_set_false() {
        assert!(on(None));
        assert!(on(Some("true")));
        assert!(on(Some("")));
        for off in ["false", "FALSE", " 0 ", "no", "off"] {
            assert!(!on(Some(off)), "{off}");
        }
    }

    #[test]
    fn files_go_under_album_artist_else_artist_and_album() {
        assert_eq!(
            destination(&tags(), "01 Song.mp3"),
            PathBuf::from("Main Act/An_ Album_/01 Song.mp3")
        );
        let mut bare = tags();
        bare.album_artist = Some("  ".into());
        bare.album = String::new();
        assert_eq!(
            destination(&bare, "x.flac"),
            PathBuf::from("Guest feat. X/Unknown Album/x.flac")
        );
        bare.artist = "..".into();
        assert_eq!(
            destination(&bare, "x.flac"),
            PathBuf::from("Unknown Artist/Unknown Album/x.flac")
        );
    }

    #[test]
    fn only_what_nothing_placed_is_taken() {
        let found = Resolution::Found(Resolved {
            via: Via::NameMusicbrainz,
            album: "rg".into(),
            stand_in: None,
            recording: String::new(),
        });
        let file = |resolved: Option<Resolution>, outcome: Option<&str>| FileState {
            tags: Some(tags()),
            resolved,
            outcome: outcome.map(str::to_owned),
            ..FileState::default()
        };
        assert!(takes(&file(Some(Resolution::NotFound), None)));
        assert!(takes(&file(
            Some(Resolution::Refused {
                reason: "403".into(),
                tries: 2
            }),
            None
        )));
        assert!(takes(&file(
            Some(found.clone()),
            Some("unmatched:not_paired")
        )));
        assert!(takes(&file(Some(found.clone()), Some("refused:excluded"))));
        // Placed, held, already tag-only, not looked up, or found and
        // waiting for the import: not taken.
        assert!(!takes(&file(Some(found.clone()), Some("imported"))));
        assert!(!takes(&file(Some(found.clone()), Some("held:quality"))));
        assert!(!takes(&file(Some(Resolution::NotFound), Some(OUTCOME))));
        assert!(!takes(&file(None, None)));
        assert!(!takes(&file(Some(found), None)));
        // Duplicates, songs the library holds and unreadable files.
        let mut dup = file(Some(Resolution::NotFound), None);
        dup.duplicate_of = Some("other".into());
        assert!(!takes(&dup));
        let mut held = file(Some(Resolution::NotFound), None);
        held.in_library = true;
        assert!(!takes(&held));
        let mut unread = file(Some(Resolution::NotFound), None);
        unread.tags = None;
        assert!(!takes(&unread));
    }

    #[test]
    fn a_copy_is_placed_once_and_a_different_file_gets_its_own_name() {
        let base = std::env::temp_dir().join(format!("dn-pi-tagsfb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (src, root) = (base.join("src"), base.join("lib"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(src.join("a.mp3"), b"aaaa").unwrap();
        std::fs::write(src.join("b.mp3"), b"bbbbbbbb").unwrap();
        let wanted = PathBuf::from("Main Act/Album/01.mp3");

        let first = place(&root, &src.join("a.mp3"), &wanted, "k1").unwrap();
        assert_eq!(first, wanted);
        assert_eq!(std::fs::read(root.join(&first)).unwrap(), b"aaaa");
        // Again (a re-run cut off after the copy): the same file, no copy.
        assert_eq!(
            place(&root, &src.join("a.mp3"), &wanted, "k1").unwrap(),
            wanted
        );
        // Another file wanting the same name.
        let second = place(&root, &src.join("b.mp3"), &wanted, "k2").unwrap();
        assert_eq!(second, PathBuf::from("Main Act/Album/01 (2).mp3"));
        // The source is untouched and no staging is left behind.
        assert_eq!(std::fs::read(src.join("a.mp3")).unwrap(), b"aaaa");
        assert!(!root.join(STAGING).exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
