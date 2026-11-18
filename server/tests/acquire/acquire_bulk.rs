//! Fork: bulk playlist requests: the estimate, album grouping, pacing and
//! the pilot.

use std::sync::Arc;

use droppedneedle_acquire::bulk_requests::{BulkRequests, Estimate, RunState, SongState};
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::target::lookup::{AlbumRelease, AlbumTrack, ReleaseCandidate};
use serde_json::Value;

use crate::common::Probe;
use crate::common::acquire::FixedAlbums;
use crate::common::bundles::{quiet_ticks, requests_over};

const GROUP: &str = "6d0b7a1e-0000-4000-8000-0000000000a1";
const RELEASE: &str = "6d0b7a1e-0000-4000-8000-0000000000a2";
const SONG_A: &str = "6d0b7a1e-0000-4000-8000-0000000000b1";
const SONG_B: &str = "6d0b7a1e-0000-4000-8000-0000000000b2";
const LONE: &str = "6d0b7a1e-0000-4000-8000-0000000000b3";
const LONE_GROUP: &str = "6d0b7a1e-0000-4000-8000-0000000000a3";

fn track(position: u32, recording: &str) -> AlbumTrack {
    AlbumTrack {
        disc: 1,
        position,
        title: format!("Track {position}"),
        duration_seconds: Some(200.0),
        recording_mbid: recording.to_owned(),
        release_track_mbid: format!("6d0b7a1e-0000-4000-8000-0000000001{position:02}"),
    }
}

/// MusicBrainz knows one edition of the album, holding both songs at
/// positions 2 and 5 among others.
fn lookup() -> Arc<FixedAlbums> {
    let candidate = ReleaseCandidate {
        id: RELEASE.to_owned(),
        status: Some("Official".to_owned()),
        date: Some("1994".to_owned()),
        release_group_mbid: GROUP.to_owned(),
        primary_type: Some("Album".to_owned()),
        secondary_types: Vec::new(),
    };
    let mut albums = FixedAlbums::default();
    albums
        .recordings
        .insert(SONG_A.to_owned(), vec![candidate.clone()]);
    albums.recordings.insert(SONG_B.to_owned(), vec![candidate]);
    albums.releases.insert(
        RELEASE.to_owned(),
        AlbumRelease {
            id: RELEASE.to_owned(),
            title: "Dummy".to_owned(),
            release_group_mbid: GROUP.to_owned(),
            artist: "Portishead".to_owned(),
            various_artists: false,
            year: Some(1994),
            tracks: vec![
                track(1, "6d0b7a1e-0000-4000-8000-0000000000c1"),
                track(2, SONG_A),
                track(3, "6d0b7a1e-0000-4000-8000-0000000000c3"),
                track(4, "6d0b7a1e-0000-4000-8000-0000000000c4"),
                track(5, SONG_B),
            ],
        },
    );
    Arc::new(albums)
}

/// A playlist of user `u-ada`: `(entry, title, album id, recording,
/// library file)` in order.
async fn seed_playlist(
    db: &AcquireDb,
    playlist: &'static str,
    songs: Vec<(
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
    )>,
) {
    db.write("test.playlist", move |tx| {
        tx.execute(
            "INSERT INTO catalog_playlists (id, name, created_at, updated_at, user_id) \
             VALUES (?1, 'Liked', 1, 1, 'u-ada')",
            [playlist],
        )?;
        for (position, (entry, title, album, recording, file)) in songs.into_iter().enumerate() {
            tx.execute(
                "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, track_name, \
                 artist_name, album_name, album_id, source_type, duration_ms, created_at, \
                 library_file_id, recording_mbid) VALUES (?1, ?2, ?3, ?4, 'Portishead', \
                 'Dummy', ?5, 'spotify', 200000, 1, ?6, ?7)",
                rusqlite::params![
                    entry,
                    playlist,
                    position as i64,
                    title,
                    album,
                    file,
                    recording
                ],
            )?;
        }
        Ok(())
    })
    .await
    .expect("playlist");
}

async fn rig() -> (AcquireDb, BulkRequests) {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "admin").await.expect("user");
    let requests = requests_over(&db).await;
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    let bulk = BulkRequests::new(db.clone(), requests, journal, lookup());
    (db, bulk)
}

/// Each download task: type, release group, recording and plan.
fn tasks(db: &AcquireDb) -> Vec<(String, String, Option<String>, Value)> {
    let rows: Vec<(String, String, Option<String>, String)> = db.db().rows(
        "SELECT download_type, release_group_mbid, recording_mbid, plan_json \
         FROM download_tasks ORDER BY download_type, created_at",
        [],
    );
    rows.into_iter()
        .map(|(kind, group, recording, plan)| {
            (
                kind,
                group,
                recording,
                serde_json::from_str(&plan).expect("plan"),
            )
        })
        .collect()
}

/// Two songs of one album go as one album download for just their
/// tracks, a lone song on its own, and the run keeps to its rate: the next
/// pass within the hour sends nothing.
#[tokio::test]
async fn two_songs_of_an_album_go_as_one_album_download_at_the_rate() {
    let (db, bulk) = rig().await;
    seed_playlist(
        &db,
        "pl-1",
        vec![
            ("e1", "Track 2", Some(GROUP), Some(SONG_A), None),
            ("e2", "Track 5", Some(GROUP), Some(SONG_B), None),
            ("e3", "Lone", Some(LONE_GROUP), Some(LONE), None),
            ("e4", "By Name", None, None, None),
            ("e5", "Owned", Some(GROUP), None, Some("file-1")),
        ],
    )
    .await;

    assert_eq!(
        bulk.estimate("pl-1").await.unwrap(),
        Estimate {
            missing: 4,
            album_groups: 1,
            grouped_songs: 2,
            by_recording: 1,
            by_name: 1,
        }
    );
    let started = bulk.start("pl-1", 3, 0).await.unwrap();
    assert_eq!(started.queued, 4);
    assert!(
        bulk.start("pl-1", 3, 0).await.is_err(),
        "one run per playlist at a time"
    );
    let run = started.run.id.clone();

    assert_eq!(
        bulk.tick().await,
        3,
        "the album's two songs and the lone song"
    );
    let tasks = tasks(&db);
    assert_eq!(tasks.len(), 2, "{tasks:?}");
    let (kind, group, _, plan) = &tasks[0];
    assert_eq!((kind.as_str(), group.as_str()), ("album", GROUP));
    assert_eq!(plan["release_mbid"], RELEASE);
    assert_eq!(
        plan["album_wanted"],
        serde_json::json!([{"disc": 1, "track": 2}, {"disc": 1, "track": 5}]),
        "only the wanted tracks"
    );
    assert_eq!(plan["bulk_request"], true, "no search review");
    let (kind, _, recording, plan) = &tasks[1];
    assert_eq!(kind, "track");
    assert_eq!(recording.as_deref(), Some(LONE));
    assert_eq!(plan["playlist_song"], true);

    assert_eq!(bulk.tick().await, 0, "three songs this hour is the rate");
    let status = bulk.status(&run).await.unwrap();
    assert_eq!((status.queued, status.in_progress), (1, 3));
    let grouped: Vec<bool> = status.songs.iter().map(|song| song.album_group).collect();
    assert_eq!(grouped, vec![true, true, false, false]);
    assert_eq!(status.songs[3].state, SongState::Queued);

    // The album sources had no copy: the album's songs go again on their
    // own (where plugins can serve them), once the rate allows.
    db.write("test.album_failed", |tx| {
        tx.execute(
            "UPDATE download_tasks SET status = 'failed', outcome_code = 'no_source' \
             WHERE download_type = 'album'",
            [],
        )?;
        Ok(())
    })
    .await
    .expect("album fails");
    assert_eq!(bulk.tick().await, 0, "still within the hour's rate");
    let status = bulk.status(&run).await.unwrap();
    assert_eq!(
        status.queued, 3,
        "the album's two songs and the by-name song"
    );
    assert!(
        status.songs[..2]
            .iter()
            .all(|song| !song.album_group && song.state == SongState::Queued),
        "{:?}",
        status.songs
    );
}

/// A pilot stops the run after its songs until a person resumes it; the
/// rest then goes.
#[tokio::test]
async fn a_pilot_waits_for_a_person_then_the_rest_goes() {
    let (db, bulk) = rig().await;
    seed_playlist(
        &db,
        "pl-2",
        vec![
            ("p1", "One", None, None, None),
            ("p2", "Two", None, None, None),
            ("p3", "Three", None, None, None),
        ],
    )
    .await;
    let run = bulk.start("pl-2", 60, 1).await.unwrap().run.id;
    assert_eq!(bulk.tick().await, 1, "the pilot");
    assert_eq!(bulk.tick().await, 0);
    let status = bulk.status(&run).await.unwrap();
    assert_eq!(status.run.state, RunState::PilotDone);
    assert_eq!(status.queued, 2);

    bulk.set_state(&run, RunState::Running).await.unwrap();
    assert_eq!(bulk.tick().await, 2, "the rest");
    assert_eq!(bulk.tick().await, 0);
    let status = bulk.status(&run).await.unwrap();
    assert_eq!(status.run.state, RunState::Done);
    assert_eq!(status.queued, 0);
}
