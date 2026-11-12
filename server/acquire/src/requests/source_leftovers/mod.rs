//! Fork: the tracks a source album's download left behind, asked for one
//! by one.
//!
//! An album on stand-in ids (a Deezer album adopted by the library
//! sources) is requested as one download over its tracklist. When that
//! download settles partial or failed and nothing retries it, each
//! position it was asked for and no file holds yet is asked for by name,
//! at its place on the album, as the per-track Request does. A track
//! already asked for (any status) is not asked again, so a sweep repeats
//! harmlessly.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_auth::{Principal, Role};
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::{AlreadyRunning, FnTick, Next, Schedule, Supervisor};
use droppedneedle_library::provider_match::MatchTrack;
use rusqlite::{OptionalExtension, params};

use super::error::RequestsError;
use super::ledger::want_key;
use super::models::WantIntake;
use super::service::RequestsService;
use super::state::RequestsState;
use crate::downloads::plan::TaskPlan;
use sql::{HELD_POSITIONS, OWNER, SETTLED_SOURCE_TASKS, SOURCE_ALBUM, TRACK_ASKED};

pub(crate) mod sql;

/// The sweep's job name.
pub const LEFTOVER_JOB: &str = "source-album-leftovers";
/// How often the sweep runs.
pub const LEFTOVER_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// How long a settled task waits first, so an automatic retry wins.
pub const SETTLE_GRACE_SECS: i64 = 30 * 60;
/// How far back a settled task is still looked at.
const LOOKBACK_SECS: i64 = 7 * 24 * 60 * 60;

/// One leftover to ask for, and who asks.
#[derive(Debug, Clone)]
struct Leftover {
    user_id: String,
    intake: WantIntake,
}

/// Read the leftovers of tasks that settled by `now` (unix seconds).
fn leftovers(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<Vec<Leftover>> {
    let tasks: Vec<(String, String, Option<String>)> = conn
        .prepare_cached(SETTLED_SOURCE_TASKS.text())?
        .query_map(
            params![now - LOOKBACK_SECS, now - SETTLE_GRACE_SECS],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (_task, user_id, plan) in tasks {
        let Some(plan) = plan.as_deref().and_then(|json| TaskPlan::decode(json).ok()) else {
            continue;
        };
        let Some(album) = plan.planned_album.as_ref() else {
            continue;
        };
        let source: Option<(String, String, String)> = conn
            .prepare_cached(SOURCE_ALBUM.text())?
            .query_row(params![album.album_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;
        let Some((artist, title, tracks)) = source else {
            continue;
        };
        let tracks: Vec<MatchTrack> = serde_json::from_str(&tracks).unwrap_or_default();
        let held: HashSet<(i64, i64)> = conn
            .prepare_cached(HELD_POSITIONS.text())?
            .query_map(params![album.album_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let asked: HashSet<(i64, i64)> = plan
            .album_wanted
            .iter()
            .map(|position| (i64::from(position.disc), i64::from(position.track)))
            .collect();
        for track in tracks {
            let position = (track.disc.max(1), track.position);
            if held.contains(&position) || (!asked.is_empty() && !asked.contains(&position)) {
                continue;
            }
            let stored = format!("track:{}", want_key(&artist, &track.title));
            let already = conn
                .prepare_cached(TRACK_ASKED.text())?
                .query_row(params![stored], |_| Ok(()))
                .optional()?
                .is_some();
            if already || !seen.insert(stored) {
                continue;
            }
            out.push(Leftover {
                user_id: user_id.clone(),
                intake: WantIntake {
                    artist_name: artist.clone(),
                    track_title: track.title.clone(),
                    album_title: Some(title.clone()),
                    duration_seconds: track.duration_seconds.map(|seconds| seconds.round() as i64),
                    isrc: track.isrc.clone(),
                    spotify_track_id: None,
                    track_number: Some(track.position),
                    disc_number: Some(track.disc.max(1)),
                },
            });
        }
    }
    Ok(out)
}

/// Ask for every leftover of source-album tasks that settled by `now`
/// (unix seconds). Answers how many were asked for.
pub async fn sweep_once(requests: &RequestsState, now: i64) -> usize {
    let found: Result<Vec<Leftover>, DbError> = requests
        .library
        .db()
        .read("requests.source_leftovers", move |conn| {
            leftovers(conn, now).map_err(DbError::from)
        })
        .await;
    let found = match found {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, "source album leftovers unreadable");
            return 0;
        }
    };
    let service = RequestsService::new(requests);
    let mut asked = 0;
    for leftover in found {
        let Some(principal) = owner(requests, &leftover.user_id).await else {
            continue;
        };
        match service.request_want(&principal, &leftover.intake).await {
            Ok(_) => asked += 1,
            Err(RequestsError::QuotaExceeded { .. } | RequestsError::StorageFull { .. }) => {
                tracing::info!("source album leftovers: request quota reached; stopping");
                break;
            }
            Err(error) => {
                tracing::warn!(?error, track = %leftover.intake.track_title,
                    "source album leftover: request failed");
            }
        }
    }
    if asked > 0 {
        tracing::info!(asked, "source album leftovers asked for one by one");
    }
    asked
}

async fn owner(requests: &RequestsState, user_id: &str) -> Option<Principal> {
    let id = user_id.to_owned();
    let row: Result<Option<(String, Option<String>)>, DbError> = requests
        .library
        .db()
        .read("requests.source_leftovers.owner", move |conn| {
            conn.query_row(OWNER.text(), [id], |row| Ok((row.get(0)?, row.get(1)?)))
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
        _ => None,
    }
}

/// Start the sweep on `supervisor`.
pub fn spawn_source_leftovers(
    supervisor: &Supervisor,
    requests: RequestsState,
) -> Result<(), AlreadyRunning> {
    let requests = Arc::new(requests);
    supervisor.periodic(
        LEFTOVER_JOB,
        Schedule::new(LEFTOVER_INTERVAL)
            .with_initial_delay(Duration::from_secs(180))
            .with_jitter(Duration::from_secs(60)),
        FnTick(move |_| {
            let requests = requests.clone();
            async move {
                let now = crate::db::to_i64(requests.library.now_epoch());
                sweep_once(&requests, now).await;
                Ok(Next::Continue)
            }
        }),
    )
}
