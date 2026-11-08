//! Fork: Spotify playlists kept in step with Spotify (Slice C).
//!
//! A playlist imported from Spotify may have sync turned on
//! (`acquire_spotify_playlist_syncs`, one row per synced playlist). The
//! loop reads each synced playlist's snapshot id, its version at Spotify,
//! and imports it again only when the snapshot changed. The import mirrors
//! the playlist (songs added at Spotify appear, songs removed go), through
//! the same populate the import job runs. With auto-request on, the songs
//! new since the last import are asked for on the owner's behalf: a track
//! request when MusicBrainz proved the recording by ISRC, else a want
//! carrying the Spotify track id and ISRC, so the download is judged
//! against Spotify's title, artist and length and files by itself
//! (`landing::want`). The playlist linker attaches each song once it lands.
//!
//! A song is new when neither its Spotify id nor its artist and title
//! (folded as a want key) were on the playlist before, so rows imported
//! before entries kept Spotify ids are not asked for again. Songs the
//! library already holds are linked by the import and not asked for.
//!
//! Requests go through [`SyncRequests`]: [`RequestsSync`] in the server,
//! which applies the owner's approval gate and quota as if they had asked.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_auth::{Principal, Role};
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::{AlreadyRunning, BoxFuture, FnTick, Next, Schedule, Supervisor};
use rusqlite::{OptionalExtension, params};

use super::spotify::{ImportFailure, ImportedTrack, SpotifyImportService};
use super::spotify_client::SpotifyError;
use super::spotify_store::CollectionsPlaylistBridge;
use super::sql;
use crate::db::AcquireDb;
use crate::requests::error::RequestsError;
use crate::requests::ledger::want_key;
use crate::requests::models::{TrackIntake, WantIntake};
use crate::requests::service::RequestsService;
use crate::requests::state::RequestsState;

/// Name of the sync loop.
pub const SYNC_JOB: &str = "spotify-playlist-sync";

/// How often synced playlists are checked. A check whose snapshot did not
/// change costs one small Spotify call per playlist.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// The most songs one sync asks for, so a playlist that grew by hundreds
/// overnight does not flood the queue; the rest are asked for by hand.
pub const MAX_SYNC_REQUESTS: usize = 50;

/// One playlist's sync setting, as the page shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncSetting {
    /// Songs added at Spotify are requested.
    pub auto_request: bool,
    /// Epoch milliseconds of the last check.
    pub checked_at: Option<i64>,
    /// Why the last check failed.
    pub last_error: Option<String>,
}

/// One synced playlist, as the loop reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncRow {
    /// Internal playlist id.
    pub playlist_id: String,
    /// Its owner.
    pub user_id: String,
    /// The Spotify playlist id.
    pub spotify_playlist_id: String,
    /// Songs added at Spotify are requested.
    pub auto_request: bool,
    /// The snapshot last imported.
    pub snapshot_id: Option<String>,
}

/// The sync settings over the shared database.
#[derive(Clone)]
pub struct SpotifySyncs {
    db: AcquireDb,
}

impl SpotifySyncs {
    /// Settings over the serving database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// The database the settings live in (the playlist row states read it).
    pub fn db(&self) -> &AcquireDb {
        &self.db
    }

    /// The setting of `user_id`'s playlist `playlist_id`; `Ok(None)` when it
    /// is not synced (a playlist not from Spotify never is), `Err` with
    /// [`SyncSettingError::NotFound`] when it is not their playlist.
    pub async fn get(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Option<SyncSetting>, SyncSettingError> {
        let (user_id, playlist_id) = (user_id.to_owned(), playlist_id.to_owned());
        self.db
            .db()
            .read("spotify.sync.get", move |conn| {
                if playlist_source(conn, &user_id, &playlist_id)?.is_none() {
                    return Ok(Err(SyncSettingError::NotFound));
                }
                Ok(Ok(conn
                    .query_row(sql::SYNC_OF.text(), params![playlist_id], |row| {
                        Ok(SyncSetting {
                            auto_request: row.get::<_, i64>(0)? != 0,
                            checked_at: row.get(1)?,
                            last_error: row.get(2)?,
                        })
                    })
                    .optional()?))
            })
            .await
            .map_err(SyncSettingError::Store)?
    }

    /// Turn sync on (with `auto_request`) or off for `user_id`'s playlist.
    pub async fn set(
        &self,
        user_id: &str,
        playlist_id: &str,
        enabled: bool,
        auto_request: bool,
    ) -> Result<(), SyncSettingError> {
        let (user_id, playlist_id) = (user_id.to_owned(), playlist_id.to_owned());
        let now = self.db.clock().now_ms();
        self.db
            .write("spotify.sync.set", move |tx| {
                match playlist_source(tx, &user_id, &playlist_id)? {
                    None => return Ok(Err(SyncSettingError::NotFound)),
                    Some(false) if enabled => return Ok(Err(SyncSettingError::NotSpotify)),
                    Some(_) => {}
                }
                if enabled {
                    tx.execute(
                        sql::UPSERT_SYNC.text(),
                        params![playlist_id, i64::from(auto_request), now],
                    )?;
                } else {
                    tx.execute(sql::DELETE_SYNC.text(), params![playlist_id])?;
                }
                Ok(Ok(()))
            })
            .await
            .map_err(SyncSettingError::Store)?
    }

    /// Every synced playlist, least recently checked first.
    pub async fn rows(&self) -> Result<Vec<SyncRow>, DbError> {
        self.db
            .db()
            .read("spotify.sync.rows", |conn| {
                conn.prepare(sql::SYNC_ROWS.text())?
                    .query_map([], |row| {
                        Ok(SyncRow {
                            playlist_id: row.get(0)?,
                            user_id: row.get(1)?,
                            spotify_playlist_id: row.get(2)?,
                            auto_request: row.get::<_, i64>(3)? != 0,
                            snapshot_id: row.get(4)?,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()
                    .map_err(DbError::from)
            })
            .await
    }

    /// Record one check: the snapshot imported (kept when `None`), when,
    /// and why it failed.
    pub async fn record(
        &self,
        playlist_id: &str,
        snapshot_id: Option<String>,
        error: Option<String>,
    ) -> Result<(), DbError> {
        let playlist_id = playlist_id.to_owned();
        let now = self.db.clock().now_ms();
        self.db
            .write("spotify.sync.record", move |tx| {
                tx.execute(
                    sql::RECORD_SYNC.text(),
                    params![playlist_id, snapshot_id, now, error],
                )?;
                Ok(())
            })
            .await
    }
}

/// `None` when `playlist_id` is not `user_id`'s; else whether it is from
/// Spotify.
fn playlist_source(
    conn: &rusqlite::Connection,
    user_id: &str,
    playlist_id: &str,
) -> rusqlite::Result<Option<bool>> {
    conn.query_row(
        sql::OWNED_PLAYLIST_SOURCE.text(),
        params![playlist_id, user_id],
        |row| Ok(row.get::<_, i64>(0)? != 0),
    )
    .optional()
}

/// Why a sync setting could not be read or written.
#[derive(Debug)]
pub enum SyncSettingError {
    /// Not the caller's playlist.
    NotFound,
    /// Sync asked for on a playlist not imported from Spotify.
    NotSpotify,
    /// The database failed.
    Store(DbError),
}

/// Asks for the songs a sync found, on the playlist owner's behalf.
pub trait SyncRequests: Send + Sync {
    /// Ask for `tracks` for `user_id`; answers how many were asked for.
    fn request<'a>(&'a self, user_id: &'a str, tracks: Vec<ImportedTrack>) -> BoxFuture<'a, usize>;
}

/// What one sync of one playlist did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Synced {
    /// The snapshot had not changed: nothing was imported.
    pub unchanged: bool,
    /// Songs new since the last import (the library holds none of them).
    pub added: usize,
    /// Of those, how many were asked for.
    pub requested: usize,
}

/// The sync: checks synced playlists and imports the changed ones.
pub struct SpotifySync {
    service: Arc<SpotifyImportService>,
    playlists: Arc<CollectionsPlaylistBridge>,
    syncs: SpotifySyncs,
    requests: Arc<dyn SyncRequests>,
}

impl SpotifySync {
    /// The sync over its parts.
    pub fn new(
        service: Arc<SpotifyImportService>,
        playlists: Arc<CollectionsPlaylistBridge>,
        syncs: SpotifySyncs,
        requests: Arc<dyn SyncRequests>,
    ) -> Self {
        Self {
            service,
            playlists,
            syncs,
            requests,
        }
    }

    /// Check every synced playlist once; answers how many were imported
    /// again. One playlist's failure is recorded on it and the rest go on.
    pub async fn run_once(&self) -> usize {
        let rows = match self.syncs.rows().await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "synced playlists unreadable");
                return 0;
            }
        };
        let mut imported = 0;
        for row in rows {
            match self.sync(&row).await {
                Ok(synced) => {
                    if !synced.unchanged {
                        imported += 1;
                        tracing::info!(
                            playlist_id = %row.playlist_id,
                            added = synced.added,
                            requested = synced.requested,
                            "spotify playlist synced"
                        );
                    }
                    if let Err(error) = self.syncs.record(&row.playlist_id, None, None).await {
                        tracing::warn!(%error, "playlist sync not recorded");
                    }
                }
                Err(failure) => {
                    let message = failure_message(&failure);
                    tracing::warn!(playlist_id = %row.playlist_id, %message, "spotify playlist sync failed");
                    if let Err(error) = self
                        .syncs
                        .record(&row.playlist_id, None, Some(message))
                        .await
                    {
                        tracing::warn!(%error, "playlist sync failure not recorded");
                    }
                }
            }
        }
        imported
    }

    /// Sync one playlist: import it when its snapshot changed and ask for
    /// the songs new since the last import.
    pub async fn sync(&self, row: &SyncRow) -> Result<Synced, ImportFailure> {
        let snapshot = self
            .service
            .snapshot(&row.user_id, &row.spotify_playlist_id)
            .await?;
        if snapshot.is_some() && snapshot == row.snapshot_id {
            return Ok(Synced {
                unchanged: true,
                ..Synced::default()
            });
        }
        let before = self.entries(row).await?;
        let mut known: HashSet<String> = HashSet::new();
        for entry in &before {
            if let Some(id) = &entry.spotify_track_id {
                known.insert(id.clone());
            }
            known.insert(want_key(&entry.artist_name, &entry.track_name));
        }
        let populated = self
            .service
            .populate(&row.user_id, &row.spotify_playlist_id, &row.playlist_id)
            .await?;
        // The import linked what the library holds already.
        let linked: HashSet<String> = self
            .entries(row)
            .await?
            .into_iter()
            .filter(|entry| entry.library_file_id.is_some())
            .filter_map(|entry| entry.spotify_track_id)
            .collect();
        let added: Vec<ImportedTrack> = populated
            .tracks
            .into_iter()
            .filter(|track| {
                let by_id = track
                    .spotify_track_id
                    .as_ref()
                    .is_some_and(|id| known.contains(id) || linked.contains(id));
                !by_id && !known.contains(&want_key(&track.artist_name, &track.track_name))
            })
            .collect();
        let mut synced = Synced {
            added: added.len(),
            ..Synced::default()
        };
        if row.auto_request && !added.is_empty() {
            let asked: Vec<ImportedTrack> = added.into_iter().take(MAX_SYNC_REQUESTS).collect();
            synced.requested = self.requests.request(&row.user_id, asked).await;
        }
        // Recorded only once the import and its asks are done, so a sync cut
        // short is done again.
        let snapshot = populated.snapshot_id.or(snapshot);
        self.syncs
            .record(&row.playlist_id, snapshot, None)
            .await
            .map_err(|error| SpotifyError::Store(format!("record playlist sync: {error}")))?;
        Ok(synced)
    }

    async fn entries(
        &self,
        row: &SyncRow,
    ) -> Result<Vec<droppedneedle_catalog::collections::models::PlaylistTrack>, SpotifyError> {
        self.playlists.entries(&row.user_id, &row.playlist_id).await
    }
}

fn failure_message(failure: &ImportFailure) -> String {
    match failure {
        ImportFailure::Settings(_) => "Spotify app settings do not read".to_owned(),
        ImportFailure::Spotify(SpotifyError::NotLinked) => "Spotify account not linked".to_owned(),
        ImportFailure::Spotify(SpotifyError::RateLimited { .. }) => {
            "Spotify is limiting requests".to_owned()
        }
        ImportFailure::Spotify(SpotifyError::Unavailable(_)) => {
            "Spotify could not be reached".to_owned()
        }
        ImportFailure::Spotify(SpotifyError::Busy) => "The database was busy".to_owned(),
        ImportFailure::Spotify(SpotifyError::Store(_)) => {
            "The playlist could not be saved".to_owned()
        }
    }
}

/// Start the sync loop on `supervisor`.
pub fn spawn_spotify_sync(
    supervisor: &Supervisor,
    sync: Arc<SpotifySync>,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        SYNC_JOB,
        Schedule::new(SYNC_INTERVAL)
            .with_initial_delay(Duration::from_secs(120))
            .with_jitter(Duration::from_secs(60)),
        FnTick(move |_| {
            let sync = sync.clone();
            async move {
                sync.run_once().await;
                Ok(Next::Continue)
            }
        }),
    )
}

/// [`SyncRequests`] through the request service, as the playlist owner.
pub struct RequestsSync {
    requests: RequestsState,
}

impl RequestsSync {
    /// Requests through `requests`.
    pub fn new(requests: RequestsState) -> Self {
        Self { requests }
    }

    async fn owner(&self, user_id: &str) -> Option<Principal> {
        let id = user_id.to_owned();
        let row: Result<Option<(String, Option<String>)>, DbError> = self
            .requests
            .library
            .db()
            .read("spotify.sync.owner", move |conn| {
                conn.query_row(sql::SYNC_OWNER.text(), [id], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()
                .map_err(DbError::from)
            })
            .await;
        match row {
            Ok(Some((role, name))) => Some(Principal::acting_for(
                user_id.to_owned(),
                Role::from_stored(&role),
                name,
            )),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, "playlist owner unreadable; nothing requested");
                None
            }
        }
    }
}

impl SyncRequests for RequestsSync {
    fn request<'a>(&'a self, user_id: &'a str, tracks: Vec<ImportedTrack>) -> BoxFuture<'a, usize> {
        Box::pin(async move {
            let Some(principal) = self.owner(user_id).await else {
                return 0;
            };
            let service = RequestsService::new(&self.requests);
            let mut asked = 0;
            for track in tracks {
                let album_title = Some(track.album_name.clone()).filter(|name| !name.is_empty());
                let result = match track.recording_mbid.clone() {
                    Some(recording_mbid) => service
                        .request_track(
                            &principal,
                            &TrackIntake {
                                recording_mbid,
                                artist_name: track.artist_name.clone(),
                                track_title: track.track_name.clone(),
                                album_title,
                                duration_seconds: track.duration,
                                release_group_mbid: Some(track.album_id.clone())
                                    .filter(|id| !id.is_empty()),
                                artist_mbid: None,
                                release_mbid: None,
                            },
                        )
                        .await
                        .map(|_| ()),
                    None => service
                        .request_want(
                            &principal,
                            &WantIntake {
                                artist_name: track.artist_name.clone(),
                                track_title: track.track_name.clone(),
                                album_title,
                                duration_seconds: track.duration,
                                isrc: track.isrc.clone(),
                                spotify_track_id: track.spotify_track_id.clone(),
                            },
                        )
                        .await
                        .map(|_| ()),
                };
                match result {
                    Ok(()) => asked += 1,
                    Err(
                        RequestsError::QuotaExceeded { .. } | RequestsError::StorageFull { .. },
                    ) => {
                        tracing::info!(user_id, "playlist sync: request quota reached; stopping");
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(?error, track = %track.track_name, "playlist sync: request failed");
                    }
                }
            }
            asked
        })
    }
}
