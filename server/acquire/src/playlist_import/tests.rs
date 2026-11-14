//! The playlist-folder import's own tests: tags read from small tagged
//! fixtures, duplicates set aside, and the report's counts.

use super::*;
use crate::playlist_import::resolve::{Resolved, Via};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/library")
        .join(name)
}

/// A source tree of two playlists: a tagged MP3 and M4A in the first,
/// the same MP3 again in the second, plus NAS housekeeping.
fn tree(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("dn-pi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for dir in ["12AM/12AM Part 1", "D&B/Part 2", "@eaDir"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    for (from, to) in [
        ("management_full.mp3", "12AM/12AM Part 1/01 song.mp3"),
        ("m4a_full_01.m4a", "12AM/12AM Part 1/02 other.m4a"),
        ("management_full.mp3", "D&B/Part 2/same song.mp3"),
        ("mp3_full_01.mp3", "@eaDir/thumb.mp3"),
    ] {
        std::fs::copy(fixture(from), root.join(to)).unwrap();
    }
    root
}

fn state_of(root: &Path) -> State {
    let found = walk::walk(root).unwrap();
    let mut state = State::default();
    for (rel, tags) in read_tags(root, &found) {
        let playlist = found
            .iter()
            .find(|file| file.rel == rel)
            .map(|file| file.playlist.clone())
            .unwrap();
        state.files.insert(
            rel,
            FileState {
                playlist,
                tags,
                ..FileState::default()
            },
        );
    }
    state
}

#[test]
fn tags_isrc_and_barcode_are_read_from_the_files() {
    let root = tree("tags");
    let state = state_of(&root);
    assert_eq!(state.files.len(), 3);
    let mp3 = state.files["12AM/12AM Part 1/01 song.mp3"]
        .tags
        .clone()
        .unwrap();
    assert!(!mp3.title.is_empty());
    assert!(!mp3.artist.is_empty());
    assert_eq!(mp3.isrc.as_deref(), Some("GBABC2400001"));
    assert_eq!(mp3.barcode.as_deref(), Some("1234567890123"));
    assert!(mp3.seconds.is_some());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_same_song_in_two_playlists_is_imported_once() {
    let root = tree("dupes");
    let mut state = state_of(&root);
    mark_duplicates(&mut state);
    let dupes: Vec<(&String, &Option<String>)> = state
        .files
        .iter()
        .filter(|(_, file)| file.duplicate_of.is_some())
        .map(|(rel, file)| (rel, &file.duplicate_of))
        .collect();
    assert_eq!(dupes.len(), 1);
    assert_eq!(dupes[0].1.as_deref(), Some("12AM/12AM Part 1/01 song.mp3"));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_song_without_isrc_is_keyed_by_artist_title_and_length() {
    let tags = Tags {
        title: "Café".to_owned(),
        artist: "Somebody".to_owned(),
        seconds: Some(200.4),
        ..Tags::default()
    };
    let close = Tags {
        title: "CAFE".to_owned(),
        seconds: Some(201.0),
        ..tags.clone()
    };
    assert_eq!(song_key(&tags), song_key(&close));
    let tagged = Tags {
        isrc: Some("gb-abc-24-00001".to_owned()),
        ..tags.clone()
    };
    assert_eq!(song_key(&tagged), "isrc:GB-ABC-24-00001".replace('-', ""));
}

#[test]
fn the_report_counts_each_playlist() {
    let mut state = State::default();
    let tags = |title: &str| {
        Some(Tags {
            title: title.to_owned(),
            artist: "A".to_owned(),
            album: "B".to_owned(),
            ..Tags::default()
        })
    };
    let found = |via| {
        Some(Resolution::Found(Resolved {
            via,
            album: "ea8e626f-520f-4f54-9fab-8004e702afef".to_owned(),
            stand_in: None,
            recording: String::new(),
        }))
    };
    let mut add = |rel: &str, playlist: &str, file: FileState| {
        state.files.insert(
            rel.to_owned(),
            FileState {
                playlist: playlist.to_owned(),
                ..file
            },
        );
    };
    add(
        "p1/a.mp3",
        "p1",
        FileState {
            tags: tags("a"),
            resolved: found(Via::IsrcMusicbrainz),
            outcome: Some("imported".to_owned()),
            ..FileState::default()
        },
    );
    add(
        "p1/b.mp3",
        "p1",
        FileState {
            tags: tags("b"),
            resolved: Some(Resolution::NotFound),
            ..FileState::default()
        },
    );
    add(
        "p2/a.mp3",
        "p2",
        FileState {
            tags: tags("a"),
            duplicate_of: Some("p1/a.mp3".to_owned()),
            ..FileState::default()
        },
    );
    add(
        "p2/c.mp3",
        "p2",
        FileState {
            tags: tags("c"),
            in_library: true,
            ..FileState::default()
        },
    );
    add(
        "p2/d.mp3",
        "p2",
        FileState {
            tags: tags("d"),
            resolved: found(Via::NameDeezer),
            outcome: Some("unmatched:album_not_found".to_owned()),
            ..FileState::default()
        },
    );
    let report = Report::of(&state, Mode::Import, None, 0);
    assert_eq!(report.total.files, 5);
    assert_eq!(report.playlists["p1"].isrc_musicbrainz, 1);
    assert_eq!(report.playlists["p1"].unresolved, 1);
    assert_eq!(report.playlists["p2"].duplicates, 1);
    assert_eq!(report.playlists["p2"].already_in_library, 1);
    assert_eq!(report.total.imported, 1);
    assert_eq!(report.total.import_unmatched, 1);
    assert_eq!(report.unresolved.len(), 1);
    assert_eq!(report.not_imported.len(), 1);
    assert_eq!(report.albums, 1);
    assert!(report.text().contains("TOTAL"));
}
