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

#[test]
fn an_unmatched_outcome_is_tried_again_and_the_rest_are_final() {
    assert!(!settled(None));
    assert!(!settled(Some("unmatched:not_paired")));
    assert!(!settled(Some("unmatched:album_not_found")));
    assert!(settled(Some("imported")));
    assert!(settled(Some("already_owned")));
    assert!(settled(Some("held:tag_mismatch")));
    assert!(settled(Some("refused:x")));
    assert!(settled(Some("unreadable")));
}

#[test]
fn an_imported_copy_stays_the_kept_one_but_an_unmatched_copy_does_not() {
    let tags = |bitrate| {
        Some(Tags {
            title: "Song".to_owned(),
            artist: "A".to_owned(),
            isrc: Some("GBABC2400001".to_owned()),
            bitrate: Some(bitrate),
            ..Tags::default()
        })
    };
    let mut state = State::default();
    state.files.insert(
        "p1/low.mp3".to_owned(),
        FileState {
            tags: tags(128),
            outcome: Some("unmatched:not_paired".to_owned()),
            ..FileState::default()
        },
    );
    state.files.insert(
        "p2/high.mp3".to_owned(),
        FileState {
            tags: tags(320),
            ..FileState::default()
        },
    );
    mark_duplicates(&mut state);
    assert_eq!(
        state.files["p1/low.mp3"].duplicate_of.as_deref(),
        Some("p2/high.mp3")
    );
    state.files.get_mut("p1/low.mp3").unwrap().outcome = Some("imported".to_owned());
    mark_duplicates(&mut state);
    assert_eq!(
        state.files["p2/high.mp3"].duplicate_of.as_deref(),
        Some("p1/low.mp3")
    );
}

#[test]
fn the_guide_asks_the_editions_carrying_the_most_recordings_first() {
    let editions = |ids: &[&str]| ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();
    assert!(pick_editions(&[]).is_empty());
    assert!(pick_editions(&[Vec::new()]).is_empty());
    assert_eq!(
        pick_editions(&[editions(&["r1", "r2"]), editions(&["r2", "r3"])]),
        editions(&["r2", "r1", "r3"])
    );
    // A tie: the first listed first.
    assert_eq!(
        pick_editions(&[editions(&["r9", "r1"])]),
        editions(&["r9", "r1"])
    );
    // An edition listed twice for one recording counts once.
    assert_eq!(
        pick_editions(&[
            editions(&["r1", "r1"]),
            editions(&["r2"]),
            editions(&["r2"])
        ]),
        editions(&["r2", "r1"])
    );
}

#[test]
fn a_playlist_csv_match_is_recorded_and_reported_as_playlist_csv() {
    use crate::playlist_import::spotify_match::{Cached, Catalogue, Item, find, map_folders};
    // The state keeps the new via under its own name.
    assert_eq!(
        serde_json::to_string(&Via::PlaylistCsv).unwrap(),
        "\"playlist_csv\""
    );
    // A CSV playlist named like its folder (Exportify's underscores) maps
    // and matches a leftover in its own folder first.
    let text = "\u{feff}Track URI,ISRC,Track Name,Album Name,Artist Name(s),Release Date,Duration (ms),Popularity,Explicit,Added By,Added At,Genres,Record Label\n\
spotify:track:t1,USUM70905526,\"Strobe - Radio Edit\",\"For Lack Of A Better Name\",\"deadmau5\",2009,214000,61,false,me,2020-01-01T00:00:00Z,\"\",\"\"\n";
    let items: Vec<Item> = csv_source::items(text).unwrap();
    let mut playlists = std::collections::BTreeMap::new();
    playlists.insert(
        "csv:6PM_Sunset".to_owned(),
        Cached {
            name: "6PM_Sunset".to_owned(),
            items: Some(items),
            ..Cached::default()
        },
    );
    let folders: std::collections::BTreeSet<String> =
        ["6PM Sunset".to_owned(), "D&B".to_owned()].into();
    let unmapped = map_folders(&mut playlists, &folders);
    assert_eq!(unmapped, vec!["D&B".to_owned()]);
    let catalogue = Catalogue {
        source: "csv:/data/tom_music_import_playlists".to_owned(),
        playlists,
        ..Catalogue::default()
    };
    assert!(csv_source::is_csv(&catalogue));
    let tags = Tags {
        title: "Strobe (Radio Edit)".to_owned(),
        artist: "Deadmau5".to_owned(),
        seconds: Some(212.0),
        ..Tags::default()
    };
    let (id, item, own) = find(&catalogue, "6PM Sunset", &tags).unwrap();
    assert_eq!(
        (id.as_str(), item.id.as_str(), own),
        ("csv:6PM_Sunset", "t1", true)
    );
    // The report: its own column and notes.
    let mut state = State {
        spotify: Some(catalogue),
        ..State::default()
    };
    state.files.insert(
        "6PM Sunset/x.mp3".to_owned(),
        FileState {
            playlist: "6PM Sunset".to_owned(),
            tags: Some(tags),
            resolved: Some(Resolution::Found(Resolved {
                via: Via::PlaylistCsv,
                album: "ea8e626f-520f-4f54-9fab-8004e702afef".to_owned(),
                stand_in: None,
                recording: String::new(),
            })),
            ..FileState::default()
        },
    );
    state.files.insert(
        "D&B/y.mp3".to_owned(),
        FileState {
            playlist: "D&B".to_owned(),
            tags: Some(Tags::default()),
            resolved: Some(Resolution::NotFound),
            ..FileState::default()
        },
    );
    let report = Report::of(&state, Mode::DryRun, None, 0);
    assert_eq!(report.total.playlist_csv, 1);
    assert_eq!(report.total.spotify_playlist, 0);
    let summary = report.spotify.as_ref().unwrap();
    assert_eq!(summary.source, "playlist_csv");
    assert_eq!(summary.folders["6PM Sunset"].matched, 1);
    let text = report.text();
    assert!(text.contains("pl-csv"));
    assert!(text.contains("Playlist CSV match"));
    assert!(text.contains("6PM_Sunset.csv"));
    assert!(text.contains("Folders with no CSV file: D&B"));
    assert!(report.headline().contains("1 resolved"));
}

/// Fork: files imported by their tags count in the `tags` column; an
/// unresolved one stays listed with where it went, and one a later run
/// resolved is left as it is and noted.
#[test]
fn files_imported_by_tags_are_counted_and_a_later_resolution_is_noted() {
    let mut state = State::default();
    let tags = |title: &str| {
        Some(Tags {
            title: title.to_owned(),
            artist: "A".to_owned(),
            album: "B".to_owned(),
            ..Tags::default()
        })
    };
    let found = Some(Resolution::Found(Resolved {
        via: Via::PlaylistCsv,
        album: "ea8e626f-520f-4f54-9fab-8004e702afef".to_owned(),
        stand_in: None,
        recording: String::new(),
    }));
    let tag_only = |title: &str, resolved: Option<Resolution>, why: &str| FileState {
        playlist: "p1".to_owned(),
        tags: tags(title),
        resolved,
        outcome: Some(tags_fallback::OUTCOME.to_owned()),
        via: Some(tags_fallback::VIA.to_owned()),
        library_path: Some(format!("A/B/{title}.mp3")),
        tags_reason: Some(why.to_owned()),
        ..FileState::default()
    };
    state.files.insert(
        "p1/a.mp3".into(),
        tag_only("a", Some(Resolution::NotFound), "not_found"),
    );
    state.files.insert(
        "p1/b.mp3".into(),
        tag_only("b", found.clone(), "unmatched:not_paired"),
    );
    state
        .files
        .insert("p1/c.mp3".into(), tag_only("c", found, "not_found"));
    let report = Report::of(&state, Mode::Import, None, 0);
    assert_eq!(report.playlists["p1"].tags, 3);
    assert_eq!(report.total.tags, 3);
    assert_eq!(report.total.tags_resolved_later, 1);
    assert_eq!(report.unresolved.len(), 1);
    assert!(report.unresolved[0].why.contains("tags_only"));
    assert!(report.not_imported.is_empty());
    assert_eq!(report.resolved_after_tags.len(), 1);
    assert_eq!(report.resolved_after_tags[0].path, "p1/c.mp3");
    let text = report.text();
    assert!(text.contains(" tags\n"));
    assert!(text.contains("3 imported by tags"));
    assert!(text.contains("resolved since"));
    // The import never takes a tag-only file again; the playlist match
    // still looks at an unresolved one.
    assert!(settled(Some(tags_fallback::OUTCOME)));
    assert!(!tags_fallback::takes(&state.files["p1/a.mp3"]));
    assert!(spotify_match::is_leftover(&state.files["p1/a.mp3"]));
    assert!(!spotify_match::is_leftover(&state.files["p1/c.mp3"]));
}
