//! Fork: Deezer stand-ins linked to the MusicBrainz album their files
//! landed under, read from a real schema.

use droppedneedle_kernel::db::fold::fold_text;
use droppedneedle_library::identity::reads::group_owned;
use droppedneedle_library::provider_match::DEEZER;
use droppedneedle_library::sources::links::{
    LinkVia, Repair, link, linked, linked_open, releases, repairs,
};
use droppedneedle_providers::virtual_ids::{self, Kind};
use rusqlite::{Connection, params};

const MB: &str = "ea8e626f-520f-4f54-9fab-8004e702afef";
const OTHER: &str = "1dbbabcb-6902-40ec-8a7f-bcb1aa15d6bd";

fn scratch() -> (droppedneedle_kernel::test_support::ScratchDir, Connection) {
    let dir = droppedneedle_kernel::test_support::ScratchDir::new("source-links").unwrap();
    let path = dir.path().join("app.db");
    droppedneedle_kernel::test_support::template::copy_to(&path).unwrap();
    let conn = Connection::open(&path).unwrap();
    // The rows seeded stand alone: no users, roots or scans behind them.
    conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    (dir, conn)
}

/// One live library album holding one file, on group `group`.
fn holding(conn: &Connection, id: &str, artist: &str, title: &str, group: &str, task: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO local_artists (id, display_name, folded_name, kind, \
         created_at, updated_at) VALUES (?1, ?2, ?3, 'person', 1, 1)",
        params![format!("ar-{id}"), artist, fold_text(artist)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at, has_indexed_track) \
         VALUES (?1, 'r', ?1, ?2, ?3, ?4, 'automatic', 1, 1, 1)",
        params![id, title, fold_text(title), format!("ar-{id}")],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, decision_source, selected_at) \
         VALUES (?1, 'musicbrainz', ?2, 'automatic', 1)",
        params![id, group],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, ingest_source, imported_at, \
         membership_source, download_task_id) VALUES (?1, ?2, 'r', ?1, ?1, ?1, 1, 1, 'x', \
         't', 't', ?3, ?4, 'flac', 'download', 1, 'automatic', ?5)",
        params![format!("tr-{id}"), id, title, fold_text(title), task],
    )
    .unwrap();
}

#[test]
fn repairs_link_stand_ins_by_files_and_by_library_album() {
    let (_dir, conn) = scratch();
    // Typewritten: its download's files sit under a MusicBrainz album.
    let typed = virtual_ids::mint(DEEZER, Kind::ReleaseGroup, "100");
    conn.execute(
        "INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name, \
         album_title, status, created_at, updated_at) \
         VALUES ('t1', 'u', ?1, 'Ravyn Lenae', 'Typewritten (Deluxe)', 'partial', 1, 1)",
        params![typed],
    )
    .unwrap();
    holding(&conn, "a1", "Ravyn Lenae", "Typewritten", MB, "t1");
    // Have You Heard?: still asked for, and the library holds it.
    let heard = virtual_ids::mint(DEEZER, Kind::ReleaseGroup, "200");
    conn.execute(
        "INSERT INTO source_identities (uuid, provider, kind, provider_id, \
         provider_album_id, created_at) VALUES (?1, 'deezer', 'release_group', '200', '200', 1)",
        params![heard],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO source_catalogue (provider, provider_album_id, provider_artist_id, \
         artist_name, title, listed_at) VALUES ('deezer', '200', '9', 'Hiatus Kaiyote', \
         'Have You Heard?', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
         album_title, requested_at, status) VALUES (?1, ?1, 'Hiatus Kaiyote', \
         'Have You Heard?', 1, 'failed')",
        params![heard],
    )
    .unwrap();
    holding(
        &conn,
        "a2",
        "Hiatus Kaiyote",
        "Have You Heard?",
        OTHER,
        "none",
    );
    // A download of another album's files proves nothing.
    let wrong = virtual_ids::mint(DEEZER, Kind::ReleaseGroup, "300");
    conn.execute(
        "INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name, \
         album_title, status, created_at, updated_at) \
         VALUES ('t3', 'u', ?1, 'Someone', 'Other Record', 'completed', 1, 1)",
        params![wrong],
    )
    .unwrap();
    holding(
        &conn,
        "a3",
        "Someone",
        "Not It",
        "5b11f4ce-a62d-471e-81fc-a69a8278c7da",
        "t3",
    );

    let mut found = repairs(&conn).unwrap();
    found.sort_by(|a, b| a.stand_in.cmp(&b.stand_in));
    let mut expected = vec![
        Repair {
            stand_in: typed.clone(),
            musicbrainz_id: MB.to_owned(),
            via: LinkVia::Files,
        },
        Repair {
            stand_in: heard.clone(),
            musicbrainz_id: OTHER.to_owned(),
            via: LinkVia::Library,
        },
    ];
    expected.sort_by(|a, b| a.stand_in.cmp(&b.stand_in));
    assert_eq!(found, expected);

    for repair in &found {
        assert!(
            link(
                &conn,
                &repair.stand_in,
                &repair.musicbrainz_id,
                repair.via,
                5
            )
            .unwrap()
        );
    }
    assert_eq!(linked(&conn, &typed).unwrap().as_deref(), Some(MB));
    assert!(group_owned(&conn, &typed).unwrap());
    assert!(group_owned(&conn, &heard).unwrap());
    assert!(!group_owned(&conn, &wrong).unwrap());
    // A stand-in keeps its first link, and a linked one is not due again.
    assert!(!link(&conn, &typed, OTHER, LinkVia::Name, 6).unwrap());
    assert!(repairs(&conn).unwrap().is_empty());
    assert_eq!(linked_open(&conn).unwrap(), vec![heard.clone()]);
}

#[test]
fn a_stand_in_group_lists_its_stand_in_releases() {
    let (_dir, conn) = scratch();
    let group = virtual_ids::mint(DEEZER, Kind::ReleaseGroup, "200");
    conn.execute(
        "INSERT INTO source_identities (uuid, provider, kind, provider_id, \
         provider_album_id, created_at) VALUES (?1, 'deezer', 'release_group', '200', '200', 1)",
        params![group],
    )
    .unwrap();
    assert_eq!(
        releases(&conn, &group).unwrap(),
        vec![virtual_ids::mint(DEEZER, Kind::Release, "200")]
    );
    assert!(releases(&conn, MB).unwrap().is_empty());
}
