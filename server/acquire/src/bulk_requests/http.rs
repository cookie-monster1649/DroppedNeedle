//! Fork: the bulk playlist requests' admin routes: estimate, start, list,
//! read one run with its songs, and pause, resume or cancel it.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use droppedneedle_auth::Admin;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{BulkError, BulkRequests, DEFAULT_PER_HOUR, Estimate, Run, RunState, RunStatus};

impl From<BulkError> for ApiError {
    fn from(error: BulkError) -> Self {
        match error {
            BulkError::NotFound => ApiError::not_found(),
            BulkError::Invalid(message) => ApiError::invalid_input(message),
            BulkError::Conflict(message) => ApiError::conflict(message),
            BulkError::Db(error) => ApiError::internal(error),
        }
    }
}

/// Start a bulk request of a playlist's missing songs.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BulkStartRequest {
    pub playlist_id: String,
    /// Songs sent per hour at most (default 60).
    #[serde(default)]
    pub per_hour: Option<i64>,
    /// Stop after this many songs for a look (default 0: no pilot).
    #[serde(default)]
    pub pilot: Option<i64>,
    /// Only estimate what would be sent; nothing is started.
    #[serde(default)]
    pub dry_run: bool,
}

/// The estimate, and the run when one started.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BulkStartResponse {
    pub estimate: Estimate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<RunStatus>,
}

/// Estimate, or start, a bulk request of a playlist's missing songs.
/// `POST /api/v3/admin/bulk-requests`.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/admin/bulk-requests",
    request_body = BulkStartRequest,
    responses((status = 200, body = BulkStartResponse))
)]
pub async fn bulk_start_handler(
    State(bulk): State<Arc<BulkRequests>>,
    _admin: Admin,
    ValidJson(body): ValidJson<BulkStartRequest>,
) -> Result<Json<BulkStartResponse>, ApiError> {
    let estimate = bulk.estimate(&body.playlist_id).await?;
    if body.dry_run {
        return Ok(Json(BulkStartResponse {
            estimate,
            status: None,
        }));
    }
    let status = bulk
        .start(
            &body.playlist_id,
            body.per_hour.unwrap_or(DEFAULT_PER_HOUR),
            body.pilot.unwrap_or(0),
        )
        .await?;
    Ok(Json(BulkStartResponse {
        estimate,
        status: Some(status),
    }))
}

/// The latest bulk requests, newest first.
/// `GET /api/v3/admin/bulk-requests`.
#[utoipa::path(
    get,
    tag = "requests",
    path = "/api/v3/admin/bulk-requests",
    responses((status = 200, body = Vec<Run>))
)]
pub async fn bulk_runs_handler(
    State(bulk): State<Arc<BulkRequests>>,
    _admin: Admin,
) -> Result<Json<Vec<Run>>, ApiError> {
    Ok(Json(bulk.runs().await?))
}

/// One bulk request with each song and what became of it.
/// `GET /api/v3/admin/bulk-requests/{run_id}`.
#[utoipa::path(
    get,
    tag = "requests",
    path = "/api/v3/admin/bulk-requests/{run_id}",
    params(("run_id" = String, Path, description = "Bulk request id")),
    responses((status = 200, body = RunStatus))
)]
pub async fn bulk_status_handler(
    State(bulk): State<Arc<BulkRequests>>,
    _admin: Admin,
    Path(run_id): Path<String>,
) -> Result<Json<RunStatus>, ApiError> {
    Ok(Json(bulk.status(&run_id).await?))
}

/// Change a bulk request's state.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BulkStateRequest {
    /// `running` (resume, also past a pilot), `paused` or `cancelled`.
    pub state: RunState,
}

/// Pause, resume or cancel a bulk request.
/// `POST /api/v3/admin/bulk-requests/{run_id}/state`.
#[utoipa::path(
    post,
    tag = "requests",
    path = "/api/v3/admin/bulk-requests/{run_id}/state",
    params(("run_id" = String, Path, description = "Bulk request id")),
    request_body = BulkStateRequest,
    responses((status = 200, body = RunStatus))
)]
pub async fn bulk_state_handler(
    State(bulk): State<Arc<BulkRequests>>,
    _admin: Admin,
    Path(run_id): Path<String>,
    ValidJson(body): ValidJson<BulkStateRequest>,
) -> Result<Json<RunStatus>, ApiError> {
    Ok(Json(bulk.set_state(&run_id, body.state).await?))
}

/// The bulk requests' routes, for admins.
pub fn routes() -> Routes<Arc<BulkRequests>> {
    Routes::new()
        .admin(routes!(bulk_start_handler, bulk_runs_handler))
        .admin(routes!(bulk_status_handler))
        .admin(routes!(bulk_state_handler))
}
