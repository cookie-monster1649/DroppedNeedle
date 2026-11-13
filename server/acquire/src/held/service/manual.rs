//! Fork: a file imported by hand onto a still-hunting song.
//!
//! A track download the app keeps retrying can be settled by a person: an
//! audio file they upload, or one fetched from a YouTube or SoundCloud page
//! with yt-dlp ([`super::super::fetch`]). The file is held for the
//! download, paired with the song it was asked for (its release, release
//! track and recording, or its want's ask and identities), and imported at
//! once as a person's choice through the held list's own import: the one
//! intake tags it, names it, compares it with any copy the library has and
//! settles the download, so the want leaves Still hunting. A file the
//! intake would not take stays in the held list with its reason.
//!
//! A fetched file keeps the codec it came in (Opus, AAC or MP3): it lands
//! at the low quality tier its bit rate gives it, so a later upgrade
//! replaces it like any other low copy.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use utoipa::ToSchema;

use super::{Acted, HeldError, HeldService, Viewer};
use crate::held::store::{EntryOrigin, GroupKey, HeldRoot, HeldRows, NewEntry, NewFile, Pairing};
use crate::reason::Reason;

/// What a still-hunting song was asked as.
#[derive(Debug, Clone)]
pub struct ManualTask {
    pub task_id: String,
    pub user_id: String,
    pub download_type: String,
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub recording_mbid: Option<String>,
    pub artist_name: String,
    pub album_title: String,
    pub track_title: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
}

/// A YouTube link the app already knows for a song.
#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
pub struct KnownLink {
    pub url: String,
    pub video_id: String,
    pub title: String,
    pub artist_name: String,
    pub album_name: String,
}

/// How a fetch from a link is going.
#[derive(Debug, Clone, Copy, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManualFetchState {
    Fetching,
    Importing,
    Done,
    Failed,
}

/// One fetch from a link, as the person's tab follows it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManualFetch {
    pub id: String,
    pub task_id: String,
    pub state: ManualFetchState,
    /// Downloaded so far, 0 to 100, while fetching.
    pub percent: Option<f64>,
    /// What went wrong, or what became of the file.
    pub message: Option<String>,
    #[serde(skip)]
    pub user_id: String,
}

/// The fetches started since the server did, newest kept.
#[derive(Default)]
pub struct ManualFetches {
    fetches: std::sync::Mutex<HashMap<String, ManualFetch>>,
}

/// Fetches remembered at most; the oldest finished ones go first.
const FETCHES_KEPT: usize = 200;

impl ManualFetches {
    pub fn put(&self, fetch: ManualFetch) {
        let mut fetches = self.fetches.lock().unwrap_or_else(|e| e.into_inner());
        if fetches.len() >= FETCHES_KEPT {
            let finished: Vec<String> = fetches
                .values()
                .filter(|f| matches!(f.state, ManualFetchState::Done | ManualFetchState::Failed))
                .map(|f| f.id.clone())
                .collect();
            for id in finished {
                fetches.remove(&id);
            }
        }
        fetches.insert(fetch.id.clone(), fetch);
    }

    pub fn update(&self, id: &str, change: impl FnOnce(&mut ManualFetch)) {
        let mut fetches = self.fetches.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(fetch) = fetches.get_mut(id) {
            change(fetch);
        }
    }

    /// A fetch the viewer may see.
    pub fn get(&self, viewer: &Viewer, id: &str) -> Option<ManualFetch> {
        let fetches = self.fetches.lock().unwrap_or_else(|e| e.into_inner());
        fetches
            .get(id)
            .filter(|fetch| viewer.admin || fetch.user_id == viewer.user_id)
            .cloned()
    }
}

const NOT_HUNTING: &str = "That song is not in your downloads any more.";

impl HeldService {
    /// The fork's fetches from links.
    pub fn manual_fetches(&self) -> &ManualFetches {
        &self.manual_fetches
    }

    /// The still-hunting track download `task_id`, when the viewer may act
    /// on it: their own, or any for an admin.
    pub async fn manual_task(
        &self,
        viewer: &Viewer,
        task_id: &str,
    ) -> Result<ManualTask, HeldError> {
        let id = task_id.to_owned();
        let task = self
            .read("held.manual_task", move |rows| manual_task_row(rows, &id))
            .await?
            .filter(|task| viewer.admin || task.user_id == viewer.user_id)
            .ok_or(HeldError::NotFound(NOT_HUNTING))?;
        if task.download_type != "track" {
            return Err(HeldError::Invalid(
                "Only a single song can take one file. Import an album through Import.".to_owned(),
            ));
        }
        Ok(task)
    }

    /// YouTube links the app already knows for the song: by its recording,
    /// its place on its album, or its artist and title.
    pub async fn manual_links(
        &self,
        viewer: &Viewer,
        task_id: &str,
    ) -> Result<Vec<KnownLink>, HeldError> {
        let task = self.manual_task(viewer, task_id).await?;
        self.read("held.manual_links", move |rows| known_links(rows, &task))
            .await
    }

    /// Import the audio file `staged`, already in the held folder, as the
    /// song download `task_id` hunts for. `source` names where it came
    /// from (`upload`, `youtube`, `soundcloud`). The staged file is the
    /// held list's from here on, imported or not.
    pub async fn manual_import(
        &self,
        viewer: &Viewer,
        task_id: &str,
        staged: &Path,
        original_name: &str,
        source: &str,
        actor: &str,
    ) -> Result<Acted, HeldError> {
        let task = match self.manual_task(viewer, task_id).await {
            Ok(task) => task,
            Err(error) => {
                let _ = tokio::fs::remove_file(staged).await;
                return Err(error);
            }
        };
        let name = staged
            .strip_prefix(&self.folders.held)
            .ok()
            .and_then(|path| path.to_str())
            .map(str::to_owned)
            .filter(|name| crate::held::below(&self.folders.held, name).is_some())
            .ok_or_else(|| HeldError::Unavailable("staged file outside the held folder".into()))?;
        let want = task
            .recording_mbid
            .as_deref()
            .is_some_and(crate::requests::ledger::is_want_key);
        let entry = NewEntry {
            user_id: Some(task.user_id.clone()),
            origin: EntryOrigin::Download,
            key: GroupKey::for_download(
                &task.download_type,
                task.recording_mbid.as_deref(),
                Some(task.release_group_mbid.as_str()),
                &task.task_id,
            ),
            release_group_mbid: Some(task.release_group_mbid.clone())
                .filter(|group| !group.is_empty()),
            recording_mbid: task.recording_mbid.clone(),
            drop_item_id: None,
            artist_name: Some(task.artist_name.clone()),
            album_title: Some(task.album_title.clone()),
        };
        // The stored hold reasons are the landing's: a want's file is held
        // as one with no release, a song's as a weak match, and a person's
        // import skips that check.
        let reason = if want {
            Reason::NoRelease
        } else {
            Reason::WeakMatch
        };
        let mut file = NewFile::new(HeldRoot::Held, &name, reason, source);
        file.original_name = original_name.to_owned();
        file.source_task_id = Some(task.task_id.clone());
        file.reason_detail = Some(format!("imported by hand from {source}"));
        file.evidence_title = task.track_title.clone();
        file.evidence_artist = Some(task.artist_name.clone());
        // A want is placed from its ask and identities; a song on a
        // release is paired with its release track.
        if !want {
            file.pairing = Pairing {
                release_mbid: task.release_mbid.clone(),
                release_track_mbid: task.release_track_mbid.clone(),
                recording_mbid: task.recording_mbid.clone(),
                disc_number: task.disc_number,
                track_number: task.track_number,
                track_title: task.track_title.clone(),
                expected_duration_seconds: None,
            };
        }
        let now = self.now();
        let held = self
            .db
            .write_background("held.manual_hold", move |tx| {
                Ok(HeldRows::new(tx).hold(&entry, &[file], now)?)
            })
            .await
            .map_err(HeldError::from);
        let (entry_id, file_id) = match held {
            Ok((entry_id, ids)) => match ids.first().copied().flatten() {
                Some(file_id) => (entry_id, file_id),
                None => {
                    let _ = tokio::fs::remove_file(staged).await;
                    return Err(HeldError::Refused(
                        "That file is already waiting in To decide for this song.".to_owned(),
                    ));
                }
            },
            Err(error) => {
                let _ = tokio::fs::remove_file(staged).await;
                return Err(error);
            }
        };
        tracing::info!(
            task_id,
            entry_id,
            file_id,
            source,
            "manual import held for a still-hunting song"
        );
        let owner = Viewer {
            user_id: task.user_id.clone(),
            admin: viewer.admin,
        };
        self.import(&owner, entry_id, Some(vec![file_id]), actor)
            .await
    }
}

fn manual_task_row(rows: &HeldRows<'_>, task_id: &str) -> rusqlite::Result<Option<ManualTask>> {
    use rusqlite::OptionalExtension as _;
    rows.conn()
        .query_row(crate::held::sql::MANUAL_TASK.text(), [task_id], |row| {
            Ok(ManualTask {
                task_id: row.get(0)?,
                user_id: row.get(1)?,
                download_type: row.get(2)?,
                release_group_mbid: row.get(3)?,
                release_mbid: row.get::<_, Option<String>>(4)?.filter(|v| !v.is_empty()),
                release_track_mbid: row.get::<_, Option<String>>(5)?.filter(|v| !v.is_empty()),
                recording_mbid: row.get::<_, Option<String>>(6)?.filter(|v| !v.is_empty()),
                artist_name: row.get(7)?,
                album_title: row.get(8)?,
                track_title: row.get(9)?,
                track_number: row.get(10)?,
                disc_number: row.get(11)?,
            })
        })
        .optional()
}

fn known_links(rows: &HeldRows<'_>, task: &ManualTask) -> rusqlite::Result<Vec<KnownLink>> {
    let mut statement = rows.conn().prepare(crate::held::sql::MANUAL_LINKS.text())?;
    let links = statement.query_map(
        rusqlite::params![
            task.recording_mbid,
            task.release_group_mbid,
            task.track_number,
            task.disc_number,
            task.track_title,
            task.artist_name,
        ],
        |row| {
            let video_id: String = row.get(0)?;
            Ok(KnownLink {
                url: format!("https://www.youtube.com/watch?v={video_id}"),
                video_id,
                title: row.get(1)?,
                artist_name: row.get(2)?,
                album_name: row.get(3)?,
            })
        },
    )?;
    links
        .filter(|link| {
            link.as_ref().map_or(true, |link| {
                !link.video_id.is_empty()
                    && link
                        .video_id
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            })
        })
        .collect()
}
