//! Fork: a user's Spotify library kept in step (followed artists, saved
//! albums) and pasted Spotify links resolved.
//!
//! - Followed artists are matched with [`resolve_artist`]. The first time
//!   an artist matches, it is followed here unless it already was, with
//!   auto-download off; the row remembers that the sync made the follow.
//!   An artist unfollowed at Spotify is unfollowed here only when the sync
//!   made that follow and no other followed Spotify artist maps to it. A
//!   follow made by hand is never removed.
//! - Saved albums are matched with [`resolve_album`] and listed for review.
//!   The first scan is the baseline; an album saved after it is flagged
//!   new until the user confirms the review, and nothing is ever requested
//!   by the sync. A matched album the library holds is favourited; an
//!   un-saved one loses only a favourite the sync added. No file and no
//!   request is ever deleted.
//! - Unmatched albums are looked up again after a week; an unmatched artist
//!   after a day, a week, then a month.
//!
//! Spotify limits apps in development mode hard, and its artist-albums
//! read most of all, so the sync spends as few calls as it can:
//! - an artist is matched by its MusicBrainz link, the credits of saved
//!   albums already proved, and its name (MusicBrainz, then Deezer) before
//!   its albums are read at Spotify, and at most
//!   [`ARTIST_FETCHES_PER_PASS`] such reads go per pass;
//! - every lookup is recorded before it calls Spotify, so a lookup a rate
//!   limit cut short waits its turn instead of going first again;
//! - followed artists are read once a day (sooner while a sync cut short
//!   has artists left), saved albums only down to the first page already
//!   known, with a full read (for un-saves) once a day;
//! - a wait Spotify asks for ends the pass, is kept across a restart
//!   ([`SpotifyLibrary::keep_cooldown`]), and a pass or "Sync now" during
//!   one does not start; "Sync now" runs a part at most once in
//!   [`SYNC_NOW_GAP`].
//!
//! The playlist sync runs [`SpotifyLibrary::run_once`] each pass (every 15
//! minutes); "Sync now" runs one part for one user in the background.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use droppedneedle_catalog::collections::db::CollectionsDb;
use droppedneedle_catalog::collections::store::FavoriteStore;
use droppedneedle_kernel::db::DbError;
use droppedneedle_library::identity::reads::copy_for_group;
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::spotify::models::{AlbumDetail, EntryTrack, SpotifyArtist};
use rusqlite::{OptionalExtension, params};

use super::models::{
    SpotifyLibraryState, SpotifyResolution, SpotifySavedAlbum, SpotifySavedAlbumList,
};
use super::spotify::{ImportFailure, SpotifyImportService};
use super::spotify_client::{
    SpotifyError, best_image_url, rate_limited_message, unavailable_message,
};
use super::spotify_resolve::{
    AlbumFacts, ArtistFacts, Found, LinkKind, Live, ResolveError, TrackFacts, parse_link,
    resolve_album, resolve_artist, resolve_artist_named, resolve_track, same_name,
};
use super::sql::{
    ALBUMS_CHECKED, ARTISTS_CHECKED, ATTEMPT_ARTIST, COOLDOWN_UNTIL, DROP_ALBUM, DROP_ARTIST,
    ENSURE_LIBRARY, FAVORITED_ALBUM, FOLLOWED, KEEP_COOLDOWN, LIBRARY_FAILED, LIBRARY_ROW,
    LIBRARY_USERS, MATCH_ALBUM, MATCH_ARTIST, NEW_ALBUMS, REQUESTED, REVIEW_ALBUMS, SAVED_ALBUMS,
    SET_LIBRARY, UPSERT_ALBUM, UPSERT_ARTIST,
};
use crate::db::AcquireDb;
use crate::follows::Follows;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// How long an unmatched album waits before it is looked up again.
const RETRY_UNMATCHED_MS: i64 = 7 * DAY_MS;

/// How long an unmatched artist waits before it is looked up again, by the
/// lookups it has had: a day, a week, then a month.
fn artist_wait_ms(attempts: i64) -> i64 {
    match attempts {
        ..=1 => DAY_MS,
        2 => 7 * DAY_MS,
        _ => 30 * DAY_MS,
    }
}

/// How long before an artist's albums are read at Spotify again.
const REFETCH_MS: i64 = 30 * DAY_MS;

/// The most artists whose albums one pass reads at Spotify (its most
/// limited read); the rest wait for the next pass.
pub const ARTIST_FETCHES_PER_PASS: usize = 10;

/// How often the 15-minute loop reads the followed artists.
const ARTISTS_EVERY_MS: i64 = DAY_MS;

/// How often the 15-minute loop reads every saved album (for un-saves);
/// between, it stops at the first page it knows.
const ALBUMS_FULL_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The shortest gap between two "Sync now" runs of one part for one user.
pub const SYNC_NOW_GAP: Duration = Duration::from_secs(5 * 60);

/// The rate row whose wait is kept across a restart.
const COOLDOWN_SCOPE: &str = "spotify";

/// Why a library sync or a resolution stopped.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    /// Spotify, the link, or the app settings failed.
    #[error("spotify: {0:?}")]
    Import(ImportFailure),
    /// The link lacks the library scopes.
    #[error("reconnect Spotify to grant library access")]
    NeedsReconnect,
    /// MusicBrainz, Deezer or the catalogue failed.
    #[error("{0}")]
    Resolve(#[from] ResolveError),
    /// The database failed.
    #[error("store: {0}")]
    Store(String),
}

impl From<ImportFailure> for LibraryError {
    fn from(error: ImportFailure) -> Self {
        Self::Import(error)
    }
}

impl From<SpotifyError> for LibraryError {
    fn from(error: SpotifyError) -> Self {
        Self::Import(ImportFailure::Spotify(error))
    }
}

impl From<DbError> for LibraryError {
    fn from(error: DbError) -> Self {
        Self::Store(error.to_string())
    }
}

impl LibraryError {
    /// One line for the card.
    pub fn message(&self) -> String {
        match self {
            Self::Import(ImportFailure::Settings(_)) => {
                "Spotify app settings do not read".to_owned()
            }
            Self::Import(ImportFailure::Spotify(SpotifyError::NotLinked)) => {
                "Spotify account not linked".to_owned()
            }
            Self::Import(ImportFailure::Spotify(SpotifyError::RateLimited { retry_after })) => {
                rate_limited_message(
                    Duration::from_secs(retry_after.unwrap_or(0)),
                    droppedneedle_kernel::clock::Clock::now_ms(
                        &droppedneedle_kernel::clock::SystemClock,
                    ),
                )
            }
            Self::Import(ImportFailure::Spotify(SpotifyError::Unavailable(detail))) => {
                unavailable_message(detail)
            }
            Self::Import(ImportFailure::Spotify(SpotifyError::Busy)) => {
                "The database was busy".to_owned()
            }
            Self::Import(ImportFailure::Spotify(SpotifyError::Store(_))) => {
                "The sync could not be saved".to_owned()
            }
            Self::NeedsReconnect => {
                "Reconnect Spotify to enable Liked Songs, saved albums and followed artists"
                    .to_owned()
            }
            Self::Resolve(_) => "MusicBrainz or Deezer could not be reached".to_owned(),
            Self::Store(_) => "The database was busy".to_owned(),
        }
    }
}

/// What a followed-artist sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ArtistSync {
    pub read: usize,
    pub matched: usize,
    pub followed: usize,
    pub unfollowed: usize,
}

/// What a saved-album scan did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AlbumScan {
    pub read: usize,
    pub matched: usize,
    pub new: usize,
    pub removed: usize,
    pub favorited: usize,
}

#[derive(Debug, Clone)]
struct AlbumRow {
    spotify_album_id: String,
    title: String,
    artist_name: String,
    year: Option<i64>,
    cover_url: Option<String>,
    release_group: Option<String>,
    matched_by: Option<String>,
    matched_at: Option<i64>,
    reviewed: bool,
    favorite_album_id: Option<String>,
}

#[derive(Debug, Clone)]
struct ArtistRow {
    spotify_artist_id: String,
    artist_mbid: Option<String>,
    matched_at: Option<i64>,
    followed_by_sync: bool,
    attempts: i64,
    fetched_at: Option<i64>,
}

#[derive(Debug, Clone, Default)]
struct Settings {
    follow_sync: bool,
    album_sync: bool,
    artists_checked_at: Option<i64>,
    albums_checked_at: Option<i64>,
    last_error: Option<String>,
}

/// Which part of a library is running, per user, and when each last
/// started by hand or last read every saved album.
#[derive(Default)]
struct Running {
    artists: HashSet<String>,
    albums: HashSet<String>,
    started: HashMap<(String, bool), Instant>,
    full_scan: HashMap<String, Instant>,
}

/// The library sync and the link resolver.
pub struct SpotifyLibrary {
    service: Arc<SpotifyImportService>,
    lookups: Live,
    db: AcquireDb,
    follows: Follows,
    favorites: FavoriteStore,
    running: Mutex<Running>,
    /// The end of the wait last kept, epoch milliseconds.
    kept_until: Mutex<i64>,
    restored: AtomicBool,
}

/// An album's facts as Spotify gives them.
pub fn album_facts(album: &AlbumDetail) -> AlbumFacts {
    let codes = album.external_ids.as_ref();
    AlbumFacts {
        spotify_id: album.id.clone().unwrap_or_default(),
        title: album.name.clone(),
        artists: album
            .artists
            .iter()
            .map(|artist| artist.name.clone())
            .filter(|name| !name.trim().is_empty())
            .collect(),
        upc: codes
            .and_then(|codes| codes.upc.clone().or_else(|| codes.ean.clone()))
            .filter(|code| !code.trim().is_empty()),
        total_tracks: album.total_tracks,
    }
}

/// A song's facts as Spotify gives them.
pub fn track_facts(track: &EntryTrack) -> TrackFacts {
    let artists: Vec<String> = track
        .artists
        .iter()
        .filter_map(|artist| artist.name.clone())
        .filter(|name| !name.trim().is_empty())
        .collect();
    let album = track.album.as_ref();
    TrackFacts {
        title: track.name.clone(),
        album: AlbumFacts {
            spotify_id: album.and_then(|a| a.id.clone()).unwrap_or_default(),
            title: album.and_then(|a| a.name.clone()).unwrap_or_default(),
            artists: artists.clone(),
            upc: None,
            total_tracks: None,
        },
        artists,
        isrc: track
            .external_ids
            .as_ref()
            .and_then(|ids| ids.isrc.clone())
            .filter(|code| !code.trim().is_empty()),
        seconds: track
            .duration_ms
            .filter(|ms| *ms > 0)
            .map(|ms| u64::try_from((ms + 500) / 1000).unwrap_or(0)),
    }
}

/// Spotify's `added_at` (RFC 3339) as epoch milliseconds.
fn saved_ms(added_at: Option<&str>) -> Option<i64> {
    let parsed =
        time::OffsetDateTime::parse(added_at?, &time::format_description::well_known::Rfc3339)
            .ok()?;
    i64::try_from(parsed.unix_timestamp_nanos() / 1_000_000).ok()
}

fn year_of(date: Option<&str>) -> Option<i64> {
    date.and_then(|date| date.get(..4))
        .and_then(|year| year.parse().ok())
}

fn matched_by(found: &Found) -> String {
    match found.source {
        super::spotify_resolve::Source::MusicBrainz => found.via.as_str().to_owned(),
        super::spotify_resolve::Source::Deezer => format!("deezer_{}", found.via.as_str()),
    }
}

/// Whether an item is due a lookup: never looked up, or unmatched and
/// looked up over a week ago. A match is kept.
fn due(matched: bool, matched_at: Option<i64>, now: i64) -> bool {
    !matched && matched_at.is_none_or(|at| now - at >= RETRY_UNMATCHED_MS)
}

/// Whether an unmatched artist is due a lookup: never looked up, or its
/// wait after `attempts` lookups is over.
fn artist_due(matched_at: Option<i64>, attempts: i64, now: i64) -> bool {
    matched_at.is_none_or(|at| now - at >= artist_wait_ms(attempts))
}

/// What the saved albums say of an artist by name: the titles of its
/// albums, and the MusicBrainz release groups of those matched by proof
/// (barcode or link), for its credits.
fn saved_evidence(saved: &[AlbumRow], name: &str) -> (Vec<String>, Vec<String>) {
    let mut titles = Vec::new();
    let mut groups = Vec::new();
    for album in saved {
        if !album
            .artist_name
            .split(", ")
            .any(|credited| same_name(credited, name))
        {
            continue;
        }
        titles.push(album.title.clone());
        if let (Some(group), Some("barcode" | "link")) =
            (&album.release_group, album.matched_by.as_deref())
        {
            groups.push(group.clone());
        }
    }
    (titles, groups)
}

/// The follow changes a followed-artist sync makes, decided over the rows
/// as stored and the artists Spotify listed. Answers the artists to
/// unfollow here: those the sync followed, unfollowed at Spotify, that no
/// artist still followed there maps to.
pub fn unfollows(rows: &[(String, Option<String>, bool)], listed: &HashSet<String>) -> Vec<String> {
    let kept: HashSet<String> = rows
        .iter()
        .filter(|(id, _, _)| listed.contains(id))
        .filter_map(|(_, mbid, _)| mbid.as_ref().map(|m| m.to_lowercase()))
        .collect();
    let mut out: Vec<String> = rows
        .iter()
        .filter(|(id, _, by_sync)| *by_sync && !listed.contains(id))
        .filter_map(|(_, mbid, _)| mbid.clone())
        .filter(|mbid| !kept.contains(&mbid.to_lowercase()))
        .collect();
    out.sort();
    out.dedup();
    out
}

impl SpotifyLibrary {
    /// The library over the import service, the lookups and the stores.
    pub fn new(
        service: Arc<SpotifyImportService>,
        lookups: Live,
        db: AcquireDb,
        follows: Follows,
    ) -> Self {
        let favorites = FavoriteStore::new(CollectionsDb::new(db.db().clone(), db.clock().clone()));
        Self {
            service,
            lookups,
            db,
            follows,
            favorites,
            running: Mutex::new(Running::default()),
            kept_until: Mutex::new(0),
            restored: AtomicBool::new(false),
        }
    }

    /// How much of the wait Spotify asked for is left; zero when none runs.
    pub fn cooling_for(&self) -> Duration {
        self.service.client().cooling_for()
    }

    /// The card's line for the wait left now.
    fn cooling_message(&self) -> String {
        rate_limited_message(self.cooling_for(), self.now())
    }

    /// Keep the wait Spotify asked for, so a restart honours it. The first
    /// call after start restores a kept wait that has not ended yet. Cheap
    /// when nothing changed: the database is written only when the wait's
    /// end moved.
    pub async fn keep_cooldown(&self) {
        let now = self.now();
        if !self.restored.swap(true, Ordering::SeqCst) {
            let kept: Result<Option<i64>, DbError> = self
                .db
                .db()
                .read("spotify.cooldown.read", |conn| {
                    Ok(conn
                        .query_row(COOLDOWN_UNTIL.text(), params![COOLDOWN_SCOPE], |row| {
                            row.get(0)
                        })
                        .optional()?)
                })
                .await;
            match kept {
                Ok(Some(until)) if until > now => {
                    let wait = Duration::from_millis(u64::try_from(until - now).unwrap_or(0));
                    self.service.client().cool_down(wait);
                    *self.kept_until.lock().unwrap_or_else(|p| p.into_inner()) = until;
                    tracing::info!(
                        wait_secs = wait.as_secs(),
                        "spotify rate-limit wait kept from before the restart"
                    );
                    return;
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "spotify rate-limit wait unreadable"),
            }
        }
        let wait = self.cooling_for();
        if wait.is_zero() {
            return;
        }
        let until = now.saturating_add(i64::try_from(wait.as_millis()).unwrap_or(0));
        {
            let mut kept = self.kept_until.lock().unwrap_or_else(|p| p.into_inner());
            if (until - *kept).abs() < 5_000 {
                return;
            }
            *kept = until;
        }
        let done = self
            .db
            .write("spotify.cooldown.keep", move |tx| {
                tx.execute(KEEP_COOLDOWN.text(), params![COOLDOWN_SCOPE, until, now])?;
                Ok(())
            })
            .await;
        match done {
            Ok(()) => tracing::info!(wait_secs = wait.as_secs(), "spotify rate-limit wait kept"),
            Err(error) => tracing::warn!(%error, "spotify rate-limit wait not kept"),
        }
    }

    fn now(&self) -> i64 {
        self.db.clock().now_ms()
    }

    async fn settings(&self, user_id: &str) -> Result<Settings, DbError> {
        let user = user_id.to_owned();
        self.db
            .db()
            .read("spotify.library.settings", move |conn| {
                Ok(conn
                    .query_row(LIBRARY_ROW.text(), params![user], |row| {
                        Ok(Settings {
                            follow_sync: row.get::<_, i64>(0)? == 1,
                            album_sync: row.get::<_, i64>(1)? == 1,
                            artists_checked_at: row.get(2)?,
                            albums_checked_at: row.get(3)?,
                            last_error: row.get(4)?,
                        })
                    })
                    .optional()?
                    .unwrap_or_default())
            })
            .await
    }

    /// The card's view of one user's library. Spotify's totals are read
    /// live; when Spotify does not answer they are left out.
    pub async fn state(&self, user_id: &str) -> Result<SpotifyLibraryState, LibraryError> {
        let settings = self.settings(user_id).await?;
        let user = user_id.to_owned();
        let new_albums: i64 = self
            .db
            .db()
            .read("spotify.library.new_albums", move |conn| {
                Ok::<_, DbError>(
                    conn.query_row(NEW_ALBUMS.text(), params![user], |row| row.get(0))?,
                )
            })
            .await?;
        let (refresher, library) = self
            .service
            .access(user_id, RequestPriority::UserInitiated)
            .await?;
        let (mut followed, mut saved) = (None, None);
        if library {
            let client = self.service.client();
            followed = client.followed_artists_total(&refresher).await.ok();
            saved = client.saved_albums_total(&refresher).await.ok();
        }
        let running = self.running.lock().unwrap_or_else(|p| p.into_inner());
        Ok(SpotifyLibraryState {
            needs_reconnect: !library,
            follow_sync: settings.follow_sync,
            album_sync: settings.album_sync,
            followed_artists: followed,
            saved_albums: saved,
            new_albums,
            artists_checked_at: settings.artists_checked_at,
            albums_checked_at: settings.albums_checked_at,
            artists_syncing: running.artists.contains(user_id),
            albums_scanning: running.albums.contains(user_id),
            last_error: settings.last_error,
        })
    }

    /// Save one user's toggles.
    pub async fn set(
        &self,
        user_id: &str,
        follow_sync: bool,
        album_sync: bool,
    ) -> Result<(), LibraryError> {
        let (user, now) = (user_id.to_owned(), self.now());
        self.db
            .write("spotify.library.set", move |tx| {
                tx.execute(
                    SET_LIBRARY.text(),
                    params![user, i64::from(follow_sync), i64::from(album_sync), now],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn record(&self, user_id: &str, what: &'static str, error: Option<String>) {
        let (user, now) = (user_id.to_owned(), self.now());
        let done = self
            .db
            .write("spotify.library.record", move |tx| {
                tx.execute(ENSURE_LIBRARY.text(), params![user, now])?;
                match (what, &error) {
                    (_, Some(message)) => {
                        tx.execute(LIBRARY_FAILED.text(), params![user, message])?
                    }
                    ("artists", None) => {
                        tx.execute(ARTISTS_CHECKED.text(), params![user, now, error])?
                    }
                    _ => tx.execute(ALBUMS_CHECKED.text(), params![user, now, error])?,
                };
                Ok(())
            })
            .await;
        if let Err(error) = done {
            tracing::warn!(%error, "spotify library check not recorded");
        }
    }

    /// Mark a part running for a user; `false` when it already is.
    fn start(&self, user_id: &str, albums: bool) -> bool {
        let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
        let set = if albums {
            &mut running.albums
        } else {
            &mut running.artists
        };
        set.insert(user_id.to_owned())
    }

    fn finish(&self, user_id: &str, albums: bool) {
        let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
        if albums {
            running.albums.remove(user_id);
        } else {
            running.artists.remove(user_id);
        }
    }

    /// Run one part for one user in the background ("Sync now"). Answers
    /// `false` when it is already running.
    /// Answers `false` when it is already running, ran within
    /// [`SYNC_NOW_GAP`], or Spotify asked for a wait that has not ended
    /// (the card then says until when).
    pub fn spawn(self: &Arc<Self>, user_id: &str, albums: bool) -> bool {
        let what = if albums { "albums" } else { "artists" };
        if !self.cooling_for().is_zero() {
            let (library, user, message) =
                (self.clone(), user_id.to_owned(), self.cooling_message());
            tokio::spawn(async move { library.record(&user, what, Some(message)).await });
            return false;
        }
        {
            let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
            let key = (user_id.to_owned(), albums);
            if running
                .started
                .get(&key)
                .is_some_and(|at| at.elapsed() < SYNC_NOW_GAP)
            {
                tracing::info!(user_id, what, "spotify sync now ran recently; not started");
                return false;
            }
            running.started.insert(key, Instant::now());
        }
        if !self.start(user_id, albums) {
            return false;
        }
        let (library, user) = (self.clone(), user_id.to_owned());
        tokio::spawn(async move {
            library
                .run_part(&user, albums, RequestPriority::UserInitiated, true)
                .await;
            library.finish(&user, albums);
        });
        true
    }

    async fn run_part(&self, user_id: &str, albums: bool, priority: RequestPriority, full: bool) {
        let what = if albums { "albums" } else { "artists" };
        let outcome = if albums {
            self.scan_albums(user_id, priority, full).await.map(|scan| {
                tracing::info!(
                    user_id,
                    read = scan.read,
                    matched = scan.matched,
                    new = scan.new,
                    removed = scan.removed,
                    favorited = scan.favorited,
                    "spotify saved albums scanned"
                );
            })
        } else {
            self.sync_artists(user_id, priority).await.map(|sync| {
                tracing::info!(
                    user_id,
                    read = sync.read,
                    matched = sync.matched,
                    followed = sync.followed,
                    unfollowed = sync.unfollowed,
                    "spotify followed artists synced"
                );
            })
        };
        match outcome {
            Ok(()) => {
                if albums && full {
                    self.running
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .full_scan
                        .insert(user_id.to_owned(), Instant::now());
                }
                self.record(user_id, what, None).await;
            }
            Err(error) => {
                tracing::warn!(user_id, %error, what, "spotify library sync failed");
                let message = if matches!(
                    error,
                    LibraryError::Import(ImportFailure::Spotify(SpotifyError::RateLimited { .. }))
                ) && !self.cooling_for().is_zero()
                {
                    self.keep_cooldown().await;
                    self.cooling_message()
                } else {
                    error.message()
                };
                self.record(user_id, what, Some(message)).await;
            }
        }
    }

    /// One pass for every user with a sync on (the 15-minute loop).
    ///
    /// Followed artists are read once a day, or sooner while a sync cut
    /// short left artists never looked up; saved albums every pass, every
    /// page once a day. A wait Spotify asked for ends the pass.
    pub async fn run_once(&self) {
        type User = (String, bool, bool, Option<i64>, bool);
        let users: Vec<User> = match self
            .db
            .db()
            .read("spotify.library.users", |conn| {
                let mut statement = conn.prepare(LIBRARY_USERS.text())?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)? == 1,
                        row.get::<_, i64>(2)? == 1,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, i64>(4)? == 1,
                    ))
                })?;
                Ok::<_, DbError>(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
        {
            Ok(users) => users,
            Err(error) => {
                tracing::warn!(%error, "spotify library settings unreadable");
                return;
            }
        };
        let now = self.now();
        for (user, follow_sync, album_sync, artists_checked_at, pending) in users {
            let artists_due =
                pending || artists_checked_at.is_none_or(|at| now - at >= ARTISTS_EVERY_MS);
            let full = self
                .running
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .full_scan
                .get(&user)
                .is_none_or(|at| at.elapsed() >= ALBUMS_FULL_EVERY);
            for (on, albums) in [(follow_sync && artists_due, false), (album_sync, true)] {
                if !self.cooling_for().is_zero() {
                    tracing::info!(
                        wait_secs = self.cooling_for().as_secs(),
                        "spotify asked for a wait; library sync pass ends"
                    );
                    self.keep_cooldown().await;
                    return;
                }
                if on && self.start(&user, albums) {
                    self.run_part(&user, albums, RequestPriority::BackgroundSync, full)
                        .await;
                    self.finish(&user, albums);
                }
            }
        }
    }

    async fn artist_rows(&self, user_id: &str) -> Result<Vec<ArtistRow>, DbError> {
        let user = user_id.to_owned();
        self.db
            .db()
            .read("spotify.library.artists", move |conn| {
                let mut statement = conn.prepare(FOLLOWED.text())?;
                let rows = statement.query_map(params![user], |row| {
                    Ok(ArtistRow {
                        spotify_artist_id: row.get(0)?,
                        artist_mbid: row.get(2)?,
                        matched_at: row.get(3)?,
                        followed_by_sync: row.get::<_, i64>(4)? == 1,
                        attempts: row.get(5)?,
                        fetched_at: row.get(6)?,
                    })
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// Follow the artists followed at Spotify and unfollow the ones the
    /// sync followed and Spotify no longer lists.
    pub async fn sync_artists(
        &self,
        user_id: &str,
        priority: RequestPriority,
    ) -> Result<ArtistSync, LibraryError> {
        let (refresher, library) = self.service.access(user_id, priority).await?;
        if !library {
            return Err(LibraryError::NeedsReconnect);
        }
        let client = self.service.client();
        let listed: Vec<SpotifyArtist> = client.followed_artists(&refresher).await?;
        let stored: HashMap<String, ArtistRow> = self
            .artist_rows(user_id)
            .await?
            .into_iter()
            .map(|row| (row.spotify_artist_id.clone(), row))
            .collect();
        let lookups = self.lookups.at(priority);
        let mut out = ArtistSync {
            read: listed.len(),
            ..ArtistSync::default()
        };
        // What the saved albums already say of each artist: titles for the
        // overlap check, and the release groups proved by barcode or link.
        let saved = self.album_rows(user_id).await?;
        let listed_ids: HashSet<String> = listed.iter().filter_map(|a| a.id.clone()).collect();
        let mut fetches = 0usize;
        for artist in &listed {
            let Some(id) = artist.id.clone() else {
                continue;
            };
            let (user, name, now) = (user_id.to_owned(), artist.name.clone(), self.now());
            {
                let (user, id, name) = (user.clone(), id.clone(), name.clone());
                self.db
                    .write("spotify.library.artist", move |tx| {
                        tx.execute(UPSERT_ARTIST.text(), params![user, id, name, now])?;
                        Ok(())
                    })
                    .await?;
            }
            let row = stored.get(&id);
            if row.is_some_and(|row| row.artist_mbid.is_some()) {
                out.matched += 1;
                continue;
            }
            let attempts = row.map_or(0, |row| row.attempts);
            if !artist_due(row.and_then(|row| row.matched_at), attempts, now) {
                continue;
            }
            let (album_titles, album_groups) = saved_evidence(&saved, &name);
            let mut facts = ArtistFacts {
                spotify_id: id.clone(),
                name: name.clone(),
                album_titles,
                album_groups,
            };
            // Link, saved albums and name first: none of them calls Spotify.
            let mut found = match resolve_artist(&facts, &lookups).await {
                Ok(found) => found,
                Err(error) => {
                    tracing::warn!(user = %user_id, artist = %id, error = %error, "spotify artist match failed; skipped");
                    self.attempted(user_id, &id, now, None).await?;
                    continue;
                }
            };
            let mut fetched = None;
            if found.is_none()
                && row
                    .and_then(|row| row.fetched_at)
                    .is_none_or(|at| now - at >= REFETCH_MS)
            {
                if fetches >= ARTIST_FETCHES_PER_PASS {
                    // Left for the next pass, unrecorded, so it stays first.
                    continue;
                }
                fetches += 1;
                fetched = Some(now);
                // Recorded before the call: a lookup a rate limit cuts short
                // waits its turn instead of going first next time.
                self.attempted(user_id, &id, now, fetched).await?;
                let titles = match client.artist_album_titles(&refresher, &id).await {
                    Ok(titles) => titles,
                    // Slow down or a lost link stops the whole run; the next
                    // pass picks up where this one stopped.
                    Err(error @ (SpotifyError::RateLimited { .. } | SpotifyError::NotLinked)) => {
                        return Err(error.into());
                    }
                    Err(error) => {
                        tracing::warn!(user = %user_id, artist = %id, error = ?error, "spotify artist albums failed; skipped");
                        continue;
                    }
                };
                let known: HashSet<String> = facts.album_titles.iter().cloned().collect();
                facts
                    .album_titles
                    .extend(titles.into_iter().filter(|title| !known.contains(title)));
                found = match resolve_artist_named(&facts, &lookups).await {
                    Ok(found) => found,
                    Err(error) => {
                        tracing::warn!(user = %user_id, artist = %id, error = %error, "spotify artist match failed; skipped");
                        continue;
                    }
                };
            }
            let Some(found) = found else {
                if fetched.is_none() {
                    self.attempted(user_id, &id, now, None).await?;
                }
                continue;
            };
            out.matched += 1;
            let mut by_sync = false;
            let already = self
                .follows
                .followed_among(user_id, std::slice::from_ref(&found.id))
                .await
                .map_err(|error| LibraryError::Store(format!("{error:?}")))?;
            if already.is_empty() {
                self.follows
                    .set_follow(user_id, &found.id, true, Some(name.clone()))
                    .await
                    .map_err(|error| LibraryError::Store(format!("{error:?}")))?;
                by_sync = true;
                out.followed += 1;
            }
            let (mbid, via) = (Some(found.id.clone()), Some(matched_by(&found)));
            self.db
                .write("spotify.library.artist_match", move |tx| {
                    tx.execute(
                        MATCH_ARTIST.text(),
                        params![user, id, mbid, via, now, i64::from(by_sync)],
                    )?;
                    Ok(())
                })
                .await?;
        }
        // Unfollowed at Spotify.
        let rows: Vec<(String, Option<String>, bool)> = stored
            .values()
            .map(|row| {
                (
                    row.spotify_artist_id.clone(),
                    row.artist_mbid.clone(),
                    row.followed_by_sync,
                )
            })
            .collect();
        for mbid in unfollows(&rows, &listed_ids) {
            self.follows
                .set_follow(user_id, &mbid, false, None)
                .await
                .map_err(|error| LibraryError::Store(format!("{error:?}")))?;
            out.unfollowed += 1;
        }
        for gone in stored.keys().filter(|id| !listed_ids.contains(*id)) {
            let (user, gone) = (user_id.to_owned(), gone.clone());
            self.db
                .write("spotify.library.artist_drop", move |tx| {
                    tx.execute(DROP_ARTIST.text(), params![user, gone])?;
                    Ok(())
                })
                .await?;
        }
        Ok(out)
    }

    /// Record one artist lookup that matched nothing (yet); `fetched` when
    /// it reads the artist's albums at Spotify.
    async fn attempted(
        &self,
        user_id: &str,
        id: &str,
        now: i64,
        fetched: Option<i64>,
    ) -> Result<(), DbError> {
        let (user, id) = (user_id.to_owned(), id.to_owned());
        self.db
            .write("spotify.library.artist_attempt", move |tx| {
                tx.execute(ATTEMPT_ARTIST.text(), params![user, id, now, fetched])?;
                Ok(())
            })
            .await
    }

    async fn album_rows(&self, user_id: &str) -> Result<Vec<AlbumRow>, DbError> {
        let user = user_id.to_owned();
        self.db
            .db()
            .read("spotify.library.albums", move |conn| {
                let mut statement = conn.prepare(SAVED_ALBUMS.text())?;
                let rows = statement.query_map(params![user], |row| {
                    Ok(AlbumRow {
                        spotify_album_id: row.get(0)?,
                        title: row.get(1)?,
                        artist_name: row.get(2)?,
                        year: row.get(3)?,
                        cover_url: row.get(4)?,
                        release_group: row.get(5)?,
                        matched_by: row.get(6)?,
                        matched_at: row.get(7)?,
                        reviewed: row.get::<_, i64>(8)? == 1,
                        favorite_album_id: row.get(9)?,
                    })
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// The library album holding a release group, if any.
    async fn held(&self, group: &str) -> Result<Option<String>, DbError> {
        let group = group.to_owned();
        self.db
            .db()
            .read("spotify.library.held", move |conn| {
                Ok(copy_for_group(conn, &group)?.map(|copy| copy.album_id))
            })
            .await
    }

    async fn favorite_ids(&self, user_id: &str) -> Result<HashSet<String>, LibraryError> {
        Ok(self
            .favorites
            .list(user_id, Some("album"))
            .await
            .map_err(|error| LibraryError::Store(format!("{error:?}")))?
            .into_iter()
            .map(|row| row.item_id)
            .collect())
    }

    /// Read the saved albums again: match new ones, flag them for review,
    /// favourite held matches and forget un-saved ones.
    ///
    /// Without `full` the read stops at the first page Spotify lists that
    /// holds only albums seen before, and nothing is forgotten; with it
    /// every page is read and un-saved albums go.
    pub async fn scan_albums(
        &self,
        user_id: &str,
        priority: RequestPriority,
        full: bool,
    ) -> Result<AlbumScan, LibraryError> {
        let (refresher, library) = self.service.access(user_id, priority).await?;
        if !library {
            return Err(LibraryError::NeedsReconnect);
        }
        let settings = self.settings(user_id).await?;
        let stored: HashMap<String, AlbumRow> = self
            .album_rows(user_id)
            .await?
            .into_iter()
            .map(|row| (row.spotify_album_id.clone(), row))
            .collect();
        let full = full || stored.is_empty();
        let known: HashSet<String> = stored.keys().cloned().collect();
        let saved = self
            .service
            .client()
            .saved_albums(&refresher, (!full).then_some(&known))
            .await?;
        // The first scan is the baseline: nothing in it is new.
        let baseline = stored.is_empty() && settings.albums_checked_at.is_none();
        let lookups = self.lookups.at(priority);
        let mut favorites = self.favorite_ids(user_id).await?;
        let mut out = AlbumScan {
            read: saved.len(),
            ..AlbumScan::default()
        };
        let mut listed: HashSet<String> = HashSet::new();
        for item in &saved {
            let album = &item.album;
            let Some(id) = album.id.clone().filter(|id| !id.is_empty()) else {
                continue;
            };
            listed.insert(id.clone());
            let facts = album_facts(album);
            let now = self.now();
            let row = stored.get(&id);
            if row.is_none() && !baseline {
                out.new += 1;
            }
            {
                let user = user_id.to_owned();
                let (id, facts) = (id.clone(), facts.clone());
                let year = year_of(album.release_date.as_deref());
                let cover = best_image_url(album.images.as_deref(), 300);
                let saved_at = saved_ms(item.added_at.as_deref());
                let reviewed = i64::from(baseline);
                self.db
                    .write("spotify.library.album", move |tx| {
                        tx.execute(
                            UPSERT_ALBUM.text(),
                            params![
                                user,
                                id,
                                facts.title,
                                facts.artists.join(", "),
                                year,
                                cover,
                                facts.total_tracks,
                                facts.upc,
                                saved_at,
                                now,
                                reviewed
                            ],
                        )?;
                        Ok(())
                    })
                    .await?;
            }
            let mut group = row.and_then(|row| row.release_group.clone());
            if due(group.is_some(), row.and_then(|row| row.matched_at), now) {
                let found = resolve_album(&facts, &lookups).await?;
                group = found.as_ref().map(|found| found.id.clone());
                let (user, id) = (user_id.to_owned(), id.clone());
                let (rg, via) = (group.clone(), found.as_ref().map(matched_by));
                self.db
                    .write("spotify.library.album_match", move |tx| {
                        tx.execute(MATCH_ALBUM.text(), params![user, id, rg, via, now])?;
                        Ok(())
                    })
                    .await?;
            }
            let Some(group) = group else { continue };
            out.matched += 1;
            if row.is_some_and(|row| row.favorite_album_id.is_some()) {
                continue;
            }
            // Favourite a held match, unless it already is one.
            if let Some(album_id) = self.held(&group).await?
                && !favorites.contains(&album_id)
            {
                let name = format!("{} - {}", facts.artists.join(", "), facts.title);
                self.favorites
                    .apply(
                        user_id,
                        &[("album".to_owned(), album_id.clone(), Some(name))],
                        true,
                    )
                    .await
                    .map_err(|error| LibraryError::Store(format!("{error:?}")))?;
                favorites.insert(album_id.clone());
                out.favorited += 1;
                let (user, id) = (user_id.to_owned(), id.clone());
                self.db
                    .write("spotify.library.album_favorite", move |tx| {
                        tx.execute(FAVORITED_ALBUM.text(), params![user, id, album_id])?;
                        Ok(())
                    })
                    .await?;
            }
        }
        // Un-saved at Spotify: only the favourite the sync added goes. A
        // read that stopped early lists only the newest, so it forgets
        // nothing.
        for row in stored
            .values()
            .filter(|_| full)
            .filter(|row| !listed.contains(&row.spotify_album_id))
        {
            if let Some(album_id) = &row.favorite_album_id {
                self.favorites
                    .apply(
                        user_id,
                        &[("album".to_owned(), album_id.clone(), None)],
                        false,
                    )
                    .await
                    .map_err(|error| LibraryError::Store(format!("{error:?}")))?;
            }
            let (user, id) = (user_id.to_owned(), row.spotify_album_id.clone());
            self.db
                .write("spotify.library.album_drop", move |tx| {
                    tx.execute(DROP_ALBUM.text(), params![user, id])?;
                    Ok(())
                })
                .await?;
            out.removed += 1;
        }
        Ok(out)
    }

    /// The saved albums for the review.
    pub async fn albums(&self, user_id: &str) -> Result<SpotifySavedAlbumList, LibraryError> {
        let rows = self.album_rows(user_id).await?;
        let settings = self.settings(user_id).await?;
        let groups: Vec<String> = rows
            .iter()
            .filter_map(|r| r.release_group.clone())
            .collect();
        let state = self
            .db
            .db()
            .read("spotify.library.album_state", move |conn| {
                let mut state: HashMap<String, (bool, bool)> = HashMap::new();
                let mut requested = conn.prepare_cached(REQUESTED.text())?;
                for group in groups {
                    let held = copy_for_group(conn, &group)?.is_some();
                    let asked = requested.exists(params![group.to_lowercase()])?;
                    state.insert(group, (held, asked));
                }
                Ok::<_, DbError>(state)
            })
            .await?;
        let albums = rows
            .into_iter()
            .map(|row| {
                let (held, requested) = row
                    .release_group
                    .as_ref()
                    .and_then(|group| state.get(group).copied())
                    .unwrap_or_default();
                SpotifySavedAlbum {
                    spotify_album_id: row.spotify_album_id,
                    title: row.title,
                    artist_name: row.artist_name,
                    year: row.year,
                    cover_url: row.cover_url,
                    release_group: row.release_group,
                    matched_by: row.matched_by,
                    is_new: !row.reviewed,
                    held,
                    requested,
                }
            })
            .collect();
        let scanning = self
            .running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .albums
            .contains(user_id);
        Ok(SpotifySavedAlbumList {
            albums,
            scanning,
            checked_at: settings.albums_checked_at,
        })
    }

    /// Confirm the review: no album is new any more.
    pub async fn mark_reviewed(&self, user_id: &str) -> Result<(), LibraryError> {
        let user = user_id.to_owned();
        self.db
            .write("spotify.library.reviewed", move |tx| {
                tx.execute(REVIEW_ALBUMS.text(), params![user])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// What a pasted Spotify link names here. With the caller's Spotify
    /// link the item's details come from Spotify; without one only its
    /// name does (oEmbed). `None` when the text is not such a link.
    pub async fn resolve_link(
        &self,
        user_id: &str,
        input: &str,
    ) -> Result<Option<SpotifyResolution>, LibraryError> {
        let Some(link) = parse_link(input) else {
            return Ok(None);
        };
        let lookups = self.lookups.at(RequestPriority::UserInitiated);
        let access = match self
            .service
            .access(user_id, RequestPriority::UserInitiated)
            .await
        {
            Ok((refresher, _)) => Some(refresher),
            Err(ImportFailure::Spotify(SpotifyError::NotLinked) | ImportFailure::Settings(_)) => {
                None
            }
            Err(error) => return Err(error.into()),
        };
        let client = self.service.client();
        let mut out = SpotifyResolution {
            kind: link.kind.as_str().to_owned(),
            found: false,
            name: String::new(),
            artist: None,
            album_id: None,
            artist_id: None,
            recording_id: None,
            source: None,
            via: None,
        };
        let found = match (&access, link.kind) {
            (Some(refresher), LinkKind::Track) => {
                let track = client.track(refresher, &link.id).await?;
                let facts = track_facts(&track);
                out.name = facts.title.clone();
                out.artist = facts.artists.first().cloned();
                resolve_track(&facts, &lookups).await?.map(|hit| {
                    out.recording_id = Some(hit.recording);
                    hit.album
                })
            }
            (Some(refresher), LinkKind::Album) => {
                let album = client.album(refresher, &link.id).await?;
                let facts = album_facts(&album);
                out.name = facts.title.clone();
                out.artist = facts.artists.first().cloned();
                resolve_album(&facts, &lookups).await?
            }
            (Some(refresher), LinkKind::Artist) => {
                let artist = client.artist(refresher, &link.id).await?;
                // The albums only back a name match; without them the
                // match is just stricter.
                let titles = client
                    .artist_album_titles(refresher, &link.id)
                    .await
                    .unwrap_or_default();
                out.name = artist.name.clone();
                let facts = ArtistFacts {
                    spotify_id: link.id.clone(),
                    name: artist.name,
                    album_titles: titles,
                    album_groups: Vec::new(),
                };
                resolve_artist(&facts, &lookups).await?
            }
            (None, kind) => {
                out.name = client.oembed_title(&link.url()).await?;
                match kind {
                    // A song's name alone is never enough.
                    LinkKind::Track => None,
                    LinkKind::Album => {
                        let facts = AlbumFacts {
                            spotify_id: link.id.clone(),
                            title: out.name.clone(),
                            ..AlbumFacts::default()
                        };
                        resolve_album(&facts, &lookups).await?
                    }
                    LinkKind::Artist => {
                        let facts = ArtistFacts {
                            spotify_id: link.id.clone(),
                            name: out.name.clone(),
                            album_titles: Vec::new(),
                            album_groups: Vec::new(),
                        };
                        resolve_artist(&facts, &lookups).await?
                    }
                }
            }
        };
        if let Some(found) = found {
            out.found = true;
            out.source = Some(found.source.as_str().to_owned());
            out.via = Some(found.via.as_str().to_owned());
            if link.kind == LinkKind::Artist {
                out.artist_id = Some(found.id);
            } else {
                out.album_id = Some(found.id);
            }
        }
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn only_sync_made_follows_are_removed() {
        let rows = vec![
            ("gone-sync".to_owned(), Some("A".to_owned()), true),
            ("gone-hand".to_owned(), Some("B".to_owned()), false),
            ("kept".to_owned(), Some("C".to_owned()), true),
            ("gone-unmatched".to_owned(), None, true),
        ];
        assert_eq!(unfollows(&rows, &set(&["kept"])), vec!["A".to_owned()]);
    }

    #[test]
    fn a_follow_another_spotify_artist_maps_to_stays() {
        let rows = vec![
            ("old".to_owned(), Some("A".to_owned()), true),
            ("alias".to_owned(), Some("a".to_owned()), false),
        ];
        assert!(unfollows(&rows, &set(&["alias"])).is_empty());
    }

    #[test]
    fn unmatched_items_wait_a_week() {
        let week = RETRY_UNMATCHED_MS;
        assert!(due(false, None, 0));
        assert!(!due(false, Some(10), 10 + week - 1));
        assert!(due(false, Some(10), 10 + week));
        assert!(!due(true, None, 0));
    }

    #[test]
    fn unmatched_artists_wait_longer_each_time() {
        assert!(artist_due(None, 0, 0));
        assert!(!artist_due(Some(0), 1, DAY_MS - 1));
        assert!(artist_due(Some(0), 1, DAY_MS));
        assert!(!artist_due(Some(0), 2, 7 * DAY_MS - 1));
        assert!(artist_due(Some(0), 2, 7 * DAY_MS));
        assert!(!artist_due(Some(0), 5, 30 * DAY_MS - 1));
        assert!(artist_due(Some(0), 5, 30 * DAY_MS));
    }

    #[test]
    fn saved_albums_lend_titles_and_proved_groups() {
        let album = |title: &str, artist: &str, group: Option<&str>, by: Option<&str>| AlbumRow {
            spotify_album_id: title.to_owned(),
            title: title.to_owned(),
            artist_name: artist.to_owned(),
            year: None,
            cover_url: None,
            release_group: group.map(str::to_owned),
            matched_by: by.map(str::to_owned),
            matched_at: None,
            reviewed: true,
            favorite_album_id: None,
        };
        let saved = vec![
            album("Kid A", "Radiohead", Some("g1"), Some("barcode")),
            album("Amnesiac", "Radiohead, Someone", Some("g2"), Some("name")),
            album("Other", "Someone Else", Some("g3"), Some("link")),
        ];
        let (titles, groups) = saved_evidence(&saved, "radiohead");
        assert_eq!(titles, vec!["Kid A".to_owned(), "Amnesiac".to_owned()]);
        assert_eq!(groups, vec!["g1".to_owned()]);
    }

    #[test]
    fn a_year_reads_from_any_precision() {
        assert_eq!(year_of(Some("2007-10-10")), Some(2007));
        assert_eq!(year_of(Some("2007")), Some(2007));
        assert_eq!(year_of(None), None);
    }
}
