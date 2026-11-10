//! Fork: where each entry of a playlist stands for its owner, so the
//! playlist page can show a missing song the way the album page shows a
//! missing track (Missing, Fetching, Held, Failed, Removed) with its request button.
//!
//! An entry's track key is its MusicBrainz recording when it has one, else
//! the want key its artist and title give (`requests::ledger::want_key`):
//! the key a playlist request asks under, so the request ledger and the
//! held list answer for it.

use rusqlite::{OptionalExtension as _, params};
use serde::Serialize;
use utoipa::ToSchema;

use super::sql;
use crate::db::AcquireDb;
use crate::requests::ledger::want_key;

/// Where one playlist entry stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistRowState {
    /// The library plays it.
    Owned,
    /// Nobody asked for it yet.
    Missing,
    /// Asked for, waiting for approval or a fetch to start.
    Requested,
    /// Being fetched.
    Fetching,
    /// A downloaded file waits in the held list for a decision.
    Held,
    /// The last ask found nothing usable, or was cancelled.
    Failed,
    /// Landed; the playlist links it within a couple of minutes.
    Imported,
    /// Its file was removed by hand. Syncs don't ask for it again; a person
    /// may, and that ask fetches it anew.
    Removed,
}

/// One entry's state.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlaylistRowStatus {
    pub track_id: String,
    pub state: PlaylistRowState,
}

/// Every entry's state, in playlist order.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlaylistRowsResponse {
    pub rows: Vec<PlaylistRowStatus>,
}

/// The state a request status and a held file give.
fn state_of(held: bool, status: Option<&str>) -> PlaylistRowState {
    if held {
        return PlaylistRowState::Held;
    }
    match status {
        None => PlaylistRowState::Missing,
        Some("pending" | "awaiting_approval") => PlaylistRowState::Requested,
        Some("queued" | "downloading") => PlaylistRowState::Fetching,
        Some("imported") => PlaylistRowState::Imported,
        Some(_) => PlaylistRowState::Failed,
    }
}

/// The track key of an entry.
fn key_of(recording: Option<&str>, artist: &str, title: &str) -> Option<String> {
    match recording.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => Some(id.to_ascii_lowercase()),
        None if !artist.trim().is_empty() && !title.trim().is_empty() => {
            Some(want_key(artist, title))
        }
        None => None,
    }
}

/// The states of `user_id`'s playlist `playlist_id`; `None` when it is not
/// theirs.
pub async fn playlist_rows(
    db: &AcquireDb,
    user_id: &str,
    playlist_id: &str,
) -> Result<Option<Vec<PlaylistRowStatus>>, droppedneedle_kernel::db::DbError> {
    let (user_id, playlist_id) = (user_id.to_owned(), playlist_id.to_owned());
    db.db()
        .read("imports.playlist_rows", move |conn| {
            let owned: Option<i64> = conn
                .query_row(
                    sql::OWNED_PLAYLIST_SOURCE.text(),
                    params![playlist_id, user_id],
                    |row| row.get(0),
                )
                .optional()?;
            if owned.is_none() {
                return Ok(None);
            }
            let mut entries = conn.prepare_cached(sql::ROW_ENTRIES.text())?;
            let entries = entries
                .query_map(params![playlist_id, user_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        row.get::<_, bool>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut held = conn.prepare_cached(sql::ROW_HELD.text())?;
            let mut request = conn.prepare_cached(sql::ROW_REQUEST.text())?;
            let mut declined = conn.prepare_cached(sql::ROW_DECLINED.text())?;
            let mut rows = Vec::with_capacity(entries.len());
            for (track_id, recording, artist, title, linked, spotify) in entries {
                let state = if linked {
                    PlaylistRowState::Owned
                } else if let Some(key) = key_of(recording.as_deref(), &artist, &title) {
                    let is_held: bool = held.query_row(params![user_id, key], |row| row.get(0))?;
                    let asked: Option<(String, i64)> = request
                        .query_row(params![key], |row| Ok((row.get(0)?, row.get(1)?)))
                        .optional()?;
                    let removed_at: Option<i64> =
                        if artist.trim().is_empty() && title.trim().is_empty() && spotify.is_none()
                        {
                            None
                        } else {
                            declined
                                .query_row(
                                    params![playlist_id, want_key(&artist, &title), spotify],
                                    |row| row.get(0),
                                )
                                .optional()?
                        };
                    // A removal outranks the ask that fetched the file; an
                    // ask made after it fetches the song again.
                    match removed_at {
                        Some(at)
                            if !is_held && asked.as_ref().is_none_or(|(_, asked)| *asked < at) =>
                        {
                            PlaylistRowState::Removed
                        }
                        _ => state_of(is_held, asked.as_ref().map(|(status, _)| status.as_str())),
                    }
                } else {
                    PlaylistRowState::Missing
                };
                rows.push(PlaylistRowStatus { track_id, state });
            }
            Ok(Some(rows))
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_file_outranks_the_request_status() {
        assert_eq!(state_of(true, Some("failed")), PlaylistRowState::Held);
        assert_eq!(state_of(false, None), PlaylistRowState::Missing);
        assert_eq!(
            state_of(false, Some("awaiting_approval")),
            PlaylistRowState::Requested
        );
        assert_eq!(
            state_of(false, Some("downloading")),
            PlaylistRowState::Fetching
        );
        assert_eq!(state_of(false, Some("cancelled")), PlaylistRowState::Failed);
    }

    #[test]
    fn an_entry_without_a_recording_is_keyed_by_its_want() {
        assert_eq!(key_of(Some(" ABC "), "a", "b").as_deref(), Some("abc"));
        assert_eq!(
            key_of(None, "Mr. Scruff", "So Long"),
            Some(want_key("Mr. Scruff", "So Long"))
        );
        assert_eq!(key_of(None, "", "So Long"), None);
    }
}
