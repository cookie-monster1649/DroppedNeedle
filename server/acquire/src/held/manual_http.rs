//! Fork: manual-import HTTP for a still-hunting song.
//!
//! Curators only (admin or trusted), the same trust as an upload: the
//! file goes into the shared library. A curator acts on their own
//! downloads, an admin on anyone's. See `held::service::manual`.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json,
    extract::{DefaultBodyLimit, Multipart, Path, State, multipart::MultipartRejection},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;
use utoipa::ToSchema;

use super::fetch;
use super::http::HeldActionResponse;
use super::service::{HeldService, KnownLink, ManualFetch, ManualFetchState};
use crate::downloads::queue_rows::Viewer;
use crate::requests::error::RequestsError;
use droppedneedle_auth::Curator;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};

/// Largest single file an upload onto a song takes.
const MAX_SONG_UPLOAD_BYTES: usize = 1 << 30;

fn viewer(curator: &Curator) -> Viewer {
    Viewer {
        user_id: curator.user_id().to_owned(),
        admin: curator.is_admin(),
    }
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::from(RequestsError::InvalidInput {
        message: message.into(),
    })
}

fn internal(error: &impl std::fmt::Display) -> ApiError {
    ApiError::from(RequestsError::internal(error))
}

/// The upload form: one `file` part, an audio file.
#[derive(Debug, ToSchema)]
pub struct ManualUploadForm {
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
}

/// A link to fetch the song from.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManualLinkRequest {
    /// A youtube.com, youtu.be, music.youtube.com or soundcloud.com page.
    pub url: String,
}

/// YouTube links the app already knows for the song.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManualLinksResponse {
    pub links: Vec<KnownLink>,
}

/// Import one uploaded audio file as a still-hunting song: it goes in at
/// the song's place, and the download is settled.
/// `POST /api/v3/downloads/{task_id}/manual-import/upload`.
#[utoipa::path(
    post,
    tag = "held",
    path = "/api/v3/downloads/{task_id}/manual-import/upload",
    params(("task_id" = String, Path, description = "Download task id")),
    request_body(content = ManualUploadForm, content_type = "multipart/form-data"),
    responses((status = 200, body = HeldActionResponse))
)]
pub async fn manual_upload_handler(
    State(service): State<Arc<HeldService>>,
    curator: Curator,
    Path(task_id): Path<String>,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<Json<HeldActionResponse>, ApiError> {
    let viewer = viewer(&curator);
    // Refused before anything is written for a song nobody may act on.
    service.manual_task(&viewer, &task_id).await?;
    let mut multipart = multipart.map_err(|_| invalid("Send the file as a multipart form."))?;
    let held_dir = service.folders().held.clone();
    tokio::fs::create_dir_all(&held_dir)
        .await
        .map_err(|error| internal(&error))?;
    let mut staged: Option<(String, PathBuf)> = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| invalid("The upload was cut off or malformed."))?
    {
        if field.name() != Some("file") || staged.is_some() {
            continue;
        }
        let name = field.file_name().unwrap_or_default().to_owned();
        if !crate::drop_import::extract::is_audio(std::path::Path::new(&name)) {
            return Err(invalid(
                "Pick one audio file (FLAC, MP3, M4A, Ogg, Opus or WAV).",
            ));
        }
        let path = held_dir.join(format!(
            "{}_{}",
            uuid::Uuid::new_v4().simple(),
            crate::drop_import::extract::safe_component(&name)
        ));
        let mut file = tokio::fs::File::create_new(&path)
            .await
            .map_err(|error| internal(&error))?;
        staged = Some((name, path.clone()));
        let written: Result<(), ApiError> = async {
            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(|_| invalid("The upload was cut off or too large (1 GB at most)."))?
            {
                file.write_all(&chunk)
                    .await
                    .map_err(|error| internal(&error))?;
            }
            file.sync_all().await.map_err(|error| internal(&error))
        }
        .await;
        if let Err(error) = written {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error);
        }
    }
    let Some((name, path)) = staged else {
        return Err(invalid("No file was uploaded."));
    };
    tracing::info!(%task_id, file = %name, "manual upload received for a still-hunting song");
    let acted = service
        .manual_import(&viewer, &task_id, &path, &name, "upload", curator.user_id())
        .await?;
    Ok(Json(acted.into()))
}

/// YouTube links the app already knows for a still-hunting song.
/// `GET /api/v3/downloads/{task_id}/manual-import/links`.
#[utoipa::path(
    get,
    tag = "held",
    path = "/api/v3/downloads/{task_id}/manual-import/links",
    params(("task_id" = String, Path, description = "Download task id")),
    responses((status = 200, body = ManualLinksResponse))
)]
pub async fn manual_links_handler(
    State(service): State<Arc<HeldService>>,
    curator: Curator,
    Path(task_id): Path<String>,
) -> Result<Json<ManualLinksResponse>, ApiError> {
    let links = service.manual_links(&viewer(&curator), &task_id).await?;
    Ok(Json(ManualLinksResponse { links }))
}

/// Fetch a still-hunting song's audio from a YouTube or SoundCloud link
/// and import it. Answers the fetch at once; follow it with
/// `GET /api/v3/downloads/manual-import/fetches/{fetch_id}`.
/// `POST /api/v3/downloads/{task_id}/manual-import/link`.
#[utoipa::path(
    post,
    tag = "held",
    path = "/api/v3/downloads/{task_id}/manual-import/link",
    params(("task_id" = String, Path, description = "Download task id")),
    request_body = ManualLinkRequest,
    responses((status = 202, body = ManualFetch))
)]
pub async fn manual_link_handler(
    State(service): State<Arc<HeldService>>,
    curator: Curator,
    Path(task_id): Path<String>,
    ValidJson(body): ValidJson<ManualLinkRequest>,
) -> Result<(StatusCode, Json<ManualFetch>), ApiError> {
    let viewer = viewer(&curator);
    let (url, site) = fetch::allowed_link(&body.url).map_err(invalid)?;
    service.manual_task(&viewer, &task_id).await?;
    let fetch = ManualFetch {
        id: uuid::Uuid::new_v4().simple().to_string(),
        task_id: task_id.clone(),
        state: ManualFetchState::Fetching,
        percent: Some(0.0),
        message: None,
        user_id: viewer.user_id.clone(),
    };
    service.manual_fetches().put(fetch.clone());
    tracing::info!(%task_id, host = url.host_str().unwrap_or_default(), "manual fetch started for a still-hunting song");
    let (id, actor) = (fetch.id.clone(), curator.user_id().to_owned());
    let worker = service.clone();
    tokio::spawn(async move {
        let outcome = fetch_and_import(&worker, &viewer, &task_id, &id, &url, site, &actor).await;
        worker.manual_fetches().update(&id, |fetch| match outcome {
            Ok(message) => {
                fetch.state = ManualFetchState::Done;
                fetch.percent = Some(100.0);
                fetch.message = Some(message);
            }
            Err(message) => {
                tracing::info!(task_id = %fetch.task_id, %message, "manual fetch did not import");
                fetch.state = ManualFetchState::Failed;
                fetch.message = Some(message);
            }
        });
    });
    Ok((StatusCode::ACCEPTED, Json(fetch)))
}

async fn fetch_and_import(
    service: &Arc<HeldService>,
    viewer: &Viewer,
    task_id: &str,
    id: &str,
    url: &reqwest::Url,
    site: fetch::LinkSite,
    actor: &str,
) -> Result<String, String> {
    let held_dir = service.folders().held.clone();
    let work = held_dir.join(format!("_fetch_{id}"));
    tokio::fs::create_dir_all(&work)
        .await
        .map_err(|_| "The download could not start on this server.".to_owned())?;
    let progress = {
        let service = service.clone();
        let id = id.to_owned();
        move |percent: f64| {
            service
                .manual_fetches()
                .update(&id, |fetch| fetch.percent = Some(percent.clamp(0.0, 100.0)));
        }
    };
    let fetched = fetch::fetch_audio(url, &work, progress).await;
    let staged = match fetched {
        Ok(file) => {
            let ext = file
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("audio")
                .to_owned();
            let staged = held_dir.join(format!(
                "{}_{}.{ext}",
                uuid::Uuid::new_v4().simple(),
                site.source()
            ));
            let moved = tokio::fs::rename(&file, &staged).await;
            let _ = tokio::fs::remove_dir_all(&work).await;
            moved.map_err(|_| "The download could not be kept on this server.".to_owned())?;
            staged
        }
        Err(message) => {
            let _ = tokio::fs::remove_dir_all(&work).await;
            return Err(message);
        }
    };
    service.manual_fetches().update(id, |fetch| {
        fetch.state = ManualFetchState::Importing;
        fetch.percent = Some(100.0);
    });
    let name = staged
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match service
        .manual_import(viewer, task_id, &staged, &name, site.source(), actor)
        .await
    {
        Ok(acted) if acted.imported > 0 => Ok("Imported into your library.".to_owned()),
        Ok(_) => Err("The file was kept in To decide.".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

/// How a fetch from a link is going.
/// `GET /api/v3/downloads/manual-import/fetches/{fetch_id}`.
#[utoipa::path(
    get,
    tag = "held",
    path = "/api/v3/downloads/manual-import/fetches/{fetch_id}",
    params(("fetch_id" = String, Path, description = "Fetch id")),
    responses((status = 200, body = ManualFetch))
)]
pub async fn manual_fetch_handler(
    State(service): State<Arc<HeldService>>,
    curator: Curator,
    Path(fetch_id): Path<String>,
) -> Result<Json<ManualFetch>, ApiError> {
    service
        .manual_fetches()
        .get(&viewer(&curator), &fetch_id)
        .map(Json)
        .ok_or_else(|| ApiError::from(RequestsError::NotFound))
}

/// The manual-import routes, for curators. The upload takes one file up to
/// `MAX_SONG_UPLOAD_BYTES`.
pub fn add_routes(routes: Routes<Arc<HeldService>>) -> Routes<Arc<HeldService>> {
    let (schemas, paths, upload) = routes!(manual_upload_handler);
    let upload = upload.layer(DefaultBodyLimit::max(MAX_SONG_UPLOAD_BYTES));
    routes
        .curator((schemas, paths, upload))
        .curator(routes!(manual_links_handler))
        .curator(routes!(manual_link_handler))
        .curator(routes!(manual_fetch_handler))
}
