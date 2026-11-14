//! Native `/api/v3` import + health handlers and the router constructor.
//!
//! Handlers are thin: extract the caller, call one service, render the
//! shape. Status mapping lives in [`ImportsError`](super::error::ImportsError)'s
//! one mapping onto `ApiError`; query strings parse through [`ValidQuery`] and
//! bodies through [`ValidJson`] so malformed input stays inside the shared
//! envelope. [`routes`] mounts every handler in the group of its caller
//! extractor, so those routes 401 anonymously, and the Spotify OAuth
//! callback in the public group (it is identified by its state token). It
//! exchanges the code with the redirect URI stored next to the state at
//! authorize time.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use axum::{
    Json,
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};
use droppedneedle_auth::session::middleware::effective_origin;
use droppedneedle_auth::{Admin, User};
use droppedneedle_kernel::config::TrustedProxies;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::jobs::queue::JobError;
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Deserialize;
use utoipa::IntoParams;

use super::error::ImportsError;
use super::health::HealthProbes;
use super::jobs::{ImportState, SpotifyImports};
use super::lidarr::{
    LIDARR_NOT_CONNECTED, LidarrClient, LidarrError, LidarrImportService, ServiceError,
    normalize_lidarr_url,
};
use super::models::{
    LidarrArtistListResponse, LidarrConnectionSettings, LidarrImportRequest, LidarrImportResponse,
    LidarrTestResponse, SabnzbdStatusResponse, SlskdStatusResponse, SpotifyAuthUrlResponse,
    SpotifyImportRequest, SpotifyImportResponse, SpotifyImportStatus, SpotifyPlaylistListQuery,
    SpotifyPlaylistListResponse, SpotifyRedirectUri, SpotifySettings, SpotifySyncState,
    SpotifySyncUpdate,
};
use super::models::{
    SpotifyLibraryState, SpotifyLibraryUpdate, SpotifyResolution, SpotifyResolveQuery,
    SpotifySavedAlbumList,
};
use super::spotify::{ImportFailure, SpotifyImportService};
use super::spotify_albums::AlbumResolver;
use super::spotify_client::{SpotifyClient, SpotifyError, now_unix_secs};
use super::spotify_library::{LibraryError, SpotifyLibrary};
use super::spotify_oauth::{PendingAuth, SPOTIFY_CALLBACK_PATH, SpotifyConnection, redirect_uri};
use super::spotify_store::{CollectionsPlaylistBridge, SqliteSpotifyLinks, SqliteSpotifyStates};
use super::spotify_sync::{SpotifySyncs, SyncSettingError};
use crate::follows::Follows;
use crate::requests::error::RequestsError;
use crate::settings::{ConfigLidarrSettings, ConfigSpotifySettings, settings_failed};

/// Every dependency the imports routes need, injected by constructor.
#[derive(Clone)]
pub struct ImportsDeps {
    /// Read-only Lidarr client (the two sanctioned GETs).
    pub lidarr: LidarrClient,
    /// Lidarr connection settings.
    pub lidarr_settings: Arc<ConfigLidarrSettings>,
    /// The follows the import writes.
    pub follows: Follows,
    /// Spotify API client over injected bases.
    pub spotify: SpotifyClient,
    /// Spotify app settings.
    pub spotify_settings: Arc<ConfigSpotifySettings>,
    /// Single-use OAuth states.
    pub spotify_states: Arc<SqliteSpotifyStates>,
    /// Per-user Spotify links.
    pub spotify_links: Arc<SqliteSpotifyLinks>,
    /// Imported playlists (`spotify:{id}` refs) and their rows.
    pub spotify_playlists: Arc<CollectionsPlaylistBridge>,
    /// Finds each imported album's MusicBrainz release group.
    pub resolver: Arc<AlbumResolver>,
    /// Queues Spotify imports on the jobs table and reads their state.
    pub spotify_imports: SpotifyImports,
    /// Fork: which imported playlists are kept in step with Spotify.
    pub spotify_syncs: SpotifySyncs,
    /// Fork: followed artists and saved albums kept in step, and pasted
    /// Spotify links resolved.
    pub spotify_library: Arc<SpotifyLibrary>,
    /// Probes behind the download client status reads.
    pub probes: Arc<HealthProbes>,
    /// Fresh ids, the OAuth state fallback when the OS RNG fails.
    pub ids: Arc<dyn IdGenerator>,
    /// Deployment base path (between origin and callback, exactly once).
    pub base_path: String,
    /// Reverse proxies whose forwarded headers name this server's address.
    pub trusted_proxies: TrustedProxies,
}

impl ImportsDeps {
    /// Lidarr import service over the wired stores.
    fn lidarr_service(&self) -> LidarrImportService {
        LidarrImportService::new(
            self.lidarr.clone(),
            self.lidarr_settings.clone(),
            self.follows.clone(),
        )
    }

    /// Spotify import service over the wired stores.
    fn spotify_service(&self) -> SpotifyImportService {
        SpotifyImportService::new(
            self.spotify.clone(),
            self.spotify_settings.clone(),
            self.spotify_links.clone(),
            self.spotify_playlists.clone(),
            self.resolver.clone(),
        )
    }
}

/// The imports, health and Spotify routes, in the group of each handler's
/// caller extractor. The Spotify OAuth callback is public: it is
/// state-token identified.
pub fn routes() -> Routes<ImportsDeps> {
    Routes::new()
        .admin(routes!(get_lidarr_config, put_lidarr_config))
        .admin(routes!(test_lidarr))
        .admin(routes!(list_lidarr_artists))
        .admin(routes!(import_lidarr))
        .admin(routes!(get_spotify_settings, put_spotify_settings))
        .admin(routes!(get_spotify_redirect_uri))
        .user(routes!(get_spotify_auth_url))
        .public(routes!(spotify_callback))
        .user(routes!(list_spotify_playlists))
        .user(routes!(import_spotify_playlist))
        .user(routes!(get_spotify_import_status))
        .user(routes!(get_spotify_sync, put_spotify_sync))
        .user(routes!(get_playlist_rows))
        .user(routes!(disconnect_spotify))
        // Fork: the Spotify library and pasted links.
        .user(routes!(get_spotify_library, put_spotify_library))
        .user(routes!(sync_spotify_artists))
        .user(routes!(list_spotify_saved_albums))
        .user(routes!(scan_spotify_saved_albums))
        .user(routes!(review_spotify_saved_albums))
        .user(routes!(resolve_spotify_link))
        .user(routes!(get_slskd_status))
        .admin(routes!(get_sabnzbd_status))
}

/// Map a Lidarr service failure to the wire. Upstream detail reaches the
/// log only.
fn lidarr_failed(error: ServiceError) -> ImportsError {
    match error {
        ServiceError::Settings(error) => settings_failed(error),
        ServiceError::NotConnected => ImportsError::NotConfigured {
            message: LIDARR_NOT_CONNECTED.to_owned(),
        },
        ServiceError::Lidarr(LidarrError::Auth) => ImportsError::AuthFailed {
            cause: "Lidarr rejected the stored API key".to_owned(),
        },
        ServiceError::Lidarr(LidarrError::Unavailable(detail)) => ImportsError::Unavailable {
            cause: format!("lidarr import failed: {detail}"),
        },
        ServiceError::Follows(RequestsError::Busy { operation }) => ImportsError::Busy {
            cause: format!("lidarr import follows: {operation}"),
        },
        ServiceError::Follows(cause) => ImportsError::internal(&format_args!(
            "lidarr import follow write failed: {cause:?}"
        )),
    }
}

/// Masked Lidarr connection settings.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/lidarr-import/config",
    responses((status = 200, description = "Masked Lidarr settings", body = LidarrConnectionSettings)))]
pub async fn get_lidarr_config(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
) -> Result<Json<LidarrConnectionSettings>, ApiError> {
    let settings = deps.lidarr_settings.get().map_err(settings_failed)?;
    Ok(Json(settings))
}

/// Save the Lidarr connection. A masked key preserves the stored one.
#[utoipa::path(put, tag = "imports", path = "/api/v3/acquire/lidarr-import/config",
    request_body = LidarrConnectionSettings,
    responses((status = 200, description = "Saved Lidarr settings", body = LidarrConnectionSettings)))]
pub async fn put_lidarr_config(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
    ValidJson(body): ValidJson<LidarrConnectionSettings>,
) -> Result<Json<LidarrConnectionSettings>, ApiError> {
    deps.lidarr_settings.save(&body).await?;
    let settings = deps.lidarr_settings.get().map_err(settings_failed)?;
    Ok(Json(settings))
}

/// Test submitted Lidarr credentials against `system/status`: the
/// submitted url/key are probed so Test works before the first
/// save, a masked key resolves to the stored one, and reachable/bad-key
/// verdicts travel in the body, never as a leaked 5xx, never echoing the
/// URL or host.
#[utoipa::path(post, tag = "imports", path = "/api/v3/acquire/lidarr-import/test",
    request_body = LidarrConnectionSettings,
    responses((status = 200, description = "Lidarr probe verdict", body = LidarrTestResponse)))]
pub async fn test_lidarr(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
    ValidJson(body): ValidJson<LidarrConnectionSettings>,
) -> Result<Json<LidarrTestResponse>, ApiError> {
    let mut api_key = body.api_key;
    if api_key == droppedneedle_kernel::runtime_config::SECRET_MASK {
        let stored = deps.lidarr_settings.get_raw().map_err(settings_failed)?;
        api_key = stored.api_key.expose().to_owned();
    }
    let url = normalize_lidarr_url(&body.url);
    let verdict = match deps.lidarr.system_status(&url, &api_key).await {
        Err(LidarrError::Auth) => Json(LidarrTestResponse {
            valid: false,
            version: None,
            message: "Lidarr rejected the API key. Check Settings -> General -> Security -> API Key in Lidarr.".to_owned(),
        }),
        Err(LidarrError::Unavailable(detail)) => {
            tracing::warn!(%detail, "lidarr test probe failed");
            Json(LidarrTestResponse {
                valid: false,
                version: None,
                message: "Couldn't reach Lidarr. Check the URL and that Lidarr is running.".to_owned(),
            })
        }
        Ok(status) => Json(LidarrTestResponse {
            valid: true,
            version: if status.version.is_empty() { None } else { Some(status.version.clone()) },
            message: if status.version.is_empty() {
                "Connected".to_owned()
            } else {
                format!("Connected - Lidarr v{}", status.version)
            },
        }),
    };
    Ok(verdict)
}

/// Monitored Lidarr artists annotated for the caller.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/lidarr-import/artists",
    responses((status = 200, description = "Import candidates", body = LidarrArtistListResponse)))]
pub async fn list_lidarr_artists(
    State(deps): State<ImportsDeps>,
    admin: Admin,
) -> Result<Json<LidarrArtistListResponse>, ApiError> {
    let artists = deps
        .lidarr_service()
        .list_candidates(admin.user_id())
        .await
        .map_err(lidarr_failed)?;
    Ok(Json(artists))
}

/// Import selected Lidarr artists into the caller's follows.
#[utoipa::path(post, tag = "imports", path = "/api/v3/acquire/lidarr-import/import",
    request_body = LidarrImportRequest,
    responses((status = 200, description = "Import summary", body = LidarrImportResponse)))]
pub async fn import_lidarr(
    State(deps): State<ImportsDeps>,
    admin: Admin,
    ValidJson(body): ValidJson<LidarrImportRequest>,
) -> Result<Json<LidarrImportResponse>, ApiError> {
    let summary = deps
        .lidarr_service()
        .import_artists(admin.user_id(), &body.selected_mbids)
        .await
        .map_err(lidarr_failed)?;
    Ok(Json(summary))
}

/// Masked Spotify app settings.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/settings",
    responses((status = 200, description = "Masked Spotify settings", body = SpotifySettings)))]
pub async fn get_spotify_settings(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
) -> Result<Json<SpotifySettings>, ApiError> {
    let settings = deps.spotify_settings.get().map_err(settings_failed)?;
    Ok(Json(settings))
}

/// Save the Spotify app settings. A masked secret preserves the stored
/// one; a non-absolute redirect origin is a 400.
#[utoipa::path(put, tag = "imports", path = "/api/v3/acquire/spotify/settings",
    request_body = SpotifySettings,
    responses((status = 200, description = "Saved Spotify settings", body = SpotifySettings)))]
pub async fn put_spotify_settings(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
    ValidJson(body): ValidJson<SpotifySettings>,
) -> Result<Json<SpotifySettings>, ApiError> {
    deps.spotify_settings.save(&body).await?;
    let settings = deps.spotify_settings.get().map_err(settings_failed)?;
    Ok(Json(settings))
}

/// The computed OAuth redirect URI, for the admin to register.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/redirect-uri",
    responses(
        (status = 200, description = "OAuth redirect URI", body = SpotifyRedirectUri),
        (status = 400, description = "No Redirect Origin is set and the request names no address to build one from"),
    ))]
pub async fn get_spotify_redirect_uri(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
    request: Request,
) -> Result<Json<SpotifyRedirectUri>, ApiError> {
    Ok(Json(SpotifyRedirectUri {
        redirect_uri: callback_uri(&deps, &request)?,
    }))
}

/// What a caller reads when the callback address cannot be built.
const NO_CALLBACK_ADDRESS: &str = "DroppedNeedle could not work out its own address for the \
     Spotify link. An admin needs to fill in Redirect Origin under Settings > Spotify.";

/// The redirect URI this server sends Spotify. Without a configured origin
/// it is built on the address the session origin check works out for this
/// request, so forwarded headers count only from a trusted proxy. A request
/// that names no address is refused instead of guessed at.
fn callback_uri(deps: &ImportsDeps, request: &Request) -> Result<String, ImportsError> {
    let configured = deps
        .spotify_settings
        .get_raw()
        .map_err(settings_failed)?
        .spotify_redirect_origin;
    let base = if configured.trim().is_empty() {
        effective_origin(request, &deps.trusted_proxies)
            .map(|origin| origin.serialized())
            .ok_or_else(|| ImportsError::InvalidInput {
                message: NO_CALLBACK_ADDRESS.to_owned(),
            })?
    } else {
        String::new()
    };
    Ok(redirect_uri(
        SPOTIFY_CALLBACK_PATH,
        &configured,
        &base,
        &deps.base_path,
    ))
}

/// Authorize URL for the caller's Spotify link flow. Requires the admin
/// app to be enabled and complete.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/auth/url",
    responses(
        (status = 200, description = "Authorize URL", body = SpotifyAuthUrlResponse),
        (status = 400, description = "Spotify is not set up, or no Redirect Origin is set and the request names no address to build one from"),
    ))]
pub async fn get_spotify_auth_url(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    request: Request,
) -> Result<Json<SpotifyAuthUrlResponse>, ApiError> {
    let uri = callback_uri(&deps, &request)?;
    let state = fresh_state(&deps);
    let response = deps.spotify_service().auth_url(&uri, &state)?;
    let pending = PendingAuth {
        user_id: principal.user_id().to_owned(),
        redirect_uri: uri,
    };
    deps.spotify_states
        .store_state(&state, &pending)
        .await
        .map_err(|cause| ImportsError::internal(&cause))?;
    Ok(Json(response))
}

/// OAuth callback query (`code`, `state`, `error`, all optional).
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SpotifyCallbackQuery {
    /// Authorization code from Spotify.
    pub code: Option<String>,
    /// Round-tripped state token.
    pub state: Option<String>,
    /// Provider-side error, when the user declined.
    pub error: Option<String>,
}

/// Spotify OAuth callback. State-identified, so it sits outside the
/// session gate: every outcome redirects to the profile page with
/// `spotify=connected` or `spotify=error` with an optional `reason`.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/auth/callback",
    params(SpotifyCallbackQuery),
    responses((status = 307, description = "Redirect to the profile page")))]
pub async fn spotify_callback(
    State(deps): State<ImportsDeps>,
    ValidQuery(query): ValidQuery<SpotifyCallbackQuery>,
) -> Response {
    finish_spotify_link(&deps, query).await
}

/// Consume the state, exchange the code with the redirect URI stored at
/// authorize time (Spotify rejects any other), read the Spotify profile
/// and store the link.
async fn finish_spotify_link(deps: &ImportsDeps, query: SpotifyCallbackQuery) -> Response {
    let profile = |suffix: &str| format!("{}{suffix}", deps.base_path);
    if query.error.is_some() || query.code.is_none() || query.state.is_none() {
        return Redirect::temporary(&profile("/profile?spotify=error")).into_response();
    }
    let pending = match deps
        .spotify_states
        .consume_state(query.state.as_deref().unwrap_or_default())
        .await
    {
        Ok(Some(pending)) => pending,
        Ok(None) => {
            return Redirect::temporary(&profile("/profile?spotify=error&reason=state"))
                .into_response();
        }
        Err(cause) => {
            tracing::error!(%cause, "spotify state lookup failed");
            return Redirect::temporary(&profile("/profile?spotify=error")).into_response();
        }
    };
    // The settings read logs a section that does not read.
    let Ok(raw) = deps.spotify_settings.get_raw() else {
        return Redirect::temporary(&profile("/profile?spotify=error")).into_response();
    };
    let grant = match deps
        .spotify
        .exchange_code(
            &raw.client_id,
            raw.client_secret.expose(),
            query.code.as_deref().unwrap_or_default(),
            &pending.redirect_uri,
        )
        .await
    {
        Ok(grant) => grant,
        Err(cause) => {
            tracing::warn!(?cause, "spotify token exchange failed");
            return Redirect::temporary(&profile("/profile?spotify=error&reason=token"))
                .into_response();
        }
    };
    let me = match deps.spotify.profile(&grant.access_token).await {
        Ok(me) => me,
        Err(cause) => {
            tracing::warn!(?cause, "spotify callback /me failed");
            return Redirect::temporary(&profile("/profile?spotify=error&reason=network"))
                .into_response();
        }
    };
    let (username, spotify_user_id) = match me {
        Some(me) => (me.display_name.unwrap_or_else(|| me.id.clone()), me.id),
        None => ("Spotify".to_owned(), String::new()),
    };
    let link = SpotifyConnection {
        access_token: grant.access_token,
        refresh_token: grant.refresh_token.unwrap_or_default(),
        expires_at_unix: now_unix_secs(deps.spotify_links.clock().as_ref()) + grant.expires_in_secs,
        username,
        spotify_user_id,
        scope: grant.scope.unwrap_or_default(),
    };
    if let Err(cause) = deps.spotify_links.upsert(&pending.user_id, &link).await {
        tracing::error!(%cause, "spotify link write failed");
        return Redirect::temporary(&profile("/profile?spotify=error")).into_response();
    }
    Redirect::temporary(&profile("/profile?spotify=connected")).into_response()
}

/// Unlink the caller's Spotify account.
/// Idempotent: no link reads as done too.
#[utoipa::path(delete, tag = "imports", path = "/api/v3/me/connections/spotify",
    responses(
        (status = 204, description = "Unlinked"),
        (status = 401, description = "Not authenticated"),
    ))]
pub async fn disconnect_spotify(
    State(deps): State<ImportsDeps>,
    User(principal): User,
) -> Result<StatusCode, ApiError> {
    let removed = deps
        .spotify_links
        .remove(principal.user_id())
        .await
        .map_err(|cause| ImportsError::internal(&cause))?;
    if removed {
        tracing::info!(user_id = %principal.user_id(), "spotify link removed");
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The caller's owned Spotify playlists. Unlinked reads as a 400,
/// upstream failures as a 502. Fork: served from the last listing; Spotify
/// is read only with `refresh=true` (or when nothing was listed yet).
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/playlists",
    params(SpotifyPlaylistListQuery),
    responses(
        (status = 200, description = "Owned playlists", body = SpotifyPlaylistListResponse),
        (status = 503, description = "Spotify is rate limiting this server")
    ))]
pub async fn list_spotify_playlists(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    ValidQuery(query): ValidQuery<SpotifyPlaylistListQuery>,
) -> Result<Json<SpotifyPlaylistListResponse>, ApiError> {
    let playlists = deps
        .spotify_service()
        .list_playlists(principal.user_id(), query.refresh)
        .await
        .map_err(|error| match error {
            ImportFailure::Settings(error) => settings_failed(error),
            ImportFailure::Spotify(error) => spotify_failed(error),
        })?;
    Ok(Json(playlists))
}

/// Map a Spotify service failure to the wire (400 not-linked, 502
/// upstream); store failures are server faults.
fn spotify_failed(error: SpotifyError) -> ImportsError {
    match error {
        SpotifyError::NotLinked => ImportsError::NotConfigured {
            message: "Spotify account not linked".to_owned(),
        },
        SpotifyError::Unavailable(detail) => ImportsError::Unavailable {
            cause: format!("spotify playlist list failed: {detail}"),
        },
        SpotifyError::RateLimited { retry_after, .. } => ImportsError::SpotifyPaced { retry_after },
        SpotifyError::Store(cause) => ImportsError::internal(&cause),
        SpotifyError::Busy => ImportsError::Busy {
            cause: "spotify store busy".to_owned(),
        },
    }
}

/// Map a jobs table failure to the wire, keeping busy a 503.
fn job_failed(error: JobError) -> ImportsError {
    if error.is_busy() {
        ImportsError::Busy {
            cause: error.to_string(),
        }
    } else {
        ImportsError::internal(&error)
    }
}

/// Longest Spotify playlist id an import accepts. Spotify's are 22
/// characters; the bound keeps the job key within what the jobs table
/// holds.
const MAX_SPOTIFY_ID: usize = 64;

/// Start a Spotify playlist import: ensure the internal record, queue the
/// import job unless one for this playlist is already queued or running,
/// and answer the id at once.
#[utoipa::path(post, tag = "imports", path = "/api/v3/acquire/spotify/playlists/{id}/import",
    params(("id" = String, Path, description = "Spotify playlist id")),
    request_body = SpotifyImportRequest,
    responses((status = 200, description = "Import acknowledgement", body = SpotifyImportResponse)))]
pub async fn import_spotify_playlist(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<SpotifyImportRequest>,
) -> Result<Json<SpotifyImportResponse>, ApiError> {
    if id.trim().is_empty() || id.chars().count() > MAX_SPOTIFY_ID {
        return Err(ImportsError::InvalidInput {
            message: "Spotify playlist id is not valid".to_owned(),
        }
        .into());
    }
    // The import needs the Spotify app settings: while they do not read it
    // is refused here rather than queued.
    deps.spotify_settings.get_raw().map_err(settings_failed)?;
    let playlist_id = deps
        .spotify_service()
        .ensure_playlist_record(principal.user_id(), &id, &body.name)
        .await
        .map_err(spotify_failed)?;
    deps.spotify_imports
        .start(principal.user_id(), &id, &playlist_id)
        .await
        .map_err(job_failed)?;
    Ok(Json(SpotifyImportResponse { playlist_id }))
}

/// Fork: where the newest Spotify import into one of the caller's
/// playlists stands: the playlist page shows a loading state while it runs
/// and the error, with a retry, when it failed.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/imports/{playlist_id}",
    params(("playlist_id" = String, Path, description = "Internal playlist id")),
    responses(
        (status = 200, description = "Import status", body = SpotifyImportStatus),
        (status = 404, description = "Not the caller's playlist"),
    ))]
pub async fn get_spotify_import_status(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    Path(playlist_id): Path<String>,
) -> Result<Json<SpotifyImportStatus>, ApiError> {
    let Some(spotify_id) = deps
        .spotify_syncs
        .spotify_id(principal.user_id(), &playlist_id)
        .await
        .map_err(sync_failed)?
    else {
        return Ok(Json(SpotifyImportStatus {
            state: "none".to_owned(),
            message: None,
        }));
    };
    let state = deps
        .spotify_imports
        .state(principal.user_id(), &spotify_id)
        .await
        .map_err(|error| sync_failed(SyncSettingError::Store(error)))?;
    let (state, message) = match state {
        None => ("none", None),
        Some(ImportState::Running) => ("running", None),
        Some(ImportState::Done) => ("done", None),
        Some(ImportState::Error { message }) => ("failed", Some(message)),
    };
    Ok(Json(SpotifyImportStatus {
        state: state.to_owned(),
        message,
    }))
}

/// Map a sync setting failure to the wire.
fn sync_failed(error: SyncSettingError) -> ImportsError {
    match error {
        SyncSettingError::NotFound => ImportsError::NotFound,
        SyncSettingError::NotSpotify => ImportsError::Conflict {
            message: "Only playlists imported from Spotify can sync with it".to_owned(),
        },
        SyncSettingError::Store(
            error @ (droppedneedle_kernel::db::DbError::Busy { .. }
            | droppedneedle_kernel::db::DbError::LaneClosed),
        ) => ImportsError::Busy {
            cause: error.to_string(),
        },
        SyncSettingError::Store(error) => ImportsError::internal(&error),
    }
}

/// Fork: whether one of the caller's playlists imported from Spotify is
/// kept in step with Spotify.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/synced/{playlist_id}",
    params(("playlist_id" = String, Path, description = "Internal playlist id")),
    responses(
        (status = 200, description = "Sync setting", body = SpotifySyncState),
        (status = 404, description = "Not the caller's playlist"),
    ))]
pub async fn get_spotify_sync(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    Path(playlist_id): Path<String>,
) -> Result<Json<SpotifySyncState>, ApiError> {
    let setting = deps
        .spotify_syncs
        .get(principal.user_id(), &playlist_id)
        .await
        .map_err(sync_failed)?;
    Ok(Json(sync_state(setting)))
}

/// Fork: turn sync with Spotify on or off for one of the caller's
/// playlists imported from Spotify.
#[utoipa::path(put, tag = "imports", path = "/api/v3/acquire/spotify/synced/{playlist_id}",
    params(("playlist_id" = String, Path, description = "Internal playlist id")),
    request_body = SpotifySyncUpdate,
    responses(
        (status = 200, description = "Sync setting", body = SpotifySyncState),
        (status = 404, description = "Not the caller's playlist"),
        (status = 409, description = "Sync asked for on a playlist not from Spotify"),
    ))]
pub async fn put_spotify_sync(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<SpotifySyncUpdate>,
) -> Result<Json<SpotifySyncState>, ApiError> {
    deps.spotify_syncs
        .set(
            principal.user_id(),
            &playlist_id,
            body.enabled,
            body.auto_request,
        )
        .await
        .map_err(sync_failed)?;
    let setting = deps
        .spotify_syncs
        .get(principal.user_id(), &playlist_id)
        .await
        .map_err(sync_failed)?;
    Ok(Json(sync_state(setting)))
}

fn sync_state(setting: Option<super::spotify_sync::SyncSetting>) -> SpotifySyncState {
    match setting {
        Some(setting) => SpotifySyncState {
            enabled: true,
            auto_request: setting.auto_request,
            checked_at: setting.checked_at,
            last_error: setting.last_error,
        },
        None => SpotifySyncState {
            enabled: false,
            auto_request: false,
            checked_at: None,
            last_error: None,
        },
    }
}

/// Live slskd client status (any signed-in user may read it).
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/slskd/status",
    responses((status = 200, description = "slskd status", body = SlskdStatusResponse)))]
pub async fn get_slskd_status(
    State(deps): State<ImportsDeps>,
    _user: User,
) -> Json<SlskdStatusResponse> {
    Json(super::health::slskd_status(deps.probes.slskd.as_ref()))
}

/// Live SABnzbd status against the saved config (admin only).
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/sabnzbd/status",
    responses((status = 200, description = "SABnzbd status", body = SabnzbdStatusResponse)))]
pub async fn get_sabnzbd_status(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
) -> Json<SabnzbdStatusResponse> {
    Json(super::health::sabnzbd_status(deps.probes.sabnzbd.as_ref()))
}

/// Fresh OAuth state token: 32 random bytes, base64url, unpadded.
/// Falls back to a request id when the OS
/// RNG is unavailable, so auth never hard-fails on entropy.
fn fresh_state(deps: &ImportsDeps) -> String {
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        return deps.ids.new_id();
    }
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Fork: where each entry of one of the caller's playlists stands (in the
/// library, missing, requested, fetching, held, failed), for the playlist
/// page's per-row status.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/playlists/{playlist_id}/rows",
    params(("playlist_id" = String, Path, description = "Internal playlist id")),
    responses(
        (status = 200, description = "Entry states", body = super::playlist_rows::PlaylistRowsResponse),
        (status = 404, description = "Not the caller's playlist"),
    ))]
pub async fn get_playlist_rows(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    Path(playlist_id): Path<String>,
) -> Result<Json<super::playlist_rows::PlaylistRowsResponse>, ApiError> {
    let rows = super::playlist_rows::playlist_rows(
        deps.spotify_syncs.db(),
        principal.user_id(),
        &playlist_id,
    )
    .await
    .map_err(|error| match error {
        error @ (droppedneedle_kernel::db::DbError::Busy { .. }
        | droppedneedle_kernel::db::DbError::LaneClosed) => ImportsError::Busy {
            cause: error.to_string(),
        },
        error => ImportsError::internal(&error),
    })?
    .ok_or(ImportsError::NotFound)?;
    Ok(Json(super::playlist_rows::PlaylistRowsResponse { rows }))
}

/// Fork: map a library failure to the wire.
fn library_failed(error: LibraryError) -> ImportsError {
    match error {
        LibraryError::Import(ImportFailure::Settings(error)) => settings_failed(error),
        LibraryError::Import(ImportFailure::Spotify(error)) => spotify_failed(error),
        LibraryError::NeedsReconnect => ImportsError::Conflict {
            message: LibraryError::NeedsReconnect.message(),
        },
        LibraryError::Resolve(error) => ImportsError::Unavailable {
            cause: format!("spotify link resolution failed: {error}"),
        },
        LibraryError::Store(cause) => ImportsError::internal(&cause),
    }
}

/// Fork: the caller's Spotify library sync: toggles, Spotify's counts,
/// and whether the link needs reconnecting for library access.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/library",
    responses(
        (status = 200, description = "Library sync state", body = SpotifyLibraryState),
        (status = 400, description = "Spotify not linked"),
    ))]
pub async fn get_spotify_library(
    State(deps): State<ImportsDeps>,
    User(principal): User,
) -> Result<Json<SpotifyLibraryState>, ApiError> {
    let state = deps
        .spotify_library
        .state(principal.user_id())
        .await
        .map_err(library_failed)?;
    Ok(Json(state))
}

/// Fork: turn the followed-artist and saved-album syncs on or off.
#[utoipa::path(put, tag = "imports", path = "/api/v3/acquire/spotify/library",
    request_body = SpotifyLibraryUpdate,
    responses(
        (status = 200, description = "Library sync state", body = SpotifyLibraryState),
        (status = 400, description = "Spotify not linked"),
    ))]
pub async fn put_spotify_library(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    ValidJson(body): ValidJson<SpotifyLibraryUpdate>,
) -> Result<Json<SpotifyLibraryState>, ApiError> {
    deps.spotify_library
        .set(principal.user_id(), body.follow_sync, body.album_sync)
        .await
        .map_err(library_failed)?;
    let state = deps
        .spotify_library
        .state(principal.user_id())
        .await
        .map_err(library_failed)?;
    Ok(Json(state))
}

/// Fork: follow the artists followed at Spotify now, in the background.
/// Follows only: auto-download stays off and nothing is downloaded.
#[utoipa::path(post, tag = "imports", path = "/api/v3/acquire/spotify/library/artists/sync",
    responses(
        (status = 202, description = "Started (or already running)"),
    ))]
pub async fn sync_spotify_artists(
    State(deps): State<ImportsDeps>,
    User(principal): User,
) -> StatusCode {
    deps.spotify_library.spawn(principal.user_id(), false);
    StatusCode::ACCEPTED
}

/// Fork: the albums saved at Spotify, matched here, for the review.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/library/albums",
    responses(
        (status = 200, description = "Saved albums", body = SpotifySavedAlbumList),
    ))]
pub async fn list_spotify_saved_albums(
    State(deps): State<ImportsDeps>,
    User(principal): User,
) -> Result<Json<SpotifySavedAlbumList>, ApiError> {
    let albums = deps
        .spotify_library
        .albums(principal.user_id())
        .await
        .map_err(library_failed)?;
    Ok(Json(albums))
}

/// Fork: read the saved albums again now, in the background. Nothing is
/// requested.
#[utoipa::path(post, tag = "imports", path = "/api/v3/acquire/spotify/library/albums/scan",
    responses(
        (status = 202, description = "Started (or already running)"),
    ))]
pub async fn scan_spotify_saved_albums(
    State(deps): State<ImportsDeps>,
    User(principal): User,
) -> StatusCode {
    deps.spotify_library.spawn(principal.user_id(), true);
    StatusCode::ACCEPTED
}

/// Fork: confirm the saved-album review: no album shows as new any more.
#[utoipa::path(post, tag = "imports", path = "/api/v3/acquire/spotify/library/albums/reviewed",
    responses(
        (status = 204, description = "Reviewed"),
    ))]
pub async fn review_spotify_saved_albums(
    State(deps): State<ImportsDeps>,
    User(principal): User,
) -> Result<StatusCode, ApiError> {
    deps.spotify_library
        .mark_reviewed(principal.user_id())
        .await
        .map_err(library_failed)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Fork: the album, song or artist here a pasted Spotify link names.
/// Never a guess: no confident match answers `found: false` with the
/// Spotify name.
#[utoipa::path(get, tag = "imports", path = "/api/v3/resolve/spotify",
    params(SpotifyResolveQuery),
    responses(
        (status = 200, description = "Resolution", body = SpotifyResolution),
        (status = 400, description = "Not a Spotify track, album or artist link"),
        (status = 502, description = "Spotify, MusicBrainz or Deezer failed"),
    ))]
pub async fn resolve_spotify_link(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    ValidQuery(query): ValidQuery<SpotifyResolveQuery>,
) -> Result<Json<SpotifyResolution>, ApiError> {
    let resolved = deps
        .spotify_library
        .resolve_link(principal.user_id(), &query.url)
        .await
        .map_err(library_failed)?;
    resolved.map(Json).ok_or_else(|| {
        ImportsError::InvalidInput {
            message: "Not a Spotify track, album or artist link".to_owned(),
        }
        .into()
    })
}
