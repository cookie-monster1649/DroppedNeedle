//! The stand-in link and settle tests.

use super::*;
use droppedneedle_providers::virtual_ids::{Kind, mint};

const MB: &str = "ea8e626f-520f-4f54-9fab-8004e702afef";

#[test]
fn only_stand_ins_of_the_placed_title_link_to_a_real_group() {
    let typed = mint("deezer", Kind::ReleaseGroup, "1");
    let other = mint("deezer", Kind::ReleaseGroup, "2");
    let asked = [
        (typed.as_str(), "Typewritten (Deluxe)"),
        (other.as_str(), "Another Album"),
        (MB, "Typewritten"),
        (typed.as_str(), "Typewritten"),
    ];
    assert_eq!(
        stand_ins_to_link(asked, MB, "Typewritten"),
        vec![typed.clone()]
    );
    assert!(stand_ins_to_link(asked, &other, "Typewritten").is_empty());
    assert!(stand_ins_to_link(asked, "", "Typewritten").is_empty());
}

const OTHER_MB: &str = "1dbbabcb-6902-40ec-8a7f-bcb1aa15d6bd";

/// A user, a link from a stand-in to `group`, a queued download and a
/// failed one for the stand-in, a wanted watch on it, and a held entry
/// with a file held as no release.
async fn seeded(group: &str, owned: bool) -> (AcquireDb, String) {
    let db = AcquireDb::scratch().unwrap();
    db.add_user("u1", "Ada", "admin").await.unwrap();
    let stand_in = mint("deezer", Kind::ReleaseGroup, "42");
    let (s, g) = (stand_in.clone(), group.to_owned());
    db.write("test.seed_stand_in", move |tx| {
        links::link(tx, &s, &g, LinkVia::Files, 1)?;
        for (id, status) in [("t-queued", "queued"), ("t-failed", "failed")] {
            tx.execute(
                "INSERT INTO download_tasks (id, user_id, release_group_mbid, \
                 release_mbid, artist_name, album_title, origin, status, created_at, \
                 updated_at, search_job_id) \
                 VALUES (?1, 'u1', ?2, ?3, 'Eels', 'Daisies', 'retry', ?4, 1, 1, 'job')",
                params![id, s, mint("deezer", Kind::Release, "42"), status],
            )?;
        }
        tx.execute(
            "INSERT INTO wanted_watches (release_group_mbid_lower, release_group_mbid, \
             user_id, artist_name, album_title, kind, created_at, next_check_at) \
             VALUES (?1, ?1, 'u1', 'Eels', 'Daisies', 'missing', 1, 5)",
            [&s],
        )?;
        tx.execute(
            "INSERT INTO acquire_held_entries (id, user_id, origin, group_key, \
             release_group_mbid, state, created_at, expires_at) \
             VALUES (7, 'u1', 'download', ?1, ?2, 'held', 1, 999999999999)",
            params![format!("album:{s}"), s],
        )?;
        tx.execute(
            "INSERT INTO acquire_held_files (entry_id, root, path, original_name, source, \
             reason, state, created_at) \
             VALUES (7, 'held', 'a/1.flac', '1.flac', 'download', 'no_release', 'held', 1)",
            [],
        )?;
        if owned {
            tx.execute_batch(&format!(
                "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
                   updated_at) VALUES ('ar1', 'Eels', 'eels', 'group', 0, 0);
                 INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                   album_artist_id, album_artist_name, grouping_source, created_at, \
                   updated_at, has_indexed_track) VALUES ('al1', 'r1', 'g', 'Daisies', \
                   'daisies', 'ar1', 'Eels', 'automatic', 0, 0, 1);
                 INSERT INTO local_album_external_identities (local_album_id, \
                   release_group_mbid, decision_source, selected_at) \
                   VALUES ('al1', '{g}', 'manual', 0);"
            ))?;
        }
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    (db, stand_in)
}

async fn settle(db: &AcquireDb, stand_in: &str, group: &str) -> Settled {
    let (s, g) = (stand_in.to_owned(), group.to_owned());
    db.write("test.settle", move |tx| {
        Ok::<_, DbError>(settle_linked(tx, &s, &g, 2_000)?.1)
    })
    .await
    .unwrap()
}

async fn one(db: &AcquireDb, text: &'static str, key: &str) -> String {
    let key = key.to_owned();
    db.db()
        .read("test.one", move |conn| {
            conn.query_row(text, [key], |row| row.get::<_, String>(0))
                .map_err(DbError::from)
        })
        .await
        .unwrap()
}

// A linked stand-in whose group the library holds: its downloads stop
// as already in the library, its watch is fulfilled, and its held
// files are decided as already owned, their copies untouched.
#[tokio::test]
async fn a_linked_stand_in_held_by_the_library_is_settled() {
    let (db, stand_in) = seeded(MB, true).await;
    let done = settle(&db, &stand_in, MB).await;
    assert_eq!(done.tasks_stopped, 2);
    assert_eq!(done.held_owned, 1);
    for id in ["t-queued", "t-failed"] {
        assert_eq!(
            one(
                &db,
                "SELECT status || ':' || outcome_code FROM download_tasks WHERE id = ?1",
                id
            )
            .await,
            "cancelled:already_in_library"
        );
    }
    assert_eq!(
        one(
            &db,
            "SELECT state FROM wanted_watches WHERE release_group_mbid_lower = ?1",
            &stand_in
        )
        .await,
        "fulfilled"
    );
    assert_eq!(
        one(
            &db,
            "SELECT state FROM acquire_held_files WHERE path = ?1",
            "a/1.flac"
        )
        .await,
        "already_owned"
    );
    assert_eq!(
        one(
            &db,
            "SELECT state FROM acquire_held_entries WHERE group_key = ?1",
            &format!("album:{stand_in}")
        )
        .await,
        "resolved"
    );
    // A second pass finds nothing left to do.
    assert!(!settle(&db, &stand_in, MB).await.any());
}

// A linked stand-in whose group the library lacks: the queued download
// searches for the group, the watch moves to it, and the held entry is
// pointed at it for the held list's own clear match.
#[tokio::test]
async fn a_linked_stand_in_not_held_moves_to_its_group() {
    let (db, stand_in) = seeded(OTHER_MB, false).await;
    let done = settle(&db, &stand_in, OTHER_MB).await;
    assert_eq!(done.tasks_moved, 1);
    assert_eq!(done.watches_moved, 1);
    assert_eq!(done.held_moved, 1);
    assert_eq!(
        one(&db, "SELECT release_group_mbid || ':' || COALESCE(release_mbid, '-') || ':' || COALESCE(search_job_id, '-') FROM download_tasks WHERE id = ?1", "t-queued").await,
        format!("{OTHER_MB}:-:-")
    );
    assert_eq!(
        one(
            &db,
            "SELECT release_group_mbid FROM download_tasks WHERE id = ?1",
            "t-failed"
        )
        .await,
        stand_in
    );
    assert_eq!(
        one(
            &db,
            "SELECT state FROM wanted_watches WHERE release_group_mbid_lower = ?1",
            OTHER_MB
        )
        .await,
        "watching"
    );
    assert_eq!(
        one(
            &db,
            "SELECT state FROM wanted_watches WHERE release_group_mbid_lower = ?1",
            &stand_in
        )
        .await,
        "stopped"
    );
    assert_eq!(
        one(
            &db,
            "SELECT release_group_mbid FROM acquire_held_entries WHERE id = ?1",
            "7"
        )
        .await,
        OTHER_MB
    );
    assert_eq!(
        one(
            &db,
            "SELECT reason || ':' || state FROM acquire_held_files WHERE path = ?1",
            "a/1.flac"
        )
        .await,
        "weak_match:held"
    );
}
