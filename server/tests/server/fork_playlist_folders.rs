//! Fork: live playlist folders. Each user's playlists are kept as folders
//! of hardlinks to the library files; nothing is ever copied, and a
//! reconcile with nothing changed changes nothing.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::jobs::playlist_folders::tree;
use droppedneedle_kernel::jobs::playlist_folders::{PlaylistFolderJob, Status, TickReport};
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
use droppedneedle_kernel::runtime_config::sections::PlaylistFolders;
use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto, LegacyConfig};
use droppedneedle_kernel::test_support::ManualClock;

use crate::common::{ScratchDir, migrated_db};

/// EXDEV, the same number on Linux and macOS.
const EXDEV: i32 = 18;

struct Rig {
    _dir: ScratchDir,
    db: Db,
    config: ConfigStore,
    library: PathBuf,
    root: PathBuf,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let dir = ScratchDir::new(tag);
        let path = dir.join("app.db");
        migrated_db(&path);
        let db = Db::open_for_tests(&path).expect("database opens");
        let config = ConfigStore::open_blocking(
            db.clone(),
            Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
            Arc::new(ManualClock::at(1_000)),
            &LegacyConfig::None,
        )
        .expect("settings open");
        let library = dir.join("music");
        let root = dir.join("music_playlists");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let rig = Self {
            _dir: dir,
            db,
            config,
            library,
            root,
        };
        rig.set_root(&rig.root.clone());
        rig.sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) VALUES \
             ('u-jj', 'JJ Monester', 'user', 1), ('u-admin', 'admin', 'admin', 1); \
             INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('art-1', 'Aurora', 'aurora', 'group', 1, 1); \
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) VALUES \
             ('alb-1', 'r1', 'g1', 'First Light', 'first light', 'art-1', 'automatic', 1, 1);",
        );
        rig
    }

    fn set_root(&self, root: &Path) {
        self.config
            .save_blocking(PlaylistFolders {
                path: root.display().to_string(),
                ..PlaylistFolders::default()
            })
            .expect("playlist folders setting saves");
    }

    fn sql(&self, text: &str) {
        let text = text.to_owned();
        self.db
            .write_blocking(Lane::Foreground, "fork-playlist-folders.seed", move |tx| {
                tx.execute_batch(&text).map_err(DbError::from)
            })
            .expect("seed writes");
    }

    /// A library file and its indexed row.
    fn track(&self, id: &str, artist: &str, title: &str, file: &str) -> PathBuf {
        let path = self.library.join(file);
        std::fs::write(&path, format!("audio of {title}")).unwrap();
        self.sql(&format!(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             artist_name, album_title, album_title_folded, file_format, availability, \
             ingest_source, imported_at, membership_source) VALUES \
             ('{id}', 'alb-1', 'r1', '{path}', '{file}', 'h-{id}', 1, 1, 's-{id}', '{title}', \
             '{title}', '{artist}', 'First Light', 'first light', 'flac', 'indexed', 'scan', 1, \
             'automatic');",
            path = path.display(),
        ));
        path
    }

    fn playlist(&self, id: &str, user: &str, name: &str) {
        self.sql(&format!(
            "INSERT INTO catalog_playlists (id, name, created_at, updated_at, user_id) \
             VALUES ('{id}', '{name}', 1, 1, '{user}');"
        ));
    }

    /// One entry; `file` is the library track id, or `None` for a track
    /// with no local file.
    fn entry(&self, playlist: &str, position: i64, artist: &str, title: &str, file: Option<&str>) {
        let file = file.map_or("NULL".to_owned(), |id| format!("'{id}'"));
        self.sql(&format!(
            "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, track_name, \
             artist_name, album_name, source_type, created_at, library_file_id) VALUES \
             ('{playlist}-{position}', '{playlist}', {position}, '{title}', '{artist}', \
             'First Light', 'local', 1, {file});"
        ));
    }

    fn job(&self) -> PlaylistFolderJob {
        PlaylistFolderJob::new(
            self.db.clone(),
            &self.config,
            Arc::new(ManualClock::at(5_000)),
        )
    }

    /// Every file under the root, relative, sorted, markers and rescan
    /// requests left out.
    fn tree(&self) -> Vec<String> {
        let mut out = Vec::new();
        walk(&self.root, &self.root, &mut out);
        out.sort();
        out
    }
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
    for item in std::fs::read_dir(dir).unwrap() {
        let item = item.unwrap();
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = item.path();
        if item.file_type().unwrap().is_dir() {
            walk(base, &path, out);
        } else {
            out.push(path.strip_prefix(base).unwrap().display().to_string());
        }
    }
}

fn nothing_changed(report: &TickReport) -> bool {
    report.rescans.is_empty() && report.changes.values().all(|changes| !changes.any())
}

fn same_file(a: &Path, b: &Path) -> bool {
    let (a, b) = (std::fs::metadata(a).unwrap(), std::fs::metadata(b).unwrap());
    a.ino() == b.ino() && a.dev() == b.dev()
}

/// The journey: links made in playlist order with gaps where an entry has
/// no file, as hardlinks; a second pass changes nothing; then a rename,
/// a removed entry, a reorder, a replaced library file and a deleted
/// playlist each land, and a user with nothing left loses the folder.
#[test]
fn folders_follow_playlists_as_hardlinks() {
    let rig = Rig::new("pl-folders-journey");
    let opener = rig.track("t1", "Aurora", "Opener", "opener.flac");
    let closer = rig.track("t2", "Aurora", "Closer", "closer.mp3");
    rig.playlist("p1", "u-jj", "Road Trip");
    rig.entry("p1", 0, "Aurora", "Opener", Some("t1"));
    rig.entry("p1", 1, "Nobody", "Not Here", None);
    rig.entry("p1", 2, "Aurora", "Closer", Some("t2"));
    rig.playlist("p2", "u-jj", "Chill");
    rig.entry("p2", 0, "Aurora", "Closer", Some("t2"));
    // The admin's only playlist has nothing local: no folder for them.
    rig.playlist("p3", "u-admin", "Wishlist");
    rig.entry("p3", 0, "Nobody", "Not Here", None);
    let job = rig.job();

    let first = job.tick(true).expect("first reconcile runs");
    assert_eq!(
        rig.tree(),
        vec![
            "JJ Monester/Chill/01 - Aurora - Closer.mp3",
            "JJ Monester/Road Trip/01 - Aurora - Opener.flac",
            "JJ Monester/Road Trip/03 - Aurora - Closer.mp3",
        ]
    );
    assert_eq!(first.changes["u-jj"].linked, 3);
    assert_eq!(first.rescans, vec!["JJ Monester"]);
    assert!(rig.root.join(".rescan/JJ Monester").is_file());
    let linked = rig
        .root
        .join("JJ Monester/Road Trip/01 - Aurora - Opener.flac");
    assert!(same_file(&linked, &opener), "a hardlink, not a copy");
    assert!(std::fs::metadata(&closer).unwrap().nlink() >= 3);
    assert!(matches!(job.status(), Status::Ready(_)));

    let second = job.tick(true).expect("second reconcile runs");
    assert!(
        nothing_changed(&second),
        "a second pass changes nothing: {second:?}"
    );

    // Rename, drop the opener, swap the order of what is left.
    rig.sql(
        "UPDATE catalog_playlists SET name = 'Long Drive' WHERE id = 'p1'; \
         DELETE FROM catalog_playlist_tracks WHERE id = 'p1-0'; \
         UPDATE catalog_playlist_tracks SET position = 5 WHERE id = 'p1-1'; \
         UPDATE catalog_playlist_tracks SET position = 1 WHERE id = 'p1-2'; \
         UPDATE catalog_playlist_tracks SET position = 2 WHERE id = 'p1-1';",
    );
    let third = job.tick(true).unwrap();
    assert_eq!(
        rig.tree(),
        vec![
            "JJ Monester/Chill/01 - Aurora - Closer.mp3",
            "JJ Monester/Long Drive/01 - Aurora - Closer.mp3",
        ]
    );
    assert_eq!(third.changes["u-jj"].renamed_dirs, 1);
    assert!(!rig.root.join("JJ Monester/Road Trip").exists());

    // An upgrade replaces the library file: a new inode, so a relink.
    let replacement = rig.library.join(".swap");
    std::fs::write(&replacement, "better audio").unwrap();
    std::fs::rename(&replacement, &closer).unwrap();
    let fourth = job.tick(true).unwrap();
    assert_eq!(fourth.changes["u-jj"].relinked, 2);
    assert!(same_file(
        &rig.root.join("JJ Monester/Chill/01 - Aurora - Closer.mp3"),
        &closer
    ));
    assert!(nothing_changed(&job.tick(true).unwrap()));

    // A deleted playlist loses its folder; with none left, the user does.
    rig.sql("DELETE FROM catalog_playlists WHERE id = 'p2';");
    job.tick(true).unwrap();
    assert_eq!(
        rig.tree(),
        vec!["JJ Monester/Long Drive/01 - Aurora - Closer.mp3"]
    );
    std::fs::remove_file(rig.root.join(".rescan/JJ Monester")).unwrap();
    rig.sql("DELETE FROM catalog_playlists WHERE id = 'p1';");
    let last = job.tick(true).unwrap();
    assert!(rig.tree().is_empty());
    assert!(!rig.root.join("JJ Monester").exists());
    assert_eq!(
        last.rescans,
        vec!["JJ Monester"],
        "the gone folder is rescanned"
    );
    assert!(
        closer.is_file() && opener.is_file(),
        "library files untouched"
    );
}

/// Links removed or replaced by hand are put back; a file someone else
/// left in the root stays.
#[test]
fn reconcile_repairs_the_disk_and_leaves_foreign_files() {
    let rig = Rig::new("pl-folders-repair");
    let opener = rig.track("t1", "Aurora", "Opener", "opener.flac");
    rig.playlist("p1", "u-jj", "Road Trip");
    rig.entry("p1", 0, "Aurora", "Opener", Some("t1"));
    std::fs::write(rig.root.join("README.txt"), "mine").unwrap();
    let job = rig.job();
    job.tick(true).unwrap();
    let link = rig
        .root
        .join("JJ Monester/Road Trip/01 - Aurora - Opener.flac");

    std::fs::remove_file(&link).unwrap();
    assert_eq!(job.tick(true).unwrap().changes["u-jj"].linked, 1);
    std::fs::remove_file(&link).unwrap();
    std::fs::write(&link, "a stray copy").unwrap();
    assert_eq!(job.tick(true).unwrap().changes["u-jj"].relinked, 1);
    assert!(same_file(&link, &opener));
    std::fs::write(rig.root.join("JJ Monester/Road Trip/stray.flac"), "x").unwrap();
    assert_eq!(job.tick(true).unwrap().changes["u-jj"].removed_files, 1);
    assert!(rig.root.join("README.txt").is_file());
}

/// Unsafe characters become `_`, names are trimmed and capped, and names
/// that clash (case aside) are told apart.
#[test]
fn names_are_sanitised_and_deduplicated() {
    let rig = Rig::new("pl-folders-names");
    rig.track("t1", "AC/DC", "Who? Me: \"Yes\" <no> | *", "t1.flac");
    rig.playlist("p1", "u-jj", "  a/b\\c:*?\"<>|  ");
    rig.entry("p1", 0, "AC/DC", "x", Some("t1"));
    rig.playlist("p2", "u-jj", "Mix");
    rig.entry("p2", 0, "AC/DC", "x", Some("t1"));
    rig.playlist("p3", "u-jj", "mix");
    rig.entry("p3", 0, "AC/DC", "x", Some("t1"));
    let long = "L".repeat(400);
    rig.playlist("p4", "u-jj", &long);
    rig.entry("p4", 0, "AC/DC", "x", Some("t1"));
    let job = rig.job();
    job.tick(true).unwrap();

    let folders: Vec<String> = std::fs::read_dir(rig.root.join("JJ Monester"))
        .unwrap()
        .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    assert!(folders.contains(&"a_b_c_______".to_owned()), "{folders:?}");
    assert!(folders.contains(&"Mix".to_owned()));
    assert!(folders.contains(&"mix (2)".to_owned()));
    assert!(
        folders
            .iter()
            .any(|name| name.starts_with("LLL") && name.len() <= 150)
    );
    for name in rig.tree() {
        let file = name.rsplit('/').next().unwrap();
        assert!(
            !file.contains([':', '*', '?', '"', '<', '>', '|', '\\']),
            "{file}"
        );
    }
    assert!(
        rig.tree()
            .contains(&"JJ Monester/Mix/01 - AC_DC - Who_ Me_ _Yes_ _no_ _ _.flac".to_owned())
    );
    assert!(nothing_changed(&job.tick(true).unwrap()));

    assert_eq!(tree::sanitise("  ..hidden.  ", "x", 150), "hidden");
    assert_eq!(tree::sanitise("///", "x", 150), "___");
    assert_eq!(tree::sanitise("   ", "Untitled", 150), "Untitled");
    assert_eq!(tree::sanitise("ééé", "x", 3), "é");
    // Decomposed (NFD) Hangul, as some tags carry it, comes out composed.
    assert_eq!(
        tree::sanitise("\u{110b}\u{1173}\u{11b7}", "x", 150),
        "\u{c74c}"
    );
}

/// A change waits one quiet tick before it lands (the debounce).
#[test]
fn a_change_lands_after_one_quiet_tick() {
    let rig = Rig::new("pl-folders-debounce");
    rig.track("t1", "Aurora", "Opener", "opener.flac");
    rig.track("t2", "Aurora", "Closer", "closer.flac");
    rig.playlist("p1", "u-jj", "Road Trip");
    rig.entry("p1", 0, "Aurora", "Opener", Some("t1"));
    let job = rig.job();
    job.tick(false).unwrap();
    assert_eq!(rig.tree().len(), 1);

    rig.entry("p1", 1, "Aurora", "Closer", Some("t2"));
    let seen = job.tick(false).unwrap();
    assert!(
        seen.reconciled.is_empty(),
        "the first tick only notes the change"
    );
    assert_eq!(rig.tree().len(), 1);
    let landed = job.tick(false).unwrap();
    assert_eq!(landed.reconciled, vec!["u-jj"]);
    assert_eq!(rig.tree().len(), 2);
    assert!(job.tick(false).unwrap().reconciled.is_empty());
}

/// A filesystem that refuses hardlinks turns the job off: nothing is made,
/// and above all nothing is copied.
#[test]
fn refused_links_disable_the_job() {
    let rig = Rig::new("pl-folders-refused");
    rig.track("t1", "Aurora", "Opener", "opener.flac");
    rig.playlist("p1", "u-jj", "Road Trip");
    rig.entry("p1", 0, "Aurora", "Opener", Some("t1"));
    let job = rig
        .job()
        .with_probe_link(|_, _| Err(std::io::Error::from_raw_os_error(EXDEV)));

    let report = job
        .tick(true)
        .expect("a refused probe is not a failed tick");
    assert_eq!(report, TickReport::default());
    assert!(matches!(job.status(), Status::Disabled(_, why) if why.contains("link")));
    assert!(job.tick(true).unwrap().reconciled.is_empty());
    assert!(rig.tree().is_empty());
    assert!(!rig.root.join("JJ Monester").exists());

    assert!(tree::refuses_links(&std::io::Error::from_raw_os_error(
        EXDEV
    )));
    assert!(!tree::refuses_links(&std::io::Error::from_raw_os_error(2)));
}

/// The root may not sit in or over a library folder, and empty is off.
#[test]
fn a_root_inside_the_library_or_empty_is_off() {
    let rig = Rig::new("pl-folders-overlap");
    rig.track("t1", "Aurora", "Opener", "opener.flac");
    rig.playlist("p1", "u-jj", "Road Trip");
    rig.entry("p1", 0, "Aurora", "Opener", Some("t1"));
    let root: droppedneedle_kernel::runtime_config::secret_sections::LibraryRoot =
        serde_json::from_value(serde_json::json!({
            "id": "r1",
            "path": rig.library.display().to_string(),
            "label": "Music",
        }))
        .unwrap();
    rig.config
        .save_secret_blocking(TypedLibrary {
            library_roots: vec![root],
            ..TypedLibrary::default()
        })
        .expect("library settings save");
    let inside = rig.library.join("playlists");
    std::fs::create_dir_all(&inside).unwrap();
    rig.set_root(&inside);
    let job = rig.job();
    let refused = job
        .tick(true)
        .expect_err("a root in the library is refused");
    assert!(refused.contains("overlaps"), "{refused}");
    assert!(matches!(job.status(), Status::Disabled(..)));
    assert_eq!(std::fs::read_dir(&inside).unwrap().count(), 0);

    rig.set_root(Path::new(""));
    assert_eq!(job.tick(true).unwrap(), TickReport::default());
    assert_eq!(job.status(), Status::Off);

    // While the setting is empty the environment's root is used; the
    // setting wins once saved.
    let job = rig
        .job()
        .with_fallback_root(&rig.root.display().to_string());
    job.tick(true).unwrap();
    assert_eq!(
        rig.tree(),
        vec!["JJ Monester/Road Trip/01 - Aurora - Opener.flac"]
    );
    rig.set_root(&inside);
    assert!(job.tick(true).is_err());
}

/// `.users.json` lists each user with a folder, with the sign-in names a
/// host helper maps to file-share accounts (never the display name alone),
/// and is rewritten only when it changes.
#[test]
fn the_users_manifest_lists_folders_and_identities() {
    let rig = Rig::new("pl-folders-manifest");
    rig.sql(
        "UPDATE auth_users SET username = 'jj-monester', email = 'jj@example.test' \
         WHERE id = 'u-jj'; \
         INSERT INTO auth_providers (id, user_id, provider, provider_uid, created_at) VALUES \
         ('ap-1', 'u-jj', 'oidc', 'sub-123', 1), \
         ('ap-2', 'u-jj', 'local', 'jj-monester', 1);",
    );
    rig.track("t1", "Aurora", "Opener", "opener.flac");
    rig.playlist("p1", "u-jj", "Road Trip");
    rig.entry("p1", 0, "Aurora", "Opener", Some("t1"));
    // The admin has only an unlinkable entry: no folder, not listed.
    rig.playlist("p3", "u-admin", "Wishlist");
    rig.entry("p3", 0, "Nobody", "Not Here", None);
    let job = rig.job();
    job.tick(true).unwrap();

    let path = rig.root.join(".users.json");
    let read = || -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    };
    assert_eq!(
        read(),
        serde_json::json!([{
            "user_id": "u-jj",
            "username": "jj-monester",
            "display_name": "JJ Monester",
            "folder": "JJ Monester",
            "email": "jj@example.test",
            "sso": [{"provider": "oidc", "subject": "sub-123"}],
            "nextcloud_user": null,
        }])
    );
    let inode = std::fs::metadata(&path).unwrap().ino();
    job.tick(true).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().ino(),
        inode,
        "unchanged, not rewritten"
    );

    rig.config
        .save_blocking(PlaylistFolders {
            path: rig.root.display().to_string(),
            nextcloud_users: [("jj-monester".to_owned(), "jj.nc".to_owned())].into(),
        })
        .unwrap();
    job.tick(true).unwrap();
    assert_eq!(read()[0]["nextcloud_user"], "jj.nc");

    rig.sql("DELETE FROM catalog_playlists WHERE id = 'p1';");
    job.tick(true).unwrap();
    assert_eq!(read(), serde_json::json!([]));
}
