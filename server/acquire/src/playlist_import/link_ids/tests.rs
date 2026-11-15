//! The "link spotify ids" step: pairing rules, the one-row-two-songs guard,
//! idempotent writes, and a playlist entry linking to the file through the
//! catalogue's own linker.

use std::path::PathBuf;
use std::sync::Arc;

use super::super::spotify_match::{Cached, PlaylistMatch};
use super::super::{FileState, Tags};
use super::*;
use crate::db::AcquireDb;
use droppedneedle_kernel::db::{DbError, Lane};

const STROBE: &str = "4uLU6hMCjMI75M1A2tKUQC";
const OTHER: &str = "3Ty7OTBNSigGEpeW2PqcsC";
const THIRD: &str = "0abcdefghijklmnopqrstu";

fn tags(artist: &str, title: &str, seconds: f64, isrc: Option<&str>) -> Tags {
    Tags {
        title: title.to_owned(),
        artist: artist.to_owned(),
        album: "Album".to_owned(),
        isrc: isrc.map(str::to_owned),
        seconds: Some(seconds),
        ..Tags::default()
    }
}

fn file(playlist: &str, tags: Tags, outcome: &str) -> FileState {
    FileState {
        playlist: playlist.to_owned(),
        tags: Some(tags),
        outcome: Some(outcome.to_owned()),
        ..FileState::default()
    }
}

fn row(id: &str, artists: &[&str], title: &str, seconds: f64, isrc: Option<&str>) -> Item {
    Item {
        id: id.to_owned(),
        title: title.to_owned(),
        artists: artists.iter().map(|name| (*name).to_owned()).collect(),
        album: "Album".to_owned(),
        album_id: None,
        seconds: Some(seconds),
        isrc: isrc.map(str::to_owned),
    }
}

fn catalogue(playlists: &[(&str, Option<&str>, Vec<Item>)]) -> Catalogue {
    Catalogue {
        source: "csv:/import".to_owned(),
        playlists: playlists
            .iter()
            .map(|(name, folder, items)| {
                (
                    format!("csv:{name}"),
                    Cached {
                        name: (*name).to_owned(),
                        folder: folder.map(str::to_owned),
                        items: Some(items.clone()),
                        ..Cached::default()
                    },
                )
            })
            .collect(),
        ..Catalogue::default()
    }
}

fn state(files: Vec<(&str, FileState)>) -> State {
    State {
        files: files
            .into_iter()
            .map(|(rel, file)| (rel.to_owned(), file))
            .collect(),
        spotify: None,
    }
}

fn rows_of(pairing: &Pairing, rel: &str) -> Vec<(String, usize, PairedBy)> {
    pairing
        .rows_of(rel)
        .iter()
        .map(|((id, at), by)| (id.clone(), *at, *by))
        .collect()
}

#[test]
fn files_pair_by_isrc_then_recorded_match_then_name_then_base_title() {
    let rows = vec![
        row(
            STROBE,
            &["deadmau5"],
            "Strobe - Radio Edit",
            214.0,
            Some("USUM70905526"),
        ),
        row(
            OTHER,
            &["Portishead"],
            "Glory Box",
            300.0,
            Some("GBAAA9400001"),
        ),
        row(
            THIRD,
            &["Lucius", "Harvey Sutherland"],
            "Next to Normal - Harvey Sutherland Remix",
            257.0,
            Some("USQE92200114"),
        ),
        row(
            "1abcdefghijklmnopqrstu",
            &["Someone"],
            "Recorded",
            200.0,
            None,
        ),
    ];
    let catalogue = catalogue(&[("12AM", Some("12AM"), rows)]);
    let mut recorded = file(
        "12AM",
        tags("Someone Else", "Totally Different", 999.0, None),
        "imported",
    );
    recorded.playlist_match = Some(PlaylistMatch {
        playlist_id: "csv:12AM".to_owned(),
        track_id: "1abcdefghijklmnopqrstu".to_owned(),
        same_folder: true,
        looked_up: true,
    });
    let state = state(vec![
        // Another folder, but the ISRC is the row's.
        (
            "Psy/a.mp3",
            file(
                "Psy",
                tags("x", "y", 1.0, Some("US-UM7-09-05526")),
                "imported",
            ),
        ),
        (
            "12AM/b.mp3",
            file(
                "12AM",
                tags("Portishead", "Glory Box (Album Version)", 301.5, None),
                "imported",
            ),
        ),
        (
            "12AM/c.mp3",
            file(
                "12AM",
                tags("Lucius", "Next to Normal", 256.9, None),
                "imported",
            ),
        ),
        ("12AM/d.mp3", recorded),
        // The name agrees, but the folder is another: no pair.
        (
            "D&B/e.mp3",
            file(
                "D&B",
                tags("Portishead", "Glory Box", 300.0, None),
                "imported",
            ),
        ),
        // Same folder and name, length too far off.
        (
            "12AM/f.mp3",
            file(
                "12AM",
                tags("Lucius", "Next to Normal (Long)", 400.0, None),
                "imported",
            ),
        ),
    ]);
    let pairing = pair(&state, &catalogue);
    assert_eq!(
        rows_of(&pairing, "Psy/a.mp3"),
        vec![("csv:12AM".to_owned(), 0, PairedBy::Isrc)]
    );
    assert_eq!(
        rows_of(&pairing, "12AM/b.mp3"),
        vec![("csv:12AM".to_owned(), 1, PairedBy::Name)]
    );
    assert_eq!(
        rows_of(&pairing, "12AM/c.mp3"),
        vec![("csv:12AM".to_owned(), 2, PairedBy::BaseName)]
    );
    assert_eq!(
        rows_of(&pairing, "12AM/d.mp3"),
        vec![("csv:12AM".to_owned(), 3, PairedBy::Recorded)]
    );
    assert!(rows_of(&pairing, "D&B/e.mp3").is_empty());
    assert!(rows_of(&pairing, "12AM/f.mp3").is_empty());
    assert_eq!(
        base_title("Wave - R.A.F. Edit Mix"),
        base_title("Wave (R.A.F. Edit Mix)")
    );
}

#[test]
fn a_row_never_pairs_to_two_different_songs() {
    let rows = vec![
        row(STROBE, &["A"], "Song", 200.0, None),
        row(OTHER, &["B"], "Tune", 300.0, Some("GBAAA9400001")),
    ];
    let catalogue = catalogue(&[("12AM", Some("12AM"), rows)]);
    let state = state(vec![
        // Two different songs (their own ISRCs differ) both agree on row 0
        // by name: it goes to neither.
        (
            "12AM/one.mp3",
            file(
                "12AM",
                tags("A", "Song", 200.0, Some("USAAA0000001")),
                "imported",
            ),
        ),
        (
            "12AM/two.mp3",
            file(
                "12AM",
                tags("A", "Song", 201.0, Some("USAAA0000002")),
                "imported",
            ),
        ),
        // Row 1 is taken by its ISRC; another song agreeing by name is
        // not given it.
        (
            "12AM/three.mp3",
            file(
                "12AM",
                tags("B", "Tune", 300.0, Some("GBAAA9400001")),
                "imported",
            ),
        ),
        (
            "12AM/four.mp3",
            file(
                "12AM",
                tags("B", "Tune", 300.0, Some("USZZZ0000009")),
                "imported",
            ),
        ),
        // A copy of song three in another folder shares its row.
        (
            "D&B/copy.mp3",
            file(
                "D&B",
                tags("B", "Tune", 300.0, Some("GBAAA9400001")),
                "imported",
            ),
        ),
    ]);
    let pairing = pair(&state, &catalogue);
    assert!(rows_of(&pairing, "12AM/one.mp3").is_empty());
    assert!(rows_of(&pairing, "12AM/two.mp3").is_empty());
    assert!(pairing.contested.contains(&("csv:12AM".to_owned(), 0)));
    assert_eq!(rows_of(&pairing, "12AM/three.mp3").len(), 1);
    assert!(rows_of(&pairing, "12AM/four.mp3").is_empty());
    assert_eq!(
        rows_of(&pairing, "D&B/copy.mp3"),
        rows_of(&pairing, "12AM/three.mp3")
    );
}

#[test]
fn links_take_the_rows_ids_and_fall_back_to_the_tag_isrc() {
    let catalogue = catalogue(&[(
        "12AM",
        Some("12AM"),
        vec![row(
            STROBE,
            &["deadmau5"],
            "Strobe",
            214.0,
            Some("USUM70905526"),
        )],
    )]);
    let paired = file(
        "12AM",
        tags("deadmau5", "Strobe", 214.0, Some("XXAAA0000001")),
        "imported",
    );
    let links = links_of(
        &paired,
        &[(("csv:12AM".to_owned(), 0), PairedBy::Name)],
        &catalogue,
    );
    assert_eq!(links.spotify, vec![STROBE.to_owned()]);
    assert_eq!(
        links.isrc,
        vec!["USUM70905526".to_owned()],
        "the row's ISRC wins"
    );
    let source = links.source_links();
    assert_eq!(source[0].provider, "spotify");
    assert_eq!(source[0].entity_type, "track");
    assert_eq!(
        source[0].canonical_url,
        format!("https://open.spotify.com/track/{STROBE}")
    );
    assert_eq!(source[1].provider, "isrc");
    assert_eq!(source[1].entity_type, "recording");
    let alone = file(
        "Psy",
        tags("x", "y", 1.0, Some("xx-aaa-00-00001")),
        "tags_only",
    );
    let links = links_of(&alone, &[], &catalogue);
    assert!(links.spotify.is_empty());
    assert_eq!(links.isrc, vec!["XXAAA0000001".to_owned()]);
    // A local-file row (no Spotify id) gives no Spotify link.
    let local = catalogue_with_local();
    let links = links_of(
        &paired,
        &[(("csv:L".to_owned(), 0), PairedBy::Name)],
        &local,
    );
    assert!(links.spotify.is_empty());
}

fn catalogue_with_local() -> Catalogue {
    catalogue(&[("L", Some("L"), vec![row("row1", &["a"], "b", 1.0, None)])])
}

const ARTIST: &str = "00000000-0000-4000-8000-000000000002";

async fn seed_track(db: &AcquireDb, id: &str, source: Option<&str>, relative: &str) {
    let (id, source, relative) = (
        id.to_owned(),
        source.map(str::to_owned),
        relative.to_owned(),
    );
    db.db()
        .write(Lane::Background, "test.seed_track", move |tx| {
            tx.execute(
                "INSERT OR IGNORE INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                 album_artist_id, grouping_source, created_at, updated_at) \
                 VALUES ('al-1', 'r', 'al-1', 'Album', 'album', ?1, 'automatic', 1, 1)",
                [ARTIST],
            )?;
            tx.execute(
                "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
                 path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
                 album_title, album_title_folded, file_format, ingest_source, source_path, \
                 imported_at, membership_source) \
                 VALUES (?1, 'al-1', 'r', '/music/' || ?3, ?3, ?1, 1, 1, 's', 'Strobe', 'strobe', \
                 'Album', 'album', 'mp3', 'drop_folder', ?2, 1, 'automatic')",
                rusqlite::params![id, source, relative],
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("seed track");
}

async fn links_of_track(db: &AcquireDb, track: &str) -> Vec<String> {
    let track = track.to_owned();
    db.db()
        .read("test.links", move |conn| {
            let mut statement = conn.prepare(
                "SELECT provider || ':' || external_entity_type || ':' || external_id || ':' \
                 || decision_source FROM local_entity_source_links WHERE local_track_id = ?1 \
                 ORDER BY 1",
            )?;
            let rows = statement
                .query_map([track], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok::<_, DbError>(rows)
        })
        .await
        .expect("links")
}

/// One pass over `state`: tracks found, plan made, links written.
async fn pass(db: &AcquireDb, state: &State, catalogue: &Catalogue, source: &Path) -> LinkReport {
    let asks = asks(state, source);
    let asked = asks.clone();
    let tracks = db
        .db()
        .read("test.tracks", move |conn| {
            Ok::<_, DbError>(tracks(conn, &asked)?)
        })
        .await
        .expect("tracks");
    let plan = plan(state, catalogue, &asks, &tracks);
    let writes = plan.writes;
    db.db()
        .write(Lane::Background, "test.write", move |tx| {
            Ok::<_, DbError>(write(tx, &writes, 1_700_000_000.0)?)
        })
        .await
        .expect("write");
    plan.report
}

#[tokio::test]
async fn links_are_written_once_and_a_tag_only_copy_waits_for_the_scan() {
    let db = AcquireDb::scratch().expect("db");
    let source = PathBuf::from("/data/import");
    seed_track(
        &db,
        "t-placed",
        Some("/data/import/12AM/a.mp3"),
        "deadmau5/Strobe.mp3",
    )
    .await;
    let catalogue = catalogue(&[(
        "12AM",
        Some("12AM"),
        vec![row(
            STROBE,
            &["deadmau5"],
            "Strobe",
            214.0,
            Some("USUM70905526"),
        )],
    )]);
    let mut tag_only = file(
        "Psy",
        tags("x", "y", 100.0, Some("XXAAA0000001")),
        "tags_only",
    );
    tag_only.library_path = Some("x/Album/y.mp3".to_owned());
    let mut duplicate = file("D&B", tags("deadmau5", "Strobe", 214.0, None), "imported");
    duplicate.duplicate_of = Some("12AM/a.mp3".to_owned());
    duplicate.outcome = None;
    let state = state(vec![
        (
            "12AM/a.mp3",
            file("12AM", tags("deadmau5", "Strobe", 214.0, None), "imported"),
        ),
        ("Psy/b.mp3", tag_only),
        ("D&B/a copy.mp3", duplicate),
        (
            "12AM/left.mp3",
            file(
                "12AM",
                tags("z", "z", 1.0, None),
                "unmatched:no_album_match",
            ),
        ),
    ]);
    let first = pass(&db, &state, &catalogue, &source).await;
    assert_eq!(first.by_spotify, 2, "the file and its duplicate");
    assert_eq!(first.no_link, 1);
    assert_eq!(first.waiting, 1, "the tag-only copy is not indexed yet");
    assert_eq!(first.not_in_library, 1);
    assert_eq!(first.rows_paired, 1);
    let want = vec![
        "isrc:recording:USUM70905526:automatic".to_owned(),
        format!("spotify:track:{STROBE}:automatic"),
    ];
    assert_eq!(links_of_track(&db, "t-placed").await, want);

    // Again: nothing new is written.
    let again = pass(&db, &state, &catalogue, &source).await;
    assert_eq!(again, first);
    assert_eq!(links_of_track(&db, "t-placed").await, want);

    // The scan indexes the tag-only copy: the next pass links it by ISRC.
    seed_track(&db, "t-tags", None, "x/Album/y.mp3").await;
    let later = pass(&db, &state, &catalogue, &source).await;
    assert_eq!(later.isrc_only, 1);
    assert_eq!(later.waiting, 0);
    assert_eq!(
        links_of_track(&db, "t-tags").await,
        vec!["isrc:recording:XXAAA0000001:automatic".to_owned()]
    );
}

/// The journey: after the step, a Spotify playlist entry with the file's
/// Spotify id links to the file through the catalogue's own linker.
#[tokio::test]
async fn a_synced_playlist_entry_links_to_the_linked_file() {
    let db = AcquireDb::scratch().expect("db");
    db.add_user("u-ada", "Ada", "admin").await.expect("user");
    let source = PathBuf::from("/data/import");
    seed_track(
        &db,
        "t-placed",
        Some("/data/import/12AM/a.mp3"),
        "deadmau5/Strobe.mp3",
    )
    .await;
    let catalogue = catalogue(&[(
        "12AM",
        Some("12AM"),
        vec![row(
            STROBE,
            &["deadmau5"],
            "Strobe - Radio Edit",
            214.0,
            None,
        )],
    )]);
    let state = state(vec![(
        "12AM/a.mp3",
        file(
            "12AM",
            tags("deadmau5", "Strobe (Radio Edit)", 213.5, None),
            "imported",
        ),
    )]);
    let report = pass(&db, &state, &catalogue, &source).await;
    assert_eq!(report.by_spotify, 1);
    db.db()
        .write(Lane::Background, "test.playlist", |tx| {
            tx.execute(
                "INSERT INTO catalog_playlists (id, name, created_at, updated_at, source_ref, \
                 user_id) VALUES ('pl-1', '12AM', 1, 1, 'spotify:playlist:abc', 'u-ada')",
                [],
            )?;
            // Named differently from the tags: only the id can link it.
            tx.execute(
                "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, track_name, \
                 artist_name, album_name, source_type, duration_ms, created_at) \
                 VALUES ('e-1', 'pl-1', 0, 'Strobe - Radio Edit', 'deadmau5', 'For Lack', '', \
                 214000, 1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO catalog_playlist_entry_identities (entry_id, spotify_track_id) \
                 VALUES ('e-1', ?1)",
                [STROBE],
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("playlist");
    let playlists = droppedneedle_catalog::collections::store::PlaylistStore::new(
        droppedneedle_catalog::collections::db::CollectionsDb::new(
            db.db().clone(),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
    );
    playlists.link_library().await.expect("linked");
    let linked: Option<String> = db
        .db()
        .read("test.entry", |conn| {
            Ok::<_, DbError>(conn.query_row(
                "SELECT library_file_id FROM catalog_playlist_tracks WHERE id = 'e-1'",
                [],
                |row| row.get(0),
            )?)
        })
        .await
        .expect("entry");
    assert_eq!(linked.as_deref(), Some("t-placed"));
}
