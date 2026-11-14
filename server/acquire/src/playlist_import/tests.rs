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

fn forbidden() -> resolve::LookupFailure {
    crate::imports::spotify_resolve::ResolveError::Provider(
        droppedneedle_providers::ProviderError::Forbidden { provider: "deezer" },
    )
    .into()
}

#[test]
fn a_refused_request_is_per_song_and_an_outage_stops() {
    use crate::imports::spotify_resolve::ResolveError;
    use droppedneedle_providers::ProviderError;
    assert_eq!(
        forbidden(),
        resolve::LookupFailure::Refused("deezer refused the request (403)".to_owned())
    );
    let limited: resolve::LookupFailure = ResolveError::Provider(ProviderError::RateLimited {
        provider: "deezer",
        retry_after: Some(Duration::from_secs(7)),
    })
    .into();
    assert!(matches!(
        limited,
        resolve::LookupFailure::Limited { wait: Some(w), .. } if w == Duration::from_secs(7)
    ));
    let down: resolve::LookupFailure = ResolveError::Provider(ProviderError::Server {
        provider: "musicbrainz",
        status: 502,
    })
    .into();
    assert!(matches!(down, resolve::LookupFailure::Down(_)));
    assert!(matches!(
        resolve::LookupFailure::from("check failed".to_owned()),
        resolve::LookupFailure::Down(_)
    ));
}

#[test]
fn one_refusal_sets_the_song_aside_and_twenty_in_a_row_stop() {
    let mut guard = Guard::default();
    for _ in 0..REFUSED_IN_A_ROW - 1 {
        assert!(matches!(guard.failed(forbidden()), Next::SetAside(_)));
    }
    guard.answered();
    for _ in 0..REFUSED_IN_A_ROW - 1 {
        assert!(matches!(guard.failed(forbidden()), Next::SetAside(_)));
    }
    match guard.failed(forbidden()) {
        Next::Stop(line) => assert!(line.contains("20 lookups in a row were refused")),
        other => panic!("{other:?}"),
    }
    let mut guard = Guard::default();
    assert!(matches!(
        guard.failed(resolve::LookupFailure::Down("x".to_owned())),
        Next::Stop(line) if line.starts_with("a lookup failed")
    ));
}

#[test]
fn a_rate_limit_waits_longer_each_time_then_stops() {
    let limited = |wait| resolve::LookupFailure::Limited {
        message: "deezer is rate limited (429)".to_owned(),
        wait,
    };
    let mut guard = Guard::default();
    assert_eq!(guard.failed(limited(None)), Next::Wait(LIMITED_WAIT_BASE));
    assert_eq!(
        guard.failed(limited(None)),
        Next::Wait(LIMITED_WAIT_BASE * 2)
    );
    assert_eq!(
        guard.failed(limited(Some(Duration::from_secs(900)))),
        Next::Wait(Duration::from_secs(900))
    );
    for _ in 3..LIMITED_WAITS {
        assert!(matches!(guard.failed(limited(None)), Next::Wait(w) if w <= LIMITED_WAIT_MAX));
    }
    assert!(matches!(guard.failed(limited(None)), Next::Stop(_)));
    guard.answered();
    assert_eq!(guard.failed(limited(None)), Next::Wait(LIMITED_WAIT_BASE));
}

#[test]
fn a_refused_song_is_tried_once_more_on_a_re_run() {
    assert!(wants_lookup(None));
    assert!(!wants_lookup(Some(&Resolution::NotFound)));
    let first = refused(None, "deezer refused the request (403)".to_owned());
    assert_eq!(
        first,
        Resolution::Refused {
            reason: "deezer refused the request (403)".to_owned(),
            tries: 1
        }
    );
    assert!(wants_lookup(Some(&first)));
    let second = refused(Some(&first), "again".to_owned());
    assert!(matches!(second, Resolution::Refused { tries: 2, .. }));
    assert!(!wants_lookup(Some(&second)));
    // The state file keeps it.
    let json = serde_json::to_string(&second).unwrap();
    assert_eq!(serde_json::from_str::<Resolution>(&json).unwrap(), second);
}

#[test]
fn the_report_lists_a_refused_lookup_with_its_reason() {
    let mut state = State::default();
    state.files.insert(
        "p1/x.mp3".to_owned(),
        FileState {
            playlist: "p1".to_owned(),
            tags: Some(Tags {
                title: "No Sleep (001)".to_owned(),
                artist: "Van Damn".to_owned(),
                album: "No Sleep (001)".to_owned(),
                ..Tags::default()
            }),
            resolved: Some(refused(None, "deezer refused the request (403)".to_owned())),
            ..FileState::default()
        },
    );
    let report = Report::of(&state, Mode::DryRun, None, 0);
    assert_eq!(report.total.unresolved, 1);
    assert_eq!(report.total.pending, 0);
    assert_eq!(
        report.unresolved[0].why,
        "lookup refused: deezer refused the request (403)"
    );
}
