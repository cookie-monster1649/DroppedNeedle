//! Fork: Spotify playlists kept in step with Spotify (Slice C).
//!
//! A playlist imported from Spotify may have sync turned on
//! (`acquire_spotify_playlist_syncs`, one row per synced playlist). Each
//! pass reads the owner's playlist list once (`/me/playlists`, 50 a page),
//! which carries every playlist's snapshot id, its version at Spotify, and
//! imports a playlist again only when its snapshot changed: an unchanged
//! playlist costs no call of its own, and a changed one only its items.
//! Liked Songs has no snapshot: its newest page stands in for one, and a
//! change reads down to the first page already known (every page once a
//! day). The import mirrors
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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_auth::{Principal, Role};
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::{AlreadyRunning, BoxFuture, FnTick, Next, Schedule, Supervisor};
use rusqlite::{OptionalExtension, params};

use super::spotify::{ImportFailure, ImportedTrack, SpotifyImportService};
use super::spotify_albums::AlbumResolver;
use super::spotify_client::{LIKED_SONGS, SpotifyError, rate_limited_message, unavailable_message};
use super::spotify_store::CollectionsPlaylistBridge;
use super::spotify_tracks::SpotifySong;
use super::spotify_usage::RefusalKind;
use super::sql;
use crate::db::AcquireDb;
use crate::requests::error::RequestsError;
use crate::requests::ledger::want_key;
use crate::requests::models::{TrackIntake, WantIntake};
use crate::requests::service::RequestsService;
use crate::requests::state::RequestsState;
use droppedneedle_providers::spotify::models::PlaylistInfo;

/// Name of the sync loop.
pub const SYNC_JOB: &str = "spotify-playlist-sync";

/// Fork: name of the loop that keeps a Spotify rate-limit wait across a
/// restart.
pub const COOLDOWN_JOB: &str = "spotify-cooldown-keeper";

/// Fork: how often the wait Spotify asked for is checked and kept.
const COOLDOWN_EVERY: Duration = Duration::from_secs(30);

/// How often synced playlists are checked. Fork: a check costs one
/// `/me/playlists` read per owner (a page per 50 playlists), one Liked Songs
/// page when it is synced, and nothing per unchanged playlist.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Fork: how often a synced Liked Songs is read to the end, for songs
/// unliked below the newest pages.
pub const LIKED_FULL_EVERY_MS: i64 = 24 * 60 * 60 * 1000;

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
    /// Fork: when a synced Liked Songs was last read to the end.
    pub full_read_at: Option<i64>,
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

    /// Fork: the Spotify playlist id of `user_id`'s playlist: `Err(NotFound)`
    /// when it is not theirs, `Ok(None)` when it is not from Spotify.
    pub async fn spotify_id(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Option<String>, SyncSettingError> {
        let (user_id, playlist_id) = (user_id.to_owned(), playlist_id.to_owned());
        let found: Option<Option<String>> = self
            .db
            .db()
            .read("spotify.sync.spotify_id", move |conn| {
                Ok(conn
                    .query_row(
                        sql::OWNED_SPOTIFY_ID.text(),
                        params![playlist_id, user_id],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
            .map_err(SyncSettingError::Store)?;
        found.ok_or(SyncSettingError::NotFound)
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
                            full_read_at: row.get(5)?,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()
                    .map_err(DbError::from)
            })
            .await
    }

    /// Fork: record that `playlist_id` (Liked Songs) was read to the end.
    pub async fn record_full_read(&self, playlist_id: &str) -> Result<(), DbError> {
        let playlist_id = playlist_id.to_owned();
        let now = self.db.clock().now_ms();
        self.db
            .write("spotify.sync.full_read", move |tx| {
                tx.execute(sql::RECORD_FULL_READ.text(), params![playlist_id, now])?;
                Ok(())
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

    /// Fork: songs asked for by name that MusicBrainz now places (title,
    /// artist and length, `spotify_tracks`) are asked for again as
    /// MusicBrainz tracks on their album, and the want is cancelled.
    /// Answers how many moved.
    fn upgrade_wants<'a>(&'a self, _resolver: &'a AlbumResolver) -> BoxFuture<'a, usize> {
        Box::pin(async { 0 })
    }
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
    library: Arc<super::spotify_library::SpotifyLibrary>,
}

impl SpotifySync {
    /// The sync over its parts.
    pub fn new(
        service: Arc<SpotifyImportService>,
        playlists: Arc<CollectionsPlaylistBridge>,
        syncs: SpotifySyncs,
        requests: Arc<dyn SyncRequests>,
        library: Arc<super::spotify_library::SpotifyLibrary>,
    ) -> Self {
        Self {
            service,
            playlists,
            syncs,
            requests,
            library,
        }
    }

    /// Check every synced playlist once; answers how many were imported
    /// again. One playlist's failure is recorded on it and the rest go on.
    pub async fn run_once(&self) -> usize {
        let moved = self.requests.upgrade_wants(self.service.resolver()).await;
        if moved > 0 {
            tracing::info!(moved, "songs asked for by name moved to musicbrainz tracks");
        }
        let rows = match self.syncs.rows().await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "synced playlists unreadable");
                return 0;
            }
        };
        let mut imported = 0;
        // Fork: one playlist list per owner, read on their first playlist
        // that is not Liked Songs.
        let mut listings: HashMap<String, Result<HashMap<String, PlaylistInfo>, String>> =
            HashMap::new();
        for row in rows {
            // Fork: a wait Spotify asked for ends the pass; the next one
            // after it ends goes on.
            if !self.library.cooling_for().is_zero() {
                tracing::info!(
                    wait_secs = self.library.cooling_for().as_secs(),
                    "spotify asked for a wait; sync pass ends"
                );
                self.library.keep_cooldown().await;
                return imported;
            }
            let listed = if row.spotify_playlist_id == LIKED_SONGS {
                None
            } else {
                if !listings.contains_key(&row.user_id) {
                    let listing = self
                        .listing(&row.user_id)
                        .await
                        .map_err(|failure| self.failure(&failure));
                    listings.insert(row.user_id.clone(), listing);
                }
                match &listings[&row.user_id] {
                    Ok(listing) => Some(listing.get(&row.spotify_playlist_id).cloned()),
                    Err(message) => {
                        self.record_failure(&row, message.clone()).await;
                        continue;
                    }
                }
            };
            let outcome = match listed {
                None => self.sync_liked(&row).await,
                Some(Some(info)) => self.sync(&row, &info).await,
                Some(None) => {
                    // Not in the owner's list: unfollowed or deleted at
                    // Spotify. Nothing is called for it.
                    self.record_failure(&row, "No longer in your Spotify playlists".to_owned())
                        .await;
                    continue;
                }
            };
            match outcome {
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
                    let message = self.failure(&failure);
                    self.record_failure(&row, message).await;
                }
            }
        }
        // Fork: followed artists and saved albums.
        self.library.run_once().await;
        imported
    }

    /// Fork: the owner's playlist list, read once a pass: each playlist's
    /// version and cover by Spotify id. Kept for the import page too.
    async fn listing(&self, user_id: &str) -> Result<HashMap<String, PlaylistInfo>, ImportFailure> {
        let (refresher, _) = self
            .service
            .access(
                user_id,
                droppedneedle_providers::RequestPriority::BackgroundSync,
            )
            .await?;
        let me = refresher.spotify_user_id();
        Ok(self
            .service
            .refresh_listing(
                user_id,
                &refresher,
                Some(me.as_str()).filter(|me| !me.is_empty()),
            )
            .await?)
    }

    fn failure(&self, failure: &ImportFailure) -> String {
        failure_message(
            failure,
            self.service.client().refusal_kind(),
            self.library.cooling_for(),
            self.service.now_ms(),
        )
    }

    async fn record_failure(&self, row: &SyncRow, message: String) {
        tracing::warn!(playlist_id = %row.playlist_id, %message, "spotify playlist sync failed");
        if let Err(error) = self
            .syncs
            .record(&row.playlist_id, None, Some(message))
            .await
        {
            tracing::warn!(%error, "playlist sync failure not recorded");
        }
    }

    /// Sync one playlist whose version and cover the owner's list gave
    /// (`listed`): import it when its snapshot changed and ask for the songs
    /// new since the last import. Fork: an unchanged one calls nothing.
    pub async fn sync(
        &self,
        row: &SyncRow,
        listed: &PlaylistInfo,
    ) -> Result<Synced, ImportFailure> {
        let snapshot = listed.snapshot_id.clone();
        if snapshot.is_some() && snapshot == row.snapshot_id {
            return Ok(Synced {
                unchanged: true,
                ..Synced::default()
            });
        }
        let known = self.known(row).await?;
        let populated = self
            .service
            .populate_listed(
                &row.user_id,
                &row.spotify_playlist_id,
                &row.playlist_id,
                listed,
            )
            .await?;
        self.after_import(row, known, populated, snapshot).await
    }

    /// Fork: sync a Liked Songs playlist: its newest page tells whether it
    /// changed; a change reads down to the first known page, or every page
    /// once a day.
    pub async fn sync_liked(&self, row: &SyncRow) -> Result<Synced, ImportFailure> {
        let now = self.service.now_ms();
        let full = row
            .full_read_at
            .is_none_or(|at| now - at >= LIKED_FULL_EVERY_MS);
        // A full read is due: the stored version does not stop it.
        let unchanged_if = if full {
            None
        } else {
            row.snapshot_id.as_deref()
        };
        let known = self.known(row).await?;
        let Some(liked) = self
            .service
            .sync_liked(&row.user_id, &row.playlist_id, unchanged_if, full)
            .await?
        else {
            return Ok(Synced {
                unchanged: true,
                ..Synced::default()
            });
        };
        if liked.complete
            && let Err(error) = self.syncs.record_full_read(&row.playlist_id).await
        {
            tracing::warn!(%error, "liked songs full read not recorded");
        }
        let unchanged =
            liked.populated.snapshot_id.is_some() && liked.populated.snapshot_id == row.snapshot_id;
        let mut synced = self.after_import(row, known, liked.populated, None).await?;
        synced.unchanged = unchanged;
        Ok(synced)
    }

    /// What the playlist held before an import: Spotify ids and want keys.
    async fn known(&self, row: &SyncRow) -> Result<HashSet<String>, ImportFailure> {
        let before = self.entries(row).await?;
        let mut known: HashSet<String> = HashSet::new();
        for entry in &before {
            if let Some(id) = &entry.spotify_track_id {
                known.insert(id.clone());
            }
            known.insert(want_key(&entry.artist_name, &entry.track_name));
        }
        Ok(known)
    }

    /// Ask for the songs an import added and record the snapshot.
    async fn after_import(
        &self,
        row: &SyncRow,
        known: HashSet<String>,
        populated: super::spotify::Populated,
        snapshot: Option<String>,
    ) -> Result<Synced, ImportFailure> {
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

fn failure_message(
    failure: &ImportFailure,
    kind: RefusalKind,
    wait: Duration,
    now_ms: i64,
) -> String {
    match failure {
        ImportFailure::Settings(_) => "Spotify app settings do not read".to_owned(),
        ImportFailure::Spotify(SpotifyError::NotLinked) => "Spotify account not linked".to_owned(),
        ImportFailure::Spotify(SpotifyError::RateLimited { retry_after, quota }) => {
            let kind = if *quota { RefusalKind::Quota } else { kind };
            let wait = if wait.is_zero() {
                Duration::from_secs(retry_after.unwrap_or(0))
            } else {
                wait
            };
            rate_limited_message(kind, wait, now_ms)
        }
        ImportFailure::Spotify(SpotifyError::Unavailable(detail)) => unavailable_message(detail),
        ImportFailure::Spotify(SpotifyError::Busy) => "The database was busy".to_owned(),
        ImportFailure::Spotify(SpotifyError::Store(_)) => {
            "The playlist could not be saved".to_owned()
        }
    }
}

/// Start the sync loop on `supervisor`. Fork: the first pass waits a
/// whole interval, so a restart (a deploy) never calls Spotify by itself;
/// beside it a small loop keeps a rate-limit wait across restarts,
/// restoring a kept one at start.
pub fn spawn_spotify_sync(
    supervisor: &Supervisor,
    sync: Arc<SpotifySync>,
) -> Result<(), AlreadyRunning> {
    let library = sync.library.clone();
    supervisor.periodic(
        COOLDOWN_JOB,
        Schedule::new(COOLDOWN_EVERY),
        FnTick(move |_| {
            let library = library.clone();
            async move {
                library.keep_cooldown().await;
                library.log_usage();
                Ok(Next::Continue)
            }
        }),
    )?;
    supervisor.periodic(
        SYNC_JOB,
        Schedule::new(SYNC_INTERVAL)
            .with_initial_delay(SYNC_INTERVAL)
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

/// Wants read per upgrade pass.
const UPGRADE_BATCH: i64 = 200;

impl SyncRequests for RequestsSync {
    fn upgrade_wants<'a>(&'a self, resolver: &'a AlbumResolver) -> BoxFuture<'a, usize> {
        Box::pin(async move {
            type Row = (
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<i64>,
                String,
            );
            let rows: Result<Vec<Row>, DbError> = self
                .requests
                .library
                .db()
                .read("spotify.open_wants", |conn| {
                    let mut statement = conn.prepare_cached(sql::OPEN_WANTS.text())?;
                    let rows = statement
                        .query_map([UPGRADE_BATCH], |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                                row.get(6)?,
                            ))
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(rows)
                })
                .await;
            let rows = match rows {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(%error, "wants to upgrade unreadable");
                    return 0;
                }
            };
            let service = RequestsService::new(&self.requests);
            let mut moved = 0;
            for (key, user_id, artist, album, title, seconds, task_id) in rows {
                let Some(title) = title.filter(|title| !title.trim().is_empty()) else {
                    continue;
                };
                let album = album
                    .filter(|album| album != "Single track")
                    .unwrap_or_default();
                let song = SpotifySong {
                    title: title.clone(),
                    artists: artist
                        .split(", ")
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    album: album.clone(),
                    seconds: seconds.and_then(|s| u64::try_from(s).ok()),
                };
                let placed = match resolver.resolve_track(&song).await {
                    Ok(Some(placed)) => placed,
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(%error, "musicbrainz failed; want upgrade waits");
                        break;
                    }
                };
                let Some(principal) = self.owner(&user_id).await else {
                    continue;
                };
                let asked = service
                    .request_track(
                        &principal,
                        &TrackIntake {
                            recording_mbid: placed.recording.clone(),
                            artist_name: artist.clone(),
                            track_title: title.clone(),
                            album_title: Some(album).filter(|album| !album.is_empty()),
                            duration_seconds: seconds,
                            release_group_mbid: Some(placed.group.clone()),
                            artist_mbid: None,
                            release_mbid: None,
                        },
                    )
                    .await;
                if let Err(error) = asked {
                    tracing::info!(%key, ?error, "want not moved: the track request was refused");
                    continue;
                }
                if let Err(error) = self.requests.dispatch.cancel(&task_id).await {
                    tracing::warn!(%key, %error, "want moved but its download not cancelled");
                }
                tracing::info!(
                    %key,
                    recording = %placed.recording,
                    group = %placed.group,
                    "want moved to a musicbrainz track request"
                );
                moved += 1;
            }
            moved
        })
    }

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
                                track_number: None,
                                disc_number: None,
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
