//! In-repo mock Lidarr and Spotify servers.
//!
//! These axum apps are the executable record of the live-verified quirks
//! the imports code ports: the Lidarr mock pins the `/api/v1` + `X-Api-Key`
//! contract (key in query is ignored, wrong key 401s) and the tolerant
//! decode shape (extra fields, `mbId` present-but-ignored); the Spotify
//! mock pins the `/me/playlists` current shape (`items: {total}`, `tracks`
//! null), the `/items` tracks endpoint (the legacy `/tracks` path 403s, as
//! it does for dev-mode apps after the March 2026 migration), and the
//! `track`/`item` entry alias. No test touches a live Lidarr or Spotify.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};

use crate::usenet::ServerGuard;

/// API key the Lidarr mock accepts.
pub const LIDARR_KEY: &str = "lidarr-test-key";
/// Version the Lidarr mock reports.
pub const LIDARR_VERSION: &str = "3.1.3.4968";
/// Bearer token the Spotify mock accepts.
pub const SPOTIFY_TOKEN: &str = "spotify-test-token";
/// Refresh token the Spotify mock accepts.
pub const SPOTIFY_REFRESH: &str = "spotify-refresh-token";
/// Spotify user id the mock owns.
pub const SPOTIFY_USER_ID: &str = "spotify-user-1";
/// A playlist whose one track arrives without its `id`, a changed answer.
pub const SPOTIFY_PLAYLIST_NO_TRACK_ID: &str = "sp-playlist-noid";
/// MBIDs the Lidarr mock serves.
pub const LIDARR_MBID_ALL: &str = "11111111-1111-1111-1111-111111111111";
/// Second mock MBID (monitor `none`).
pub const LIDARR_MBID_NONE: &str = "22222222-2222-2222-2222-222222222222";
/// Unmonitored mock MBID (never imported).
pub const LIDARR_MBID_UNMONITORED: &str = "33333333-3333-3333-3333-333333333333";

/// Loopback mock server handle. Dropping it stops the server.
pub struct MockServer {
    /// Base URL (scheme + host + port, no trailing slash).
    pub base_url: String,
    _guard: ServerGuard,
}

/// Serve `app` on a loopback port.
async fn serve(app: axum::Router) -> Result<MockServer, std::io::Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let guard = ServerGuard::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap_or(());
    }));
    Ok(MockServer {
        base_url: format!("http://{addr}"),
        _guard: guard,
    })
}

/// Recorded upstream calls for test assertions.
#[derive(Debug, Clone, Default)]
pub struct RecordedCalls {
    /// Paths the Lidarr mock served (management paths would show here).
    pub lidarr_paths: Vec<String>,
    /// `X-Api-Key` values the Lidarr mock saw.
    pub lidarr_keys: Vec<String>,
    /// Token-endpoint grant types the accounts mock saw.
    pub spotify_grants: Vec<String>,
    /// API paths the Spotify mock served.
    pub spotify_paths: Vec<String>,
}

/// The redirect URI the mock Spotify app lists by default: the v3 callback
/// as the test routers derive it (host `app.test`, no origin, no base
/// path).
pub const SPOTIFY_REGISTERED_REDIRECT: &str =
    "http://app.test/api/v3/acquire/spotify/auth/callback";

/// Cloneable recorder handle shared by a mock app and its test.
#[derive(Debug, Clone, Default)]
pub struct MockRecorder {
    inner: Arc<Mutex<RecordedCalls>>,
    limit_playlists: Arc<std::sync::atomic::AtomicBool>,
    grown: Arc<std::sync::atomic::AtomicBool>,
    unliked: Arc<std::sync::atomic::AtomicBool>,
    saved_more: Arc<std::sync::atomic::AtomicBool>,
    unsaved: Arc<std::sync::atomic::AtomicBool>,
    unfollowed: Arc<std::sync::atomic::AtomicBool>,
}

impl MockRecorder {
    /// Fork: from now on `sp-playlist-1` has a third song ("Paper
    /// Lanterns", no proving ISRC) and a new snapshot id.
    pub fn grow_playlist(&self) {
        self.grown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn is_grown(&self) -> bool {
        self.grown.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Fork: "Midnight Drive" is unliked from now on (Liked Songs keeps
    /// only "Glass Tides").
    pub fn unlike_song(&self) {
        self.unliked
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Fork: "Glass Tides" is saved from now on (a third saved album).
    pub fn save_album(&self) {
        self.saved_more
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Fork: "Neon Meridian" is un-saved from now on.
    pub fn unsave_album(&self) {
        self.unsaved
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Fork: follow or unfollow "Aurora Current" at Spotify.
    pub fn set_followed(&self, followed: bool) {
        self.unfollowed
            .store(!followed, std::sync::atomic::Ordering::SeqCst);
    }

    fn flag(flag: &std::sync::atomic::AtomicBool) -> bool {
        flag.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Answer `/me/playlists` with a 429 asking for seven seconds from now on.
    pub fn limit_playlists(&self) {
        self.limit_playlists
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The redirect URI the mock app lists.
    pub fn registered_redirect_uri(&self) -> String {
        SPOTIFY_REGISTERED_REDIRECT.to_owned()
    }

    /// Snapshot the calls recorded so far.
    pub fn snapshot(&self) -> RecordedCalls {
        self.inner
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    fn record(&self, visit: impl FnOnce(&mut RecordedCalls)) {
        if let Ok(mut guard) = self.inner.lock() {
            visit(&mut guard);
        }
    }
}

/// Shared mock state.
#[derive(Debug, Clone)]
struct MockState {
    recorder: MockRecorder,
}

/// Serve the mock Lidarr. Serves exactly the two sanctioned GETs; every
/// other path 404s, so a stray management call fails its test loudly.
pub async fn serve_lidarr() -> Result<(MockServer, MockRecorder), std::io::Error> {
    serve_lidarr_with(&[]).await
}

/// Serve the mock Lidarr with extra artists appended to the catalog.
pub async fn serve_lidarr_with(
    extra: &[Value],
) -> Result<(MockServer, MockRecorder), std::io::Error> {
    let recorder = MockRecorder::default();
    let mut artists = vec![
        json!({
            "foreignArtistId": LIDARR_MBID_ALL,
            "artistName": "Aurora Current",
            "monitored": true,
            "monitorNewItems": "all",
            "status": "continuing",
            "mbId": "not-a-mbid",
            "statistics": {"trackFileCount": 12},
        }),
        json!({
            "foreignArtistId": LIDARR_MBID_NONE,
            "artistName": "Boreal Static",
            "monitored": true,
            "monitorNewItems": "none",
            "status": "ended",
        }),
        json!({
            "foreignArtistId": LIDARR_MBID_UNMONITORED,
            "artistName": "Glass Tides",
            "monitored": false,
            "monitorNewItems": "all",
            "status": "continuing",
        }),
        json!({
            "foreignArtistId": "not-a-mbid",
            "artistName": "Broken Join Key",
            "monitored": true,
            "monitorNewItems": "all",
            "status": "continuing",
        }),
    ];
    artists.extend(extra.iter().cloned());
    let state = LidarrState {
        recorder: recorder.clone(),
        artists,
    };
    let app = axum::Router::new()
        .route("/api/v1/system/status", get(lidarr_status))
        .route("/api/v1/artist", get(lidarr_artists))
        .fallback(lidarr_fallback)
        .with_state(state);
    Ok((serve(app).await?, recorder))
}

/// Lidarr mock state.
#[derive(Debug, Clone)]
struct LidarrState {
    recorder: MockRecorder,
    artists: Vec<Value>,
}

fn lidarr_authed(headers: &HeaderMap) -> bool {
    headers
        .get("X-Api-Key")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|key| key == LIDARR_KEY)
}

async fn lidarr_status(State(state): State<LidarrState>, headers: HeaderMap) -> Response {
    state.recorder.record(|calls| {
        calls.lidarr_paths.push("/api/v1/system/status".to_owned());
        calls.lidarr_keys.push(
            headers
                .get("X-Api-Key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned(),
        );
    });
    if !lidarr_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"appName": "Lidarr", "version": LIDARR_VERSION})).into_response()
}

async fn lidarr_artists(State(state): State<LidarrState>, headers: HeaderMap) -> Response {
    state.recorder.record(|calls| {
        calls.lidarr_paths.push("/api/v1/artist".to_owned());
        calls.lidarr_keys.push(
            headers
                .get("X-Api-Key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned(),
        );
    });
    if !lidarr_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(state.artists.clone()).into_response()
}

async fn lidarr_fallback(
    State(state): State<LidarrState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let path = uri.path().to_owned();
    state.recorder.record(|calls| {
        calls.lidarr_paths.push(path.clone());
        calls.lidarr_keys.push(
            headers
                .get("X-Api-Key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned(),
        );
    });
    StatusCode::NOT_FOUND.into_response()
}

/// Serve the mock Spotify pair. Returns `(api, accounts, covers, recorder)`
/// base URLs: the API mock, the accounts (token) mock, and the cover host
/// the playlist images point at.
pub async fn serve_spotify()
-> Result<(MockServer, MockServer, MockServer, MockRecorder), std::io::Error> {
    let recorder = MockRecorder::default();
    let covers = serve(cover_app()).await?;
    let state = SpotifyState {
        recorder: recorder.clone(),
        cover_base: covers.base_url.clone(),
    };
    let api = axum::Router::new()
        .route("/me", get(spotify_me))
        .route("/me/playlists", get(spotify_playlists))
        .route("/playlists/{id}", get(spotify_playlist))
        .route("/playlists/{id}/items", get(spotify_items))
        .route("/playlists/{id}/tracks", get(spotify_tracks_tombstone))
        .route("/me/tracks", get(spotify_liked))
        .route("/me/albums", get(spotify_saved_albums))
        .route("/me/following", get(spotify_following))
        .route("/tracks/{id}", get(spotify_track))
        .route("/albums/{id}", get(spotify_album))
        .route("/artists/{id}", get(spotify_artist))
        .route("/artists/{id}/albums", get(spotify_artist_albums))
        .route("/oembed", get(spotify_oembed))
        .with_state(state);
    let accounts = axum::Router::new()
        .route("/api/token", axum::routing::post(spotify_token))
        .with_state(MockState {
            recorder: recorder.clone(),
        });
    Ok((serve(api).await?, serve(accounts).await?, covers, recorder))
}

/// Spotify API mock state.
#[derive(Debug, Clone)]
struct SpotifyState {
    recorder: MockRecorder,
    cover_base: String,
}

fn spotify_authed(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {SPOTIFY_TOKEN}"))
}

async fn spotify_me(State(state): State<SpotifyState>, headers: HeaderMap) -> Response {
    state
        .recorder
        .record(|calls| calls.spotify_paths.push("/me".to_owned()));
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"id": SPOTIFY_USER_ID, "display_name": "Mock Listener"})).into_response()
}

async fn spotify_playlists(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    state
        .recorder
        .record(|calls| calls.spotify_paths.push("/me/playlists".to_owned()));
    if state
        .recorder
        .limit_playlists
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "7")]).into_response();
    }
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // Current simplified-playlist shape: the count under `items` as a dict,
    // `tracks` null. One foreign-owned playlist must be filtered out.
    let all = vec![
        json!({
            "id": "sp-playlist-1",
            "name": "Neon Nights",
            "description": "Synthwave staples",
            "images": [
                {"url": format!("{}/covers/neon-small.jpg", state.cover_base), "width": 60},
                {"url": format!("{}/covers/neon.jpg", state.cover_base), "width": 640},
            ],
            "owner": {"id": SPOTIFY_USER_ID, "display_name": "Mock Listener"},
            "items": {"href": "https://api.spotify.com/v1/x", "total": 2},
            "tracks": null,
        }),
        json!({
            "id": "sp-playlist-2",
            "name": "Paper Satellites",
            "description": "",
            "images": [],
            "owner": {"id": SPOTIFY_USER_ID, "display_name": "Mock Listener"},
            "tracks": {"href": "https://api.spotify.com/v1/y", "total": 0},
        }),
        json!({
            "id": "sp-foreign",
            "name": "Someone Else's",
            "description": "",
            "images": [],
            "owner": {"id": "another-user", "display_name": "Stranger"},
            "items": {"href": "https://api.spotify.com/v1/z", "total": 9},
            "tracks": null,
        }),
    ];
    let offset: usize = params
        .get("offset")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let limit: usize = params
        .get("limit")
        .and_then(|value| value.parse().ok())
        .unwrap_or(50);
    let total = all.len();
    let items: Vec<Value> = all.into_iter().skip(offset).take(limit).collect();
    Json(json!({"items": items, "total": total, "limit": limit, "offset": offset})).into_response()
}

async fn spotify_playlist(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    state
        .recorder
        .record(|calls| calls.spotify_paths.push(format!("/playlists/{id}")));
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if id != "sp-playlist-1" && id != SPOTIFY_PLAYLIST_NO_TRACK_ID {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(json!({
        "id": id,
        "name": "Neon Nights",
        "images": [
            {"url": format!("{}/covers/neon-small.jpg", state.cover_base), "width": 60},
            {"url": format!("{}/covers/neon.jpg", state.cover_base), "width": 640},
        ],
        "tracks": {"total": if state.recorder.is_grown() { 3 } else { 2 }},
        "snapshot_id": if state.recorder.is_grown() { "snap-2" } else { "snap-1" },
    }))
    .into_response()
}

async fn spotify_items(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    state
        .recorder
        .record(|calls| calls.spotify_paths.push(format!("/playlists/{id}/items")));
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if id == SPOTIFY_PLAYLIST_NO_TRACK_ID {
        return Json(json!({
            "items": [{"track": {
                "type": "track", "name": "Nameless",
                "artists": [{"name": "Aurora Current"}],
                "album": {"id": "sp-album-1", "name": "Neon Meridian", "images": []},
            }}],
            "next": null,
        }))
        .into_response();
    }
    if id != "sp-playlist-1" {
        return StatusCode::NOT_FOUND.into_response();
    }
    // One entry under the legacy `track` alias, one under the new `item`
    // key, plus an `is_local` row the client must skip. A grown playlist
    // (fork) has a third song.
    let grown = state.recorder.is_grown().then(|| {
        json!({"track": {
            "id": "sp-track-3", "type": "track", "name": "Paper Lanterns",
            "artists": [{"name": "Boreal Static"}],
            "album": {"id": "sp-album-4", "name": "Paper Lanterns", "images": []},
            "external_ids": {"isrc": "USRC17607841"},
            "track_number": 1, "disc_number": 1, "duration_ms": 222000,
        }})
    });
    let mut body = json!({
        "items": [
            {"track": {
                "id": "sp-track-1", "type": "track", "name": "Midnight Drive",
                "artists": [{"name": "Aurora Current"}],
                "album": {"id": "sp-album-1", "name": "Neon Meridian", "images": []},
                "external_ids": {"isrc": "USRC17607839"},
                "track_number": 1, "disc_number": 1, "duration_ms": 183000,
            }},
            {"item": {
                "id": "sp-track-2", "type": "track", "name": "Glass Tides",
                "artists": [{"name": "Boreal Static"}],
                "album": {"id": "sp-album-2", "name": "Glass Tides", "images": []},
                "external_ids": {"isrc": "USRC17607840"},
                "track_number": 2, "disc_number": 1, "duration_ms": 201000,
            }},
            {"is_local": true, "track": {
                "id": "sp-local", "type": "track", "name": "Local File",
                "artists": [{"name": "Nobody"}],
                "album": {"id": "sp-album-3", "name": "Local", "images": []},
            }},
        ],
        "next": null,
    });
    if let (Some(extra), Some(items)) = (grown, body["items"].as_array_mut()) {
        items.insert(2, extra);
    }
    Json(body).into_response()
}

/// Fork: a Spotify id the library mock answers for "Midnight Drive",
/// "Neon Meridian" and "Aurora Current" (22 base-62 characters, so links
/// to them parse).
pub const SPOTIFY_LINK_TRACK: &str = "0MidnightDrive00000001";
/// See [`SPOTIFY_LINK_TRACK`].
pub const SPOTIFY_LINK_ALBUM: &str = "0NeonMeridian000000001";
/// See [`SPOTIFY_LINK_TRACK`].
pub const SPOTIFY_LINK_ARTIST: &str = "0AuroraCurrent00000001";
/// A Spotify album id nothing here matches ("Nothing Known").
pub const SPOTIFY_LINK_UNKNOWN: &str = "0NothingKnown000000001";

fn midnight_drive() -> Value {
    json!({
        "id": "sp-track-1", "type": "track", "name": "Midnight Drive",
        "artists": [{"name": "Aurora Current"}],
        "album": {"id": "sp-album-1", "name": "Neon Meridian", "images": []},
        "external_ids": {"isrc": "USRC17607839"},
        "track_number": 1, "disc_number": 1, "duration_ms": 183000,
    })
}

fn glass_tides_song() -> Value {
    json!({
        "id": "sp-track-2", "type": "track", "name": "Glass Tides",
        "artists": [{"name": "Boreal Static"}],
        "album": {"id": "sp-album-2", "name": "Glass Tides", "images": []},
        "external_ids": {"isrc": "USRC17607840"},
        "track_number": 2, "disc_number": 1, "duration_ms": 201000,
    })
}

fn album_body(id: &str, title: &str, artist: &str, tracks: i64) -> Value {
    json!({
        "id": id, "name": title, "artists": [{"id": null, "name": artist}],
        "total_tracks": tracks, "release_date": "2024-05-03", "album_type": "album",
        "images": [], "external_ids": {},
    })
}

fn page_of(all: Vec<Value>, params: &HashMap<String, String>) -> Value {
    let offset: usize = params
        .get("offset")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let limit: usize = params
        .get("limit")
        .and_then(|value| value.parse().ok())
        .unwrap_or(50);
    let total = all.len();
    let items: Vec<Value> = all.into_iter().skip(offset).take(limit).collect();
    json!({"items": items, "total": total, "next": null})
}

fn record_path(state: &SpotifyState, path: String) {
    state
        .recorder
        .record(|calls| calls.spotify_paths.push(path));
}

/// Fork: Liked Songs, newest first.
async fn spotify_liked(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    record_path(&state, "/me/tracks".to_owned());
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut all = vec![json!({"added_at": "2026-11-02T00:00:00Z", "track": glass_tides_song()})];
    if !MockRecorder::flag(&state.recorder.unliked) {
        all.push(json!({"added_at": "2026-11-01T00:00:00Z", "track": midnight_drive()}));
    }
    Json(page_of(all, &params)).into_response()
}

/// Fork: saved albums: "Neon Meridian" (matches at MusicBrainz by name)
/// and "Nothing Known" (matches nowhere); "Glass Tides" once saved.
async fn spotify_saved_albums(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    record_path(&state, "/me/albums".to_owned());
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut all = Vec::new();
    if MockRecorder::flag(&state.recorder.saved_more) {
        all.push(json!({"added_at": "2026-11-03T00:00:00Z",
            "album": album_body("sp-album-2", "Glass Tides", "Boreal Static", 1)}));
    }
    if !MockRecorder::flag(&state.recorder.unsaved) {
        all.push(json!({"added_at": "2026-11-02T00:00:00Z",
            "album": album_body("sp-album-1", "Neon Meridian", "Aurora Current", 1)}));
    }
    all.push(json!({"added_at": "2026-11-01T00:00:00Z",
        "album": album_body("sp-album-9", "Nothing Known", "Nobody Known", 3)}));
    Json(page_of(all, &params)).into_response()
}

/// Fork: followed artists: "Aurora Current" unless unfollowed.
async fn spotify_following(State(state): State<SpotifyState>, headers: HeaderMap) -> Response {
    record_path(&state, "/me/following".to_owned());
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut items = Vec::new();
    if !MockRecorder::flag(&state.recorder.unfollowed) {
        items.push(json!({"id": SPOTIFY_LINK_ARTIST, "name": "Aurora Current", "images": []}));
    }
    let total = items.len();
    Json(
        json!({"artists": {"items": items, "next": null, "cursors": {"after": null},
        "total": total}}),
    )
    .into_response()
}

async fn spotify_track(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    record_path(&state, format!("/tracks/{id}"));
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if id != SPOTIFY_LINK_TRACK {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(midnight_drive()).into_response()
}

async fn spotify_album(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    record_path(&state, format!("/albums/{id}"));
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match id.as_str() {
        SPOTIFY_LINK_ALBUM => {
            Json(album_body(&id, "Neon Meridian", "Aurora Current", 1)).into_response()
        }
        SPOTIFY_LINK_UNKNOWN => {
            Json(album_body(&id, "Nothing Known", "Nobody Known", 3)).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn spotify_artist(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    record_path(&state, format!("/artists/{id}"));
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if id != SPOTIFY_LINK_ARTIST {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(json!({"id": id, "name": "Aurora Current", "images": []})).into_response()
}

async fn spotify_artist_albums(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    record_path(&state, format!("/artists/{id}/albums"));
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(
        json!({"items": [album_body("sp-album-1", "Neon Meridian", "Aurora Current", 1)],
        "next": null}),
    )
    .into_response()
}

/// Fork: the public oEmbed answer: a name, nothing more.
async fn spotify_oembed(
    State(state): State<SpotifyState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    record_path(&state, "/oembed".to_owned());
    let url = params.get("url").cloned().unwrap_or_default();
    let title = if url.contains(SPOTIFY_LINK_ALBUM) {
        "Neon Meridian"
    } else if url.contains(SPOTIFY_LINK_TRACK) {
        "Midnight Drive"
    } else if url.contains(SPOTIFY_LINK_ARTIST) {
        "Aurora Current"
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(json!({"title": title, "type": "rich"})).into_response()
}

/// The deprecated `/tracks` path: 403, as live Spotify answers dev-mode
/// apps after the March 2026 migration. Pins the `/items` choice.
async fn spotify_tracks_tombstone(State(state): State<SpotifyState>) -> Response {
    state
        .recorder
        .record(|calls| calls.spotify_paths.push("LEGACY_TRACKS_PATH".to_owned()));
    StatusCode::FORBIDDEN.into_response()
}

async fn spotify_token(
    State(state): State<MockState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let params: HashMap<String, String> = serde_urlencoded_parse(&body).into_iter().collect();
    let grant = params.get("grant_type").cloned().unwrap_or_default();
    state
        .recorder
        .record(|calls| calls.spotify_grants.push(grant.clone()));
    // Basic auth required, any test client id accepted.
    let authed = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("Basic "));
    if !authed {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match grant.as_str() {
        "authorization_code"
            if params.get("code").is_some_and(|code| code == "good-code")
                && params
                    .get("redirect_uri")
                    .is_some_and(|uri| *uri == state.recorder.registered_redirect_uri()) =>
        {
            Json(json!({
                "access_token": SPOTIFY_TOKEN,
                "refresh_token": SPOTIFY_REFRESH,
                "expires_in": 3600,
                "scope": "playlist-read-private user-library-read user-follow-read",
            }))
            .into_response()
        }
        "refresh_token"
            if params
                .get("refresh_token")
                .is_some_and(|token| token == SPOTIFY_REFRESH) =>
        {
            Json(json!({
                "access_token": SPOTIFY_TOKEN,
                "expires_in": 3600,
            }))
            .into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

/// Minimal form decoder (no new dependency).
fn serde_urlencoded_parse(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hi = bytes.next().unwrap_or(b'0');
            let lo = bytes.next().unwrap_or(b'0');
            let hex = |digit: u8| match digit {
                b'0'..=b'9' => digit - b'0',
                b'a'..=b'f' => digit - b'a' + 10,
                b'A'..=b'F' => digit - b'A' + 10,
                _ => 0,
            };
            out.push((hex(hi) * 16 + hex(lo)) as char);
        } else if byte == b'+' {
            out.push(' ');
        } else {
            out.push(byte as char);
        }
    }
    out
}

/// Cover host: one real image, one redirect (must be refused), one
/// non-image (must be refused).
fn cover_app() -> axum::Router {
    axum::Router::new()
        .route("/covers/neon.jpg", get(cover_image))
        .route("/covers/neon-small.jpg", get(cover_image))
        .route("/covers/redirect.jpg", get(cover_redirect))
        .route("/covers/not-an-image.jpg", get(cover_text))
}

async fn cover_image() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CONTENT_LENGTH, "16"),
        ],
        Body::from(vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11,
        ]),
    )
        .into_response()
}

async fn cover_redirect() -> Response {
    (StatusCode::FOUND, [(header::LOCATION, "/covers/neon.jpg")]).into_response()
}

async fn cover_text() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html")],
        "nope",
    )
        .into_response()
}
