//! Spotify: the accounts host (the authorize page and the token grants)
//! and the Web API reads a playlist import makes.
//!
//! Every call sends through the executor on the `spotify` rate row, at the
//! priority the caller names. Spotify counts calls per app in a rolling
//! 30-second window and answers 429 with `Retry-After` past it; the
//! executor turns that wait into a cooldown on the row. The caller holds
//! the user's tokens and refreshes them: an expired token is
//! [`ProviderError::Unauthorized`]. The wire shapes are in
//! `spotify/models.rs`; the playlist covers come through
//! [`ImageClient::spotify_cover`](crate::images::ImageClient::spotify_cover).

use std::time::Duration;

use base64::Engine as _;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::ProviderError;
use crate::executor::{Answer, Backoff, Executor, Request, Spec, Url, standard_classify};

pub mod models;

use models::{EntryPage, PlaylistInfo, PlaylistPage, Profile, TokenAnswer};

/// The provider name errors carry, and its rate row.
pub const SOURCE: &str = "spotify";
/// Spotify Web API root.
pub const API_BASE: &str = "https://api.spotify.com/v1";
/// Spotify accounts origin: the authorize page and the token grants.
pub const ACCOUNTS_BASE: &str = "https://accounts.spotify.com";
/// OAuth scopes the import asks for. Fork: the library scopes read Liked
/// Songs, saved albums (`user-library-read`) and followed artists
/// (`user-follow-read`).
pub const SCOPES: &str = "playlist-read-private playlist-read-collaborative user-read-private user-library-read user-follow-read";
/// Fork: the scopes the library sync needs on top of the playlist ones.
pub const LIBRARY_SCOPES: [&str; 2] = ["user-library-read", "user-follow-read"];
/// Fork: Spotify's public web origin, which answers oEmbed for a link.
pub const WEB_BASE: &str = "https://open.spotify.com";
/// Fork: saved items read per page (Spotify's maximum).
pub const LIBRARY_PAGE: i64 = 50;
/// Largest Web API or accounts answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Playlists read per page of the caller's list (Spotify's maximum).
pub const PLAYLISTS_PAGE: i64 = 50;
/// Entries read per page of one playlist (Spotify's maximum).
pub const ITEMS_PAGE: i64 = 100;

/// Characters a query value keeps as they are (RFC 3986 unreserved);
/// everything else is percent-encoded.
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Fork: the playlist entry fields an import reads, so Spotify sends only
/// those. The track sits under `track` or `item` (see
/// [`models::PlaylistEntry`]), so both are named.
pub const ITEM_FIELDS: &str = "next,items(is_local,\
track(id,type,name,artists(name),album(id,name,images),external_ids(isrc),track_number,disc_number,duration_ms),\
item(id,type,name,artists(name),album(id,name,images),external_ids(isrc),track_number,disc_number,duration_ms))";

/// Fork: why Spotify last refused a call (429), as its body said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// Spotify's `error.reason`, e.g. `QUOTA_EXCEEDED`; empty when the body
    /// named none (the rolling rate limit).
    pub reason: String,
    /// The wait its `Retry-After` named, uncapped.
    pub retry_after: Option<Duration>,
}

impl Refusal {
    /// True when the app's allowance is used up, not just paced.
    #[must_use]
    pub fn is_quota(&self) -> bool {
        self.reason.eq_ignore_ascii_case("QUOTA_EXCEEDED")
    }
}

/// Fork: the last 429 Spotify answered, as the classifier read it. The
/// spec's classifier is a plain function, so it is kept here for the caller
/// to take right after the call that failed.
static LAST_REFUSAL: std::sync::Mutex<Option<Refusal>> = std::sync::Mutex::new(None);

/// Fork: take the reason of the last 429 Spotify answered, if one is
/// waiting.
pub fn take_refusal() -> Option<Refusal> {
    LAST_REFUSAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

/// Fork: Spotify's 429 body, `{"error":{"status":429,"reason":"..."}}`.
#[derive(serde::Deserialize)]
struct RefusalBody {
    error: RefusalError,
}

#[derive(serde::Deserialize)]
struct RefusalError {
    #[serde(default)]
    reason: Option<String>,
}

/// Fork: the reason a 429 body names, empty when none.
#[must_use]
pub fn refusal_reason(body: &[u8]) -> String {
    serde_json::from_slice::<RefusalBody>(body)
        .ok()
        .and_then(|body| body.error.reason)
        .unwrap_or_default()
}

/// The standard rules, keeping the reason of a 429 (fork).
fn classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    if answer.reply.status() == 429 {
        let refusal = Refusal {
            reason: refusal_reason(answer.reply.bytes()),
            retry_after: answer.retry_after(),
        };
        *LAST_REFUSAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(refusal);
    }
    standard_classify(answer)
}

/// Spotify on the wire: the standard status rules. Spotify normally names
/// its wait on a 429; one that does not rests the row a few seconds,
/// doubling. Fork: a long wait it names (an app over its development-mode
/// quota asks for many hours) is kept in full, up to a day: calling again
/// sooner is only refused, and stretches the quota's wait.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Exponential {
        base: Duration::from_secs(2),
        max: Duration::from_secs(60),
    },
    retry_after_cap: Duration::from_secs(24 * 3600),
    classify,
};

/// A token grant from the accounts host. Debug is hand-written: the tokens
/// never appear in debug output.
#[derive(Clone, PartialEq)]
pub struct TokenGrant {
    /// Fresh access token.
    pub access_token: String,
    /// Rotated refresh token, when the grant carries one.
    pub refresh_token: Option<String>,
    /// Lifetime in seconds (default 3600).
    pub expires_in_secs: i64,
    /// Fork: the scopes granted, when the grant names them.
    pub scope: Option<String>,
}

impl std::fmt::Debug for TokenGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenGrant")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_in_secs", &self.expires_in_secs)
            .field("scope", &self.scope)
            .finish()
    }
}

impl From<TokenAnswer> for TokenGrant {
    fn from(answer: TokenAnswer) -> Self {
        Self {
            access_token: answer.access_token,
            refresh_token: answer.refresh_token,
            expires_in_secs: answer.expires_in.unwrap_or(3600),
            scope: answer.scope.filter(|scope| !scope.trim().is_empty()),
        }
    }
}

/// The Spotify client. Built once with the providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct SpotifyClient {
    executor: Executor,
    api_base: Option<Url>,
    accounts_base: Option<Url>,
    /// Fork: where oEmbed is asked: Spotify's web origin beside the real
    /// API, else the API's own address (a test mock serves both).
    web_base: Option<Url>,
}

impl SpotifyClient {
    /// Calls through `executor` to the Web API at `api_base` and the
    /// accounts host at `accounts_base`.
    #[must_use]
    pub fn new(executor: Executor, api_base: &str, accounts_base: &str) -> Self {
        let web_base = if api_base.trim_end_matches('/') == API_BASE {
            WEB_BASE
        } else {
            api_base
        };
        Self {
            executor,
            api_base: parse_base(api_base, SOURCE),
            accounts_base: parse_base(accounts_base, SOURCE),
            web_base: parse_base(web_base, SOURCE),
        }
    }

    /// The authorize page for one app, redirect URI and state, or `None`
    /// when the accounts address is unusable.
    #[must_use]
    pub fn authorize_url(&self, client_id: &str, redirect_uri: &str, state: &str) -> Option<Url> {
        let mut url = self.accounts_base.clone()?;
        url.path_segments_mut()
            .ok()?
            .pop_if_empty()
            .push("authorize");
        let query = [
            ("client_id", client_id),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri),
            ("scope", SCOPES),
            ("state", state),
        ]
        .iter()
        .map(|(key, value)| format!("{key}={}", utf8_percent_encode(value, UNRESERVED)))
        .collect::<Vec<_>>()
        .join("&");
        url.set_query(Some(&query));
        Some(url)
    }

    /// Exchange an OAuth code for tokens.
    ///
    /// # Errors
    ///
    /// The executor's typed failure, or a payload error for an answer that
    /// does not decode.
    pub async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        redirect_uri: &str,
        priority: RequestPriority,
    ) -> Result<TokenGrant, ProviderError> {
        self.token(
            client_id,
            client_secret,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ],
            priority,
        )
        .await
    }

    /// Refresh an access token.
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::exchange_code`].
    pub async fn refresh(
        &self,
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
        priority: RequestPriority,
    ) -> Result<TokenGrant, ProviderError> {
        self.token(
            client_id,
            client_secret,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
            priority,
        )
        .await
    }

    /// One Basic-auth grant on the accounts host.
    async fn token(
        &self,
        client_id: &str,
        client_secret: &str,
        form: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<TokenGrant, ProviderError> {
        let base = self
            .accounts_base
            .as_ref()
            .ok_or_else(|| unusable_base(SOURCE))?;
        let form = form
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        let credentials = base64::engine::general_purpose::STANDARD
            .encode(format!("{client_id}:{client_secret}"));
        let request = Request::post_form(base, &["api", "token"], form)
            .map_err(|_| unusable_base(SOURCE))?
            .header("authorization", format!("Basic {credentials}"));
        let reply = self.executor.send(request, priority).await?;
        let answer: TokenAnswer = decode(&reply)?;
        Ok(TokenGrant::from(answer))
    }

    /// The user `access_token` belongs to (`GET /me`).
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::exchange_code`]; an expired token is
    /// [`ProviderError::Unauthorized`].
    pub async fn me(
        &self,
        access_token: &str,
        priority: RequestPriority,
    ) -> Result<Profile, ProviderError> {
        self.get(access_token, &["me"], &[], priority).await
    }

    /// One page of the caller's playlists (`GET /me/playlists`), from
    /// `offset`, [`PLAYLISTS_PAGE`] at a time.
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`].
    pub async fn playlists_page(
        &self,
        access_token: &str,
        offset: i64,
        priority: RequestPriority,
    ) -> Result<PlaylistPage, ProviderError> {
        let (limit, offset) = (PLAYLISTS_PAGE.to_string(), offset.to_string());
        self.get(
            access_token,
            &["me", "playlists"],
            &[("limit", &limit), ("offset", &offset)],
            priority,
        )
        .await
    }

    /// One playlist's metadata (`GET /playlists/{id}`).
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`].
    pub async fn playlist(
        &self,
        access_token: &str,
        playlist_id: &str,
        priority: RequestPriority,
    ) -> Result<PlaylistInfo, ProviderError> {
        self.get(
            access_token,
            &["playlists", playlist_id],
            &[("fields", "id,name,images,tracks.total,snapshot_id")],
            priority,
        )
        .await
    }

    /// One page of a playlist's entries (`GET /playlists/{id}/items`), from
    /// `offset`, [`ITEMS_PAGE`] at a time. The older `/tracks` endpoint is
    /// deprecated and 403s for development-mode apps after the March 2026
    /// migration: never switch this back.
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`].
    pub async fn playlist_items_page(
        &self,
        access_token: &str,
        playlist_id: &str,
        offset: i64,
        priority: RequestPriority,
    ) -> Result<EntryPage, ProviderError> {
        let (limit, offset) = (ITEMS_PAGE.to_string(), offset.to_string());
        self.get(
            access_token,
            &["playlists", playlist_id, "items"],
            &[
                ("limit", &limit),
                ("offset", &offset),
                ("fields", ITEM_FIELDS),
            ],
            priority,
        )
        .await
    }

    /// Fork: one album (`GET /albums/{id}`), for its barcode. The batch
    /// `GET /albums` is gone for development-mode apps (February 2026).
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`].
    pub async fn album(
        &self,
        access_token: &str,
        album_id: &str,
        priority: RequestPriority,
    ) -> Result<models::AlbumDetail, ProviderError> {
        self.get(access_token, &["albums", album_id], &[], priority)
            .await
    }

    /// Fork: one page of the caller's Liked Songs (`GET /me/tracks`), from
    /// `offset`, `limit` at a time (at most [`LIBRARY_PAGE`]).
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`]; a link without `user-library-read` is
    /// refused (403).
    pub async fn saved_tracks_page(
        &self,
        access_token: &str,
        offset: i64,
        limit: i64,
        priority: RequestPriority,
    ) -> Result<models::SavedTrackPage, ProviderError> {
        let (limit, offset) = (limit.clamp(1, LIBRARY_PAGE).to_string(), offset.to_string());
        self.get(
            access_token,
            &["me", "tracks"],
            &[("limit", &limit), ("offset", &offset)],
            priority,
        )
        .await
    }

    /// Fork: one page of the caller's saved albums (`GET /me/albums`), from
    /// `offset`, [`LIBRARY_PAGE`] at a time.
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::saved_tracks_page`].
    pub async fn saved_albums_page(
        &self,
        access_token: &str,
        offset: i64,
        priority: RequestPriority,
    ) -> Result<models::SavedAlbumPage, ProviderError> {
        let (limit, offset) = (LIBRARY_PAGE.to_string(), offset.to_string());
        self.get(
            access_token,
            &["me", "albums"],
            &[("limit", &limit), ("offset", &offset)],
            priority,
        )
        .await
    }

    /// Fork: one cursor page of the artists the caller follows
    /// (`GET /me/following?type=artist`), after cursor `after`.
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`]; a link without `user-follow-read` is
    /// refused (403).
    pub async fn followed_artists_page(
        &self,
        access_token: &str,
        after: Option<&str>,
        priority: RequestPriority,
    ) -> Result<models::FollowedArtists, ProviderError> {
        let limit = LIBRARY_PAGE.to_string();
        let mut query = vec![("type", "artist"), ("limit", limit.as_str())];
        if let Some(after) = after {
            query.push(("after", after));
        }
        self.get(access_token, &["me", "following"], &query, priority)
            .await
    }

    /// Fork: one track (`GET /tracks/{id}`).
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`].
    pub async fn track(
        &self,
        access_token: &str,
        track_id: &str,
        priority: RequestPriority,
    ) -> Result<models::EntryTrack, ProviderError> {
        self.get(access_token, &["tracks", track_id], &[], priority)
            .await
    }

    /// Fork: one artist (`GET /artists/{id}`).
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`].
    pub async fn artist(
        &self,
        access_token: &str,
        artist_id: &str,
        priority: RequestPriority,
    ) -> Result<models::SpotifyArtist, ProviderError> {
        self.get(access_token, &["artists", artist_id], &[], priority)
            .await
    }

    /// Fork: the first page of an artist's albums and singles
    /// (`GET /artists/{id}/albums`), for the album-overlap check. Spotify
    /// answers 400 to a `limit` over 10 for development-mode apps
    /// (February 2026), so the page is kept at 10.
    ///
    /// # Errors
    ///
    /// As [`SpotifyClient::me`].
    pub async fn artist_albums_page(
        &self,
        access_token: &str,
        artist_id: &str,
        priority: RequestPriority,
    ) -> Result<models::ArtistAlbumPage, ProviderError> {
        self.get(
            access_token,
            &["artists", artist_id, "albums"],
            &[("include_groups", "album,single"), ("limit", "10")],
            priority,
        )
        .await
    }

    /// Fork: the name behind a public Spotify link, through oEmbed, which
    /// needs no account. `link` is the canonical `open.spotify.com` address
    /// the caller built from a parsed id.
    ///
    /// # Errors
    ///
    /// The executor's typed failure, or a payload error.
    pub async fn oembed(
        &self,
        link: &str,
        priority: RequestPriority,
    ) -> Result<models::OEmbed, ProviderError> {
        let base = self
            .web_base
            .as_ref()
            .ok_or_else(|| unusable_base(SOURCE))?;
        let request =
            Request::get(base, &["oembed"], &[("url", link)]).map_err(|_| unusable_base(SOURCE))?;
        let reply = self.executor.send(request, priority).await?;
        decode(&reply)
    }

    /// One authenticated Web API GET of `path`. Each path value is one
    /// segment, so an id from a request cannot add a path or a query.
    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        access_token: &str,
        path: &[&str],
        query: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<T, ProviderError> {
        let base = self
            .api_base
            .as_ref()
            .ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(base, path, query)
            .map_err(|_| unusable_base(SOURCE))?
            .header("authorization", format!("Bearer {access_token}"));
        let reply = self.executor.send(request, priority).await?;
        decode(&reply)
    }
}
