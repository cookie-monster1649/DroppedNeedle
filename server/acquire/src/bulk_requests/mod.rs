//! Fork: bulk playlist requests.
//!
//! An admin asks for every song a playlist holds that the library does not
//! play. The run sends songs at a set rate (songs per hour), may stop after
//! a pilot of N songs for a look, and lists each song with what became of
//! it: queued, on its way, in the library, waiting for a person (held), or
//! failed and why.
//!
//! Two or more songs of one album (a MusicBrainz release group) go as one
//! album download for just those tracks: an edition holding them is found
//! from the songs' recordings, and the download fetches only those
//! positions (as "get missing tracks" does). A song the album download
//! cannot take is asked for on its own, as the playlist sync asks: by its
//! recording when MusicBrainz knows it, else by name. Songs asked for on
//! their own are marked playlist songs, so their album's copy imports on
//! proof (`landing::specs`), and every download a run sends skips the
//! search review.

pub mod http;
pub(crate) mod sql;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_auth::{Principal, Role};
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::{AlreadyRunning, FnTick, Next, Schedule, Supervisor};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::db::AcquireDb;
use crate::dispatch::{DispatchOrigin, DispatchRequest, DispatchTarget};
use crate::downloads::journal::Journal;
use crate::requests::RequestsState;
use crate::requests::error::RequestsError;
use crate::requests::models::{TrackIntake, WantIntake};
use crate::requests::service::RequestsService;
use crate::target::lookup::{AlbumLookup, AlbumRelease};

/// The loop's job name.
pub const BULK_JOB: &str = "acquire.bulk_requests";
/// How often the loop sends what the rate allows.
const TICK_EVERY: Duration = Duration::from_secs(60);
/// The rate when the admin names none: songs per hour.
pub const DEFAULT_PER_HOUR: i64 = 60;
/// The highest rate a run may ask for.
pub const MAX_PER_HOUR: i64 = 3_600;
/// Editions of an album read to find one holding a group's songs.
const EDITIONS_TRIED: usize = 3;
/// One hour in milliseconds.
const HOUR_MS: i64 = 3_600_000;

/// A run's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Sending at its rate.
    Running,
    /// The pilot was sent; waits for a person to go on.
    PilotDone,
    /// Stopped by a person; goes on when resumed.
    Paused,
    /// Everything was sent.
    Done,
    /// Stopped for good.
    Cancelled,
}

impl RunState {
    /// Every state, as the schema lists them.
    pub const ALL: [Self; 5] = [
        Self::Running,
        Self::PilotDone,
        Self::Paused,
        Self::Done,
        Self::Cancelled,
    ];

    /// The stored text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::PilotDone => "pilot_done",
            Self::Paused => "paused",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "running" => Self::Running,
            "pilot_done" => Self::PilotDone,
            "paused" => Self::Paused,
            "done" => Self::Done,
            _ => Self::Cancelled,
        }
    }
}

/// A song's stored state; what became of it after it was sent is read
/// from its download ([`SongState`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    /// Not sent yet.
    Queued,
    /// Asked for.
    Sent,
    /// Could not be asked for.
    Failed,
}

impl ItemState {
    /// Every state, as the schema lists them.
    pub const ALL: [Self; 3] = [Self::Queued, Self::Sent, Self::Failed];

    /// The stored text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sent => "sent",
            Self::Failed => "failed",
        }
    }
}

/// One run as stored.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Run {
    pub id: String,
    pub playlist_id: String,
    /// The playlist's owner, whom the songs are asked for.
    pub user_id: String,
    pub state: RunState,
    /// Songs sent per hour at most.
    pub per_hour: i64,
    /// Songs in the pilot; 0 for none.
    pub pilot: i64,
    /// Epoch milliseconds.
    pub created_at: i64,
    pub updated_at: i64,
}

impl Run {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            playlist_id: row.get(1)?,
            user_id: row.get(2)?,
            state: RunState::parse(&row.get::<_, String>(3)?),
            per_hour: row.get(4)?,
            pilot: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }
}

/// What became of one song.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SongState {
    /// Not sent yet.
    Queued,
    /// Sent; its download is waiting or under way.
    InProgress,
    /// The library plays it, or its download completed.
    Done,
    /// Its files wait for a person (held), or only part of its album came.
    Review,
    /// It could not be asked for, or its download failed.
    Failed,
}

/// One song of a run, with what became of it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Song {
    pub id: i64,
    pub position: i64,
    pub artist_name: String,
    pub track_title: String,
    pub album_title: String,
    pub state: SongState,
    /// Sent within one album download for several songs.
    pub album_group: bool,
    /// Why, for a song in review or failed: the outcome code, else what
    /// the request said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A run with its songs and their counts.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RunStatus {
    pub run: Run,
    pub queued: usize,
    pub in_progress: usize,
    pub done: usize,
    pub review: usize,
    pub failed: usize,
    /// Songs in review or failed, by reason.
    pub reasons: HashMap<String, usize>,
    pub songs: Vec<Song>,
}

/// What a run of a playlist would send, read before anything is sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct Estimate {
    /// Songs the library does not play.
    pub missing: usize,
    /// Albums with two or more of them, each one album download.
    pub album_groups: usize,
    /// Songs in those albums.
    pub grouped_songs: usize,
    /// Songs asked for on their own, by recording.
    pub by_recording: usize,
    /// Songs asked for on their own, by name (no recording known).
    pub by_name: usize,
}

/// Why a run could not start or change.
#[derive(Debug, thiserror::Error)]
pub enum BulkError {
    #[error("playlist not found")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("database: {0}")]
    Db(#[from] DbError),
}

/// A song waiting to be sent.
#[derive(Debug, Clone)]
struct Queued {
    id: i64,
    artist_name: String,
    track_title: String,
    album_title: String,
    release_group_mbid: Option<String>,
    recording_mbid: Option<String>,
    isrc: Option<String>,
    spotify_track_id: Option<String>,
    duration_ms: Option<i64>,
    solo: bool,
}

impl Queued {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            artist_name: row.get(1)?,
            track_title: row.get(2)?,
            album_title: row.get(3)?,
            release_group_mbid: row.get(4)?,
            recording_mbid: row.get(5)?,
            isrc: row.get(6)?,
            spotify_track_id: row.get(7)?,
            duration_ms: row.get(8)?,
            solo: row.get::<_, i64>(9)? != 0,
        })
    }

    /// Whether an album download may take it.
    fn groupable(&self) -> bool {
        !self.solo && self.release_group_mbid.is_some() && self.recording_mbid.is_some()
    }
}

/// A missing song as the playlist lists it.
type Missing = (
    String,
    i64,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

/// An edition and each held song's id and (disc, position) on it.
type Edition = (AlbumRelease, Vec<(i64, (u32, u32))>);

/// How one song (or album of songs) was sent.
enum Sent {
    /// Sent; the run goes on.
    Ok,
    /// The owner's request quota or the library's storage is spent: the
    /// run waits for the next tick.
    Stop,
}

/// Bulk playlist requests.
pub struct BulkRequests {
    db: AcquireDb,
    requests: RequestsState,
    journal: Arc<Journal>,
    lookup: Arc<dyn AlbumLookup>,
    /// One tick at a time.
    pass: tokio::sync::Mutex<()>,
}

impl BulkRequests {
    pub fn new(
        db: AcquireDb,
        requests: RequestsState,
        journal: Arc<Journal>,
        lookup: Arc<dyn AlbumLookup>,
    ) -> Self {
        Self {
            db,
            requests,
            journal,
            lookup,
            pass: tokio::sync::Mutex::new(()),
        }
    }

    fn now_ms(&self) -> i64 {
        self.db.clock().now_ms()
    }

    async fn missing(&self, playlist_id: &str) -> Result<Vec<Missing>, BulkError> {
        let id = playlist_id.to_owned();
        Ok(self
            .db
            .db()
            .read("bulk.missing", move |conn| {
                let mut statement = conn.prepare_cached(sql::MISSING_SONGS.text())?;
                let rows = statement
                    .query_map([id], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                            row.get(9)?,
                        ))
                    })?
                    .collect::<Result<Vec<Missing>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await?)
    }

    /// What a run of `playlist_id` would send. Reads only.
    pub async fn estimate(&self, playlist_id: &str) -> Result<Estimate, BulkError> {
        let missing = self.missing(playlist_id).await?;
        Ok(estimate_of(&missing))
    }

    /// Start a run of `playlist_id`'s missing songs at `per_hour`, stopping
    /// after `pilot` songs when it is not 0. One run per playlist at a time.
    pub async fn start(
        &self,
        playlist_id: &str,
        per_hour: i64,
        pilot: i64,
    ) -> Result<RunStatus, BulkError> {
        if !(1..=MAX_PER_HOUR).contains(&per_hour) {
            return Err(BulkError::Invalid(format!(
                "The rate must be 1 to {MAX_PER_HOUR} songs an hour."
            )));
        }
        if pilot < 0 {
            return Err(BulkError::Invalid(
                "The pilot cannot be negative.".to_owned(),
            ));
        }
        let missing = self.missing(playlist_id).await?;
        let (playlist, now) = (playlist_id.to_owned(), self.now_ms());
        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let id = run_id.clone();
        self.db
            .write("bulk.start", move |tx| {
                let owner: Option<Option<String>> = tx
                    .query_row(sql::PLAYLIST_OWNER.text(), [&playlist], |row| row.get(0))
                    .optional()?;
                let Some(Some(owner)) = owner else {
                    return Ok(Err(BulkError::NotFound));
                };
                let open: Option<String> = tx
                    .query_row(sql::OPEN_RUN.text(), [&playlist], |row| row.get(0))
                    .optional()?;
                if open.is_some() {
                    return Ok(Err(BulkError::Conflict(
                        "This playlist already has a bulk request under way.".to_owned(),
                    )));
                }
                tx.execute(
                    sql::INSERT_RUN.text(),
                    params![id, playlist, owner, per_hour, pilot, now],
                )?;
                let mut insert = tx.prepare_cached(sql::INSERT_ITEM.text())?;
                for (entry, position, title, artist, album, group, recording, isrc, spotify, ms) in
                    missing
                {
                    insert.execute(params![
                        id, position, entry, artist, title, album, group, recording, isrc, spotify,
                        ms
                    ])?;
                }
                Ok(Ok(()))
            })
            .await??;
        tracing::info!(run_id, playlist_id, per_hour, pilot, "bulk request started");
        self.status(&run_id).await
    }

    /// Move run `run_id` to `state`: pause, resume (also past a pilot) or
    /// cancel. A finished or cancelled run stays as it is.
    pub async fn set_state(&self, run_id: &str, state: RunState) -> Result<RunStatus, BulkError> {
        let run = self.run(run_id).await?;
        if matches!(run.state, RunState::Done | RunState::Cancelled) {
            return Err(BulkError::Conflict(
                "The bulk request has ended.".to_owned(),
            ));
        }
        let mut state = state;
        // Resuming past a pilot sends the rest: the pilot is spent.
        if state == RunState::Running && run.state == RunState::PilotDone {
            let id = run_id.to_owned();
            self.db
                .write("bulk.pilot_spent", move |tx| {
                    tx.execute(sql::PILOT_SPENT.text(), [id])?;
                    Ok(())
                })
                .await?;
        }
        if state == RunState::Done {
            state = RunState::Cancelled;
        }
        self.write_state(run_id, state).await?;
        self.status(run_id).await
    }

    async fn write_state(&self, run_id: &str, state: RunState) -> Result<(), DbError> {
        let (id, now) = (run_id.to_owned(), self.now_ms());
        self.db
            .write("bulk.state", move |tx| {
                tx.execute(sql::SET_RUN_STATE.text(), params![id, state.as_str(), now])?;
                Ok(())
            })
            .await
    }

    async fn run(&self, run_id: &str) -> Result<Run, BulkError> {
        let id = run_id.to_owned();
        self.db
            .db()
            .read("bulk.run", move |conn| {
                conn.query_row(sql::RUN.text(), [id], Run::from_row)
                    .optional()
                    .map_err(DbError::from)
            })
            .await?
            .ok_or(BulkError::NotFound)
    }

    /// The latest runs, newest first.
    pub async fn runs(&self) -> Result<Vec<Run>, BulkError> {
        Ok(self
            .db
            .db()
            .read("bulk.runs", |conn| {
                let mut statement = conn.prepare_cached(sql::RUNS.text())?;
                let rows = statement
                    .query_map([], Run::from_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await?)
    }

    /// Run `run_id` with each song and what became of it.
    pub async fn status(&self, run_id: &str) -> Result<RunStatus, BulkError> {
        let run = self.run(run_id).await?;
        let id = run_id.to_owned();
        type ItemRow = (
            i64,
            i64,
            String,
            String,
            String,
            String,
            bool,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
            bool,
        );
        let rows: Vec<ItemRow> = self
            .db
            .db()
            .read("bulk.items", move |conn| {
                let mut statement = conn.prepare_cached(sql::ITEMS.text())?;
                let rows = statement
                    .query_map([id], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get::<_, i64>(6)? != 0,
                            row.get(7)?,
                            row.get(8)?,
                            row.get(9)?,
                            row.get::<_, i64>(10)? != 0,
                            row.get::<_, i64>(11)? != 0,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await?;
        let mut status = RunStatus {
            run,
            queued: 0,
            in_progress: 0,
            done: 0,
            review: 0,
            failed: 0,
            reasons: HashMap::new(),
            songs: Vec::with_capacity(rows.len()),
        };
        for (
            id,
            position,
            artist,
            title,
            album,
            stored,
            group,
            detail,
            task,
            outcome,
            held,
            owned,
        ) in rows
        {
            let (state, reason) =
                song_state(&stored, detail, task.as_deref(), outcome, held, owned);
            match state {
                SongState::Queued => status.queued += 1,
                SongState::InProgress => status.in_progress += 1,
                SongState::Done => status.done += 1,
                SongState::Review => status.review += 1,
                SongState::Failed => status.failed += 1,
            }
            if let Some(reason) = reason.as_ref() {
                *status.reasons.entry(reason.clone()).or_default() += 1;
            }
            status.songs.push(Song {
                id,
                position,
                artist_name: artist,
                track_title: title,
                album_title: album,
                state,
                album_group: group,
                reason,
            });
        }
        Ok(status)
    }

    /// One pass: each running run sends what its rate (and pilot) allow.
    /// Answers how many songs were sent.
    pub async fn tick(&self) -> usize {
        let _pass = self.pass.lock().await;
        let runs = match self
            .db
            .db()
            .read("bulk.running", |conn| {
                let mut statement = conn.prepare_cached(sql::RUNNING.text())?;
                let rows = statement
                    .query_map([], Run::from_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await
        {
            Ok(runs) => runs,
            Err(error) => {
                tracing::warn!(%error, "bulk requests unreadable");
                return 0;
            }
        };
        let mut sent = 0;
        for run in runs {
            match self.tick_run(&run).await {
                Ok(count) => sent += count,
                Err(error) => tracing::warn!(run_id = %run.id, %error, "bulk request pass failed"),
            }
        }
        sent
    }

    async fn counts(&self, run_id: &str, since: i64) -> Result<(i64, i64), DbError> {
        let id = run_id.to_owned();
        self.db
            .db()
            .read("bulk.counts", move |conn| {
                let recent: i64 =
                    conn.query_row(sql::SENT_SINCE.text(), params![id, since], |row| row.get(0))?;
                let handled: i64 =
                    conn.query_row(sql::HANDLED.text(), params![id], |row| row.get(0))?;
                Ok((recent, handled))
            })
            .await
    }

    async fn queued(
        &self,
        sql: &'static str,
        run_id: &str,
        arg: rusqlite::types::Value,
    ) -> Result<Vec<Queued>, DbError> {
        let id = run_id.to_owned();
        self.db
            .db()
            .read("bulk.queued", move |conn| {
                let mut statement = conn.prepare_cached(sql)?;
                let rows = statement
                    .query_map(params![id, arg], Queued::from_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }

    /// Songs whose album download failed for want of a source (the album
    /// sources had no copy, or only refused ones) go again on their own,
    /// where the track sources (plugins among them) can serve them. A
    /// download that held files waits for a person instead.
    async fn requeue_failed_albums(&self, run_id: &str) -> Result<(), DbError> {
        let id = run_id.to_owned();
        let failed: Vec<(i64, Option<String>)> = self
            .db
            .db()
            .read("bulk.failed_albums", move |conn| {
                let mut statement = conn.prepare_cached(sql::FAILED_ALBUM_SONGS.text())?;
                let rows = statement
                    .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await?;
        let again: Vec<(i64, String)> = failed
            .into_iter()
            .filter_map(|(id, code)| {
                let code = code.unwrap_or_else(|| "cancelled".to_owned());
                let source = crate::reason::Reason::from_code(&code)
                    .is_none_or(|reason| reason.kind() == crate::reason::ReasonKind::Source);
                source.then(|| (id, format!("album download failed ({code}); asked alone")))
            })
            .collect();
        if again.is_empty() {
            return Ok(());
        }
        self.db
            .write("bulk.requeue", move |tx| {
                let mut statement = tx.prepare_cached(sql::REQUEUE_SOLO.text())?;
                for (id, detail) in again {
                    statement.execute(params![id, detail])?;
                }
                Ok(())
            })
            .await
    }

    async fn tick_run(&self, run: &Run) -> Result<usize, BulkError> {
        self.requeue_failed_albums(&run.id).await?;
        let now = self.now_ms();
        let (recent, handled) = self.counts(&run.id, now - HOUR_MS).await?;
        let mut budget = run.per_hour - recent;
        if run.pilot > 0 {
            if handled >= run.pilot {
                self.write_state(&run.id, RunState::PilotDone).await?;
                tracing::info!(run_id = %run.id, "bulk request pilot sent; waits for a person");
                return Ok(0);
            }
            budget = budget.min(run.pilot - handled);
        }
        if budget <= 0 {
            return Ok(0);
        }
        let queued = self
            .queued(sql::QUEUED.text(), &run.id, budget.into())
            .await?;
        if queued.is_empty() {
            self.write_state(&run.id, RunState::Done).await?;
            tracing::info!(run_id = %run.id, "bulk request sent everything");
            return Ok(0);
        }
        let Some(owner) = self.owner(&run.user_id).await? else {
            self.write_state(&run.id, RunState::Cancelled).await?;
            tracing::warn!(run_id = %run.id, "bulk request owner gone; cancelled");
            return Ok(0);
        };
        let mut done: HashSet<i64> = HashSet::new();
        let mut sent = 0usize;
        for song in queued {
            if budget <= 0 {
                break;
            }
            if done.contains(&song.id) {
                continue;
            }
            if song.groupable() {
                let group = song.release_group_mbid.clone().unwrap_or_default();
                let siblings = self
                    .queued(sql::QUEUED_OF_ALBUM.text(), &run.id, group.clone().into())
                    .await?;
                if siblings.len() >= 2 {
                    match self.send_album(run, &owner, &group, &siblings).await? {
                        Some(taken) => {
                            let count = i64::try_from(taken.len()).unwrap_or(i64::MAX);
                            budget -= count;
                            sent += taken.len();
                            let solo: Vec<i64> = siblings
                                .iter()
                                .map(|sibling| sibling.id)
                                .filter(|id| !taken.contains(id))
                                .collect();
                            self.mark_solo(solo).await?;
                            done.extend(taken);
                            if done.contains(&song.id) {
                                continue;
                            }
                        }
                        None => {
                            self.mark_solo(siblings.iter().map(|s| s.id).collect())
                                .await?;
                        }
                    }
                }
            }
            match self.send_song(&owner, &song).await? {
                Sent::Ok => {
                    budget -= 1;
                    sent += 1;
                    done.insert(song.id);
                }
                Sent::Stop => break,
            }
        }
        Ok(sent)
    }

    async fn mark_solo(&self, ids: Vec<i64>) -> Result<(), DbError> {
        if ids.is_empty() {
            return Ok(());
        }
        self.db
            .write("bulk.solo", move |tx| {
                let mut statement = tx.prepare_cached(sql::MARK_SOLO.text())?;
                for id in ids {
                    statement.execute([id])?;
                }
                Ok(())
            })
            .await
    }

    async fn owner(&self, user_id: &str) -> Result<Option<Principal>, DbError> {
        let id = user_id.to_owned();
        let row: Option<(String, Option<String>)> = self
            .db
            .db()
            .read("bulk.owner", move |conn| {
                conn.query_row(sql::OWNER.text(), [id], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()
                .map_err(DbError::from)
            })
            .await?;
        Ok(row.map(|(role, name)| {
            Principal::acting_for(user_id.to_owned(), Role::from_stored(&role), name)
        }))
    }

    /// Send `siblings` (two or more songs of album `group`) as one album
    /// download for just their tracks. Answers the songs it took, or `None`
    /// when no edition holds two of them (they then go one by one).
    async fn send_album(
        &self,
        run: &Run,
        owner: &Principal,
        group: &str,
        siblings: &[Queued],
    ) -> Result<Option<Vec<i64>>, BulkError> {
        let Some((release, positions)) = self.edition_for(group, siblings).await else {
            return Ok(None);
        };
        let wanted: Vec<(u32, u32)> = positions.iter().map(|(_, at)| *at).collect();
        let artist = if release.various_artists || release.artist.is_empty() {
            siblings[0].artist_name.clone()
        } else {
            release.artist.clone()
        };
        let request = DispatchRequest {
            user_id: owner.user_id().to_owned(),
            target: DispatchTarget::Album {
                release_group_mbid: group.to_owned(),
            },
            artist_name: artist,
            title: release.title.clone(),
            origin: DispatchOrigin::User,
            release_mbid: Some(release.id.clone()),
            wanted,
            planned_album: None,
            idempotency_key: Some(format!("bulk:{}:{group}", run.id)),
        };
        let outcome = match self.requests.dispatch.dispatch(&request).await {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(run_id = %run.id, group, %error, "bulk album download not sent");
                return Ok(None);
            }
        };
        let (task_id, ours) = match &outcome {
            crate::dispatch::DispatchOutcome::Dispatched { task_id } => (task_id.clone(), true),
            crate::dispatch::DispatchOutcome::Active { task_id } => (task_id.clone(), false),
        };
        if ours {
            self.stamp(&task_id, false).await;
        }
        let taken: Vec<i64> = positions.iter().map(|(id, _)| *id).collect();
        let (ids, key, task, now) = (
            taken.clone(),
            group.to_owned(),
            task_id.clone(),
            self.now_ms(),
        );
        let detail = format!("{} tracks from '{}'", taken.len(), release.title);
        self.db
            .write("bulk.sent_album", move |tx| {
                let mut statement = tx.prepare_cached(sql::MARK_SENT.text())?;
                for id in ids {
                    statement.execute(params![id, key, task, 1, detail, now])?;
                }
                Ok(())
            })
            .await?;
        tracing::info!(run_id = %run.id, group, task_id, songs = taken.len(), "bulk album download sent");
        Ok(Some(taken))
    }

    /// An edition of `group` holding two or more of `songs`, with each held
    /// song's (disc, position) on it; the edition holding the most wins.
    async fn edition_for(
        &self,
        group: &str,
        songs: &[Queued],
    ) -> Option<Edition> {
        let first = songs[0].recording_mbid.as_deref()?;
        let candidates = match self.lookup.recording_releases(first).await {
            Ok(Some(candidates)) => candidates,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(group, %error, "bulk album editions unavailable");
                return None;
            }
        };
        let in_group: Vec<_> = candidates
            .into_iter()
            .filter(|candidate| candidate.release_group_mbid.eq_ignore_ascii_case(group))
            .collect();
        let order = crate::target::choose::release_order(&in_group, &HashMap::new(), None);
        let mut best: Option<Edition> = None;
        for (release_mbid, _) in order.into_iter().take(EDITIONS_TRIED) {
            let release = match self.lookup.release(&release_mbid).await {
                Ok(Some(release)) => release,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(group, %error, "bulk album edition unavailable");
                    break;
                }
            };
            let held = positions_on(&release, songs);
            let better = best
                .as_ref()
                .is_none_or(|(_, known)| held.len() > known.len());
            if held.len() >= 2 && better {
                let all = held.len() == songs.len();
                best = Some((release, held));
                if all {
                    break;
                }
            }
        }
        best
    }

    /// Ask for one song on its own.
    async fn send_song(&self, owner: &Principal, song: &Queued) -> Result<Sent, BulkError> {
        let service = RequestsService::new(&self.requests);
        let album_title = Some(song.album_title.clone()).filter(|name| !name.is_empty());
        let duration_seconds = song.duration_ms.map(|ms| ms / 1000).filter(|s| *s > 0);
        let result = match song.recording_mbid.clone() {
            Some(recording_mbid) => service
                .request_track(
                    owner,
                    &TrackIntake {
                        recording_mbid: recording_mbid.clone(),
                        artist_name: song.artist_name.clone(),
                        track_title: song.track_title.clone(),
                        album_title,
                        duration_seconds,
                        release_group_mbid: song.release_group_mbid.clone(),
                        artist_mbid: None,
                        release_mbid: None,
                    },
                )
                .await
                .map(|response| (recording_mbid, response.task_id)),
            None => service
                .request_want(
                    owner,
                    &WantIntake {
                        artist_name: song.artist_name.clone(),
                        track_title: song.track_title.clone(),
                        album_title,
                        duration_seconds,
                        isrc: song.isrc.clone(),
                        spotify_track_id: song.spotify_track_id.clone(),
                        track_number: None,
                        disc_number: None,
                    },
                )
                .await
                .map(|response| (response.key, response.task_id)),
        };
        let now = self.now_ms();
        let id = song.id;
        match result {
            Ok((key, task_id)) => {
                if let Some(task_id) = task_id.as_deref() {
                    self.stamp(task_id, true).await;
                }
                self.db
                    .write("bulk.sent_song", move |tx| {
                        tx.execute(
                            sql::MARK_SENT.text(),
                            params![id, key, task_id, 0, Option::<String>::None, now],
                        )?;
                        Ok(())
                    })
                    .await?;
                Ok(Sent::Ok)
            }
            Err(RequestsError::QuotaExceeded { .. } | RequestsError::StorageFull { .. }) => {
                tracing::info!(
                    song_id = id,
                    "bulk request: quota or storage spent; waiting"
                );
                Ok(Sent::Stop)
            }
            Err(RequestsError::Busy { .. }) => Ok(Sent::Stop),
            Err(error) => {
                let detail = match &error {
                    RequestsError::InvalidInput { message }
                    | RequestsError::Conflict { message }
                    | RequestsError::Forbidden { message } => message.clone(),
                    other => format!("{other:?}"),
                };
                tracing::info!(song_id = id, %detail, "bulk request: song not asked for");
                self.db
                    .write("bulk.failed_song", move |tx| {
                        tx.execute(sql::MARK_FAILED.text(), params![id, detail, now])?;
                        Ok(())
                    })
                    .await?;
                Ok(Sent::Ok)
            }
        }
    }

    /// Mark task `task_id`'s plan as a bulk request's (no search review),
    /// and for a song asked for on its own as a playlist song.
    async fn stamp(&self, task_id: &str, playlist_song: bool) {
        let (id, now) = (task_id.to_owned(), self.db.now_unix_f64());
        let stamped = self
            .journal
            .run("bulk.stamp", move |store| {
                let mut plan = store.task_plan(&id)?;
                plan.bulk_request = true;
                plan.playlist_song |= playlist_song;
                store.set_task_plan(&id, &plan, now)
            })
            .await;
        if let Err(error) = stamped {
            tracing::warn!(task_id, %error, "bulk request plan not stamped");
        }
    }
}

/// Each of `songs` on `release`: its id and (disc, position).
fn positions_on(release: &AlbumRelease, songs: &[Queued]) -> Vec<(i64, (u32, u32))> {
    songs
        .iter()
        .filter_map(|song| {
            let recording = song.recording_mbid.as_deref()?;
            release
                .tracks
                .iter()
                .find(|track| track.recording_mbid.eq_ignore_ascii_case(recording))
                .map(|track| (song.id, (track.disc.max(1), track.position)))
        })
        .collect()
}

/// What `missing` would send: album groups of two or more songs with a
/// recording, the rest one by one.
fn estimate_of(missing: &[Missing]) -> Estimate {
    let mut groups: HashMap<&str, usize> = HashMap::new();
    for row in missing {
        if let (Some(group), Some(_)) = (row.5.as_deref(), row.6.as_deref()) {
            *groups.entry(group).or_default() += 1;
        }
    }
    let mut estimate = Estimate {
        missing: missing.len(),
        ..Estimate::default()
    };
    for count in groups.values().filter(|count| **count >= 2) {
        estimate.album_groups += 1;
        estimate.grouped_songs += count;
    }
    for row in missing {
        let grouped = row
            .5
            .as_deref()
            .is_some_and(|group| row.6.is_some() && groups.get(group).is_some_and(|n| *n >= 2));
        if grouped {
            continue;
        }
        if row.6.is_some() {
            estimate.by_recording += 1;
        } else {
            estimate.by_name += 1;
        }
    }
    estimate
}

/// What became of a song: its stored state, then its newest download.
fn song_state(
    stored: &str,
    detail: Option<String>,
    task_status: Option<&str>,
    outcome: Option<String>,
    held: bool,
    owned: bool,
) -> (SongState, Option<String>) {
    if owned {
        return (SongState::Done, None);
    }
    match stored {
        "queued" => return (SongState::Queued, None),
        "failed" => return (SongState::Failed, detail.or(Some("not_asked".to_owned()))),
        _ => {}
    }
    if held {
        return (SongState::Review, outcome.or(Some("held".to_owned())));
    }
    match task_status {
        Some("completed") => (SongState::Done, None),
        Some("partial") => (SongState::Review, Some("partial".to_owned())),
        Some("failed" | "cancelled") => (
            SongState::Failed,
            outcome.or_else(|| task_status.map(str::to_owned)),
        ),
        Some(_) => (SongState::InProgress, None),
        // Asked for, no download yet (awaiting approval, or not linked).
        None => (SongState::InProgress, None),
    }
}

/// Start the pacing loop.
pub fn spawn_bulk_requests(
    supervisor: &Supervisor,
    bulk: Arc<BulkRequests>,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        BULK_JOB,
        Schedule::new(TICK_EVERY)
            .with_initial_delay(Duration::from_secs(90))
            .with_jitter(Duration::from_secs(10)),
        FnTick(move |_| {
            let bulk = bulk.clone();
            async move {
                bulk.tick().await;
                Ok(Next::Continue)
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(group: Option<&str>, recording: Option<&str>) -> Missing {
        (
            "e".to_owned(),
            0,
            "t".to_owned(),
            "a".to_owned(),
            "b".to_owned(),
            group.map(str::to_owned),
            recording.map(str::to_owned),
            None,
            None,
            None,
        )
    }

    #[test]
    fn the_estimate_groups_two_or_more_songs_of_an_album() {
        let missing = vec![
            row(Some("g1"), Some("r1")),
            row(Some("g1"), Some("r2")),
            row(Some("g2"), Some("r3")),
            row(Some("g3"), None),
            row(None, None),
        ];
        assert_eq!(
            estimate_of(&missing),
            Estimate {
                missing: 5,
                album_groups: 1,
                grouped_songs: 2,
                by_recording: 1,
                by_name: 2,
            }
        );
    }

    #[test]
    fn a_held_song_is_in_review_and_a_library_copy_is_done() {
        assert_eq!(
            song_state(
                "sent",
                None,
                Some("failed"),
                Some("weak_match".into()),
                true,
                false
            ),
            (SongState::Review, Some("weak_match".to_owned()))
        );
        assert_eq!(
            song_state("sent", None, Some("failed"), None, false, true).0,
            SongState::Done
        );
        assert_eq!(
            song_state(
                "sent",
                None,
                Some("failed"),
                Some("no_source".into()),
                false,
                false
            ),
            (SongState::Failed, Some("no_source".to_owned()))
        );
        assert_eq!(
            song_state("queued", None, None, None, false, false).0,
            SongState::Queued
        );
    }
}
