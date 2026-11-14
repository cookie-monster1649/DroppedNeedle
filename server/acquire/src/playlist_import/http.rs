//! The playlist-folder import's admin routes: start a dry run or the real
//! import, and read what it is doing.

use std::sync::Arc;

use axum::{Json, extract::State};
use droppedneedle_auth::Admin;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::mount::{Routes, routes};

use super::{Mode, PlaylistImport, Progress, StartError};

fn start(import: &Arc<PlaylistImport>, mode: Mode) -> Result<Json<Progress>, ApiError> {
    match import.start(mode) {
        Ok(()) => Ok(Json(import.progress())),
        Err(StartError::Off) => Err(ApiError::invalid_input(StartError::Off.to_string())),
        Err(StartError::Running) => Err(ApiError::conflict(StartError::Running.to_string())),
    }
}

/// Start a dry run: tags read, songs resolved, a report written; the
/// library is left as it is.
/// `POST /api/v3/admin/playlist-import/dry-run`.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/admin/playlist-import/dry-run",
    responses((status = 200, body = Progress))
)]
pub async fn playlist_import_dry_run_handler(
    State(import): State<Arc<PlaylistImport>>,
    _admin: Admin,
) -> Result<Json<Progress>, ApiError> {
    start(&import, Mode::DryRun)
}

/// Start the import itself; a run stopped early goes on where it was.
/// `POST /api/v3/admin/playlist-import/run`.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/admin/playlist-import/run",
    responses((status = 200, body = Progress))
)]
pub async fn playlist_import_run_handler(
    State(import): State<Arc<PlaylistImport>>,
    _admin: Admin,
) -> Result<Json<Progress>, ApiError> {
    start(&import, Mode::Import)
}

/// What the playlist import is doing, and how its last run ended.
/// `GET /api/v3/admin/playlist-import/status`.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/admin/playlist-import/status",
    responses((status = 200, body = Progress))
)]
pub async fn playlist_import_status_handler(
    State(import): State<Arc<PlaylistImport>>,
    _admin: Admin,
) -> Result<Json<Progress>, ApiError> {
    Ok(Json(import.progress()))
}

/// The playlist import's routes, for admins.
pub fn routes() -> Routes<Arc<PlaylistImport>> {
    Routes::new()
        .admin(routes!(playlist_import_dry_run_handler))
        .admin(routes!(playlist_import_run_handler))
        .admin(routes!(playlist_import_status_handler))
}
