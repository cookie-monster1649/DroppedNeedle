//! Fork: the public-playlist match. The folders of a playlist-folder
//! import are often exports of Spotify playlists that still exist
//! publicly. After the normal resolver, each file it left (nothing found,
//! or never looked up) is matched to an entry of its folder's Spotify
//! playlist by title, primary artist and length, and the entry's ISRC, or
//! else its Spotify album, resolves it.
//!
//! - `PLAYLIST_IMPORT_SPOTIFY_USER` names the Spotify user whose public
//!   playlists are read; `PLAYLIST_IMPORT_SPOTIFY_PLAYLISTS` (playlist
//!   links or ids, comma or space separated) names them instead. Unset
//!   means off.
//! - The calls use the Spotify link of an admin, through the import's
//!   Spotify client: its pacing, refusal classification, growing wait,
//!   usage counter and kept cooldown. A wait Spotify asked for is waited
//!   out, never skipped.
//! - Playlists map to folders by name: case-insensitive, punctuation
//!   folded, exact. The rest is listed in the report.
//! - The first call is the probe: a 403 or 404 there (a development-mode
//!   app may not read another user's playlists) ends the step with a note
//!   in the report; the run goes on.
//! - The listing and each playlist's entries are kept in the state with
//!   the playlist's snapshot; a run within [`LISTING_FRESH_SECS`] of the
//!   last listing calls nothing, and a playlist whose snapshot did not
//!   change is not read again.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::resolve::{self, Resolved, Via};
use super::{FileState, PlaylistImport, Resolution, State, Tags};
use crate::imports::spotify::SpotifyImportService;
use crate::imports::spotify_client::{SpotifyError, TokenRefresher, is_refused};
use crate::imports::spotify_library::SpotifyLibrary;
use crate::imports::spotify_resolve::{AlbumFacts, Lookups, Source as FoundSource, TrackFacts};
use crate::stand_ins::StandInCheck;
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::db::fold::fold_text;
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::spotify::MATCH_ITEM_FIELDS;
use droppedneedle_providers::spotify::models::EntryTrack;

/// The Spotify user whose public playlists are read.
pub const USER_ENV: &str = "PLAYLIST_IMPORT_SPOTIFY_USER";
/// Playlist links or ids to read instead.
pub const PLAYLISTS_ENV: &str = "PLAYLIST_IMPORT_SPOTIFY_PLAYLISTS";
/// A listing younger than this is used as it is: no call at all.
pub const LISTING_FRESH_SECS: i64 = 24 * 60 * 60;
/// The most a length may differ, in seconds.
pub const LENGTH_SLACK: f64 = 3.0;
/// The note a refused probe leaves.
pub const REFUSED_NOTE: &str = "Spotify refused to read these playlists for this app \
(development mode): it needs the owner to sign in";

/// Which playlists are read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicSource {
    /// Every public playlist of this Spotify user.
    User(String),
    /// These playlist ids.
    Playlists(Vec<String>),
}

impl PublicSource {
    /// The source the environment names, if any.
    pub fn from_env() -> Option<Self> {
        let read = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        Self::parse(read(USER_ENV).as_deref(), read(PLAYLISTS_ENV).as_deref())
    }

    /// The source of a user id and a playlist list; the list wins.
    pub fn parse(user: Option<&str>, playlists: Option<&str>) -> Option<Self> {
        if let Some(list) = playlists {
            let ids: Vec<String> = list
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter_map(playlist_id_of)
                .collect();
            if !ids.is_empty() {
                return Some(Self::Playlists(ids));
            }
        }
        let user = user?.trim();
        let user = user
            .strip_prefix("spotify:user:")
            .or_else(|| {
                user.split("/user/")
                    .nth(1)
                    .map(|rest| rest.split(['/', '?']).next().unwrap_or(rest))
            })
            .unwrap_or(user);
        (!user.is_empty()).then(|| Self::User(user.to_owned()))
    }

    /// A stable key, so a changed setting reads the playlists again.
    pub fn key(&self) -> String {
        match self {
            Self::User(user) => format!("user:{user}"),
            Self::Playlists(ids) => format!("playlists:{}", ids.join(",")),
        }
    }
}

/// The playlist id in a link (`open.spotify.com/playlist/ID?si=…`), a
/// URI (`spotify:playlist:ID`) or a bare id.
pub fn playlist_id_of(value: &str) -> Option<String> {
    let value = value.trim();
    let id = value
        .strip_prefix("spotify:playlist:")
        .or_else(|| {
            value
                .split("/playlist/")
                .nth(1)
                .map(|rest| rest.split(['/', '?', '#']).next().unwrap_or(rest))
        })
        .unwrap_or(value);
    (!id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| id.to_owned())
}

/// One playlist entry, as kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub album_id: Option<String>,
    pub seconds: Option<f64>,
    pub isrc: Option<String>,
}

impl Item {
    /// The entry of a Spotify track.
    pub fn of(track: &EntryTrack) -> Self {
        Self {
            id: track.id.clone().unwrap_or_default(),
            title: track.name.clone(),
            artists: track
                .artists
                .iter()
                .filter_map(|artist| artist.name.clone())
                .filter(|name| !name.trim().is_empty())
                .collect(),
            album: track
                .album
                .as_ref()
                .and_then(|album| album.name.clone())
                .unwrap_or_default(),
            album_id: track
                .album
                .as_ref()
                .and_then(|album| album.id.clone())
                .filter(|id| !id.is_empty()),
            seconds: track.duration_ms.map(|ms| ms as f64 / 1000.0),
            isrc: track
                .external_ids
                .as_ref()
                .and_then(|ids| ids.isrc.as_deref())
                .map(|code| code.replace('-', "").trim().to_ascii_uppercase())
                .filter(|code| code.len() == 12),
        }
    }
}

/// One Spotify playlist, as kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cached {
    pub name: String,
    pub snapshot_id: Option<String>,
    /// The folder it maps to.
    pub folder: Option<String>,
    /// The entries, read at `items_snapshot`.
    pub items: Option<Vec<Item>>,
    pub items_snapshot: Option<String>,
}

/// What the step knows of Spotify, kept in the state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalogue {
    /// [`PublicSource::key`] of the setting it was read under.
    pub source: String,
    /// When the playlists were last listed (seconds since the epoch).
    pub listed_at: i64,
    /// By Spotify playlist id.
    pub playlists: BTreeMap<String, Cached>,
    /// Why the step stopped, when it did (shown in the report).
    pub note: Option<String>,
    /// Spotify calls the last step made.
    pub calls: u64,
}

impl Catalogue {
    /// The Spotify ids of the playlists mapped to a folder.
    pub fn mapped_ids(&self) -> BTreeSet<String> {
        self.playlists
            .iter()
            .filter(|(_, cached)| cached.folder.is_some())
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// A file matched to a playlist entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistMatch {
    pub playlist_id: String,
    pub track_id: String,
    /// Matched in its own folder's playlist (else across all of them).
    pub same_folder: bool,
    /// The entry was looked up (found or not), so a later run does not ask
    /// again for the same entry.
    pub looked_up: bool,
}

/// A folder or playlist name, folded for the mapping: case-insensitive,
/// accents and punctuation gone, spaces gone.
pub fn fold_name(name: &str) -> String {
    fold_text(name)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Map each playlist to the folder of the same folded name. Answers the
/// folders no playlist maps to. A name two playlists share maps neither.
pub fn map_folders(
    playlists: &mut BTreeMap<String, Cached>,
    folders: &BTreeSet<String>,
) -> Vec<String> {
    let mut by_name: HashMap<String, Vec<&String>> = HashMap::new();
    for folder in folders {
        by_name.entry(fold_name(folder)).or_default().push(folder);
    }
    let mut taken: HashMap<String, usize> = HashMap::new();
    for cached in playlists.values() {
        *taken.entry(fold_name(&cached.name)).or_default() += 1;
    }
    let mut mapped = BTreeSet::new();
    for cached in playlists.values_mut() {
        let key = fold_name(&cached.name);
        cached.folder = match by_name.get(&key) {
            Some(found) if found.len() == 1 && taken.get(&key) == Some(&1) => {
                mapped.insert(found[0].clone());
                Some(found[0].clone())
            }
            _ => None,
        };
    }
    folders
        .iter()
        .filter(|folder| !mapped.contains(*folder))
        .cloned()
        .collect()
}

/// Words that mark a bracketed or dashed part of a title as a version
/// note rather than part of the name.
const VERSION_NOTES: [&str; 14] = [
    "original mix",
    "original version",
    "extended mix",
    "extended version",
    "radio edit",
    "radio mix",
    "radio version",
    "album version",
    "single version",
    "explicit",
    "clean",
    "mono",
    "stereo",
    "edit",
];

fn is_feat(folded: &str) -> bool {
    ["feat ", "feat. ", "ft ", "ft. ", "featuring ", "with "]
        .iter()
        .any(|lead| folded.starts_with(lead))
}

fn is_note(part: &str) -> bool {
    let folded = fold_text(part);
    let folded = folded.trim();
    is_feat(folded) || folded.contains("remaster") || VERSION_NOTES.contains(&folded)
}

/// Keep only letters, digits and single spaces.
fn squash(value: &str) -> String {
    fold_text(value)
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A title folded for matching: version notes in brackets or after a
/// dash ("(Original Mix)", "- Radio Edit", "[2011 Remaster]") and featured
/// credits gone, case, accents and punctuation folded.
pub fn normalize_title(title: &str) -> String {
    let mut out = String::new();
    let mut rest = title;
    // Bracketed parts: a note goes, anything else stays.
    while let Some(open) = rest.find(['(', '[']) {
        let close_char = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find(close_char) {
            Some(close) => {
                let inner = &after[..close];
                if !is_note(inner) {
                    out.push(' ');
                    out.push_str(inner);
                    out.push(' ');
                }
                rest = &after[close + 1..];
            }
            None => {
                rest = after;
            }
        }
    }
    out.push_str(rest);
    // Dashed suffixes: " - Original Mix", " - Remastered 2009".
    let mut parts: Vec<&str> = out.split(" - ").collect();
    while parts.len() > 1 && parts.last().is_some_and(|part| is_note(part)) {
        parts.pop();
    }
    let joined = parts.join(" - ");
    // A featured credit outside brackets: "Song feat. Someone".
    let folded = fold_text(&joined);
    let cut = [" feat. ", " feat ", " ft. ", " ft ", " featuring "]
        .iter()
        .filter_map(|lead| folded.find(lead))
        .min()
        .map_or(folded.as_str(), |at| &folded[..at]);
    squash(cut)
}

/// The artists a credit names, folded: split on commas, `&`, `;`, `/`,
/// " x ", " and ", " vs " and featured credits.
pub fn split_artists(credit: &str) -> Vec<String> {
    let mut folded = format!(" {} ", fold_text(credit));
    for sep in [
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " featuring ",
        " with ",
        " x ",
        " and ",
        " vs. ",
        " vs ",
        "&",
        ",",
        ";",
        "/",
        "(",
        ")",
    ] {
        folded = folded.replace(sep, "\u{1f}");
    }
    folded
        .split('\u{1f}')
        .map(squash)
        .filter(|name| !name.is_empty())
        .collect()
}

/// Whether a file's artist credit and an entry's artists share the
/// primary artist (either side's first).
pub fn artists_overlap(file_artist: &str, item_artists: &[String]) -> bool {
    let file: Vec<String> = split_artists(file_artist);
    let item: Vec<String> = item_artists
        .iter()
        .flat_map(|name| split_artists(name))
        .collect();
    let (Some(file_first), Some(item_first)) = (file.first(), item_first_of(item_artists)) else {
        return false;
    };
    item.contains(file_first) || file.contains(&item_first)
}

fn item_first_of(artists: &[String]) -> Option<String> {
    artists
        .first()
        .and_then(|name| split_artists(name).into_iter().next())
}

/// Whether a file is the entry: the same title (folded), a shared primary
/// artist, and lengths within [`LENGTH_SLACK`].
pub fn is_match(tags: &Tags, item: &Item) -> bool {
    let (Some(file), Some(entry)) = (tags.seconds, item.seconds) else {
        return false;
    };
    if (file - entry).abs() > LENGTH_SLACK {
        return false;
    }
    let title = normalize_title(&tags.title);
    !title.is_empty()
        && title == normalize_title(&item.title)
        && artists_overlap(&tags.artist, &item.artists)
}

/// The one entry several matches are: all the same track, or all the same
/// ISRC. `None` when they differ (ambiguous) or there are none.
fn one_of<'a>(found: &[(&'a String, &'a Item)]) -> Option<(&'a String, &'a Item)> {
    let first = *found.first()?;
    let same = found.iter().all(|(_, item)| {
        item.id == first.1.id || (item.isrc.is_some() && item.isrc == first.1.isrc)
    });
    same.then_some(first)
}

/// The entry a file is: in its folder's playlist first, else across every
/// playlist when that is not ambiguous. Answers the playlist id, the entry
/// and whether it was the file's own folder.
pub fn find<'a>(
    catalogue: &'a Catalogue,
    folder: &str,
    tags: &Tags,
) -> Option<(&'a String, &'a Item, bool)> {
    let mut own = Vec::new();
    let mut all = Vec::new();
    for (id, cached) in &catalogue.playlists {
        for item in cached.items.iter().flatten() {
            if is_match(tags, item) {
                all.push((id, item));
                if cached.folder.as_deref() == Some(folder) {
                    own.push((id, item));
                }
            }
        }
    }
    if !own.is_empty() {
        return one_of(&own).map(|(id, item)| (id, item, true));
    }
    one_of(&all).map(|(id, item)| (id, item, false))
}

/// The resolver's facts for an entry.
pub fn item_facts(item: &Item) -> TrackFacts {
    let primary: Vec<String> = item.artists.iter().take(1).cloned().collect();
    TrackFacts {
        title: item.title.clone(),
        artists: item.artists.clone(),
        album: AlbumFacts {
            spotify_id: item.album_id.clone().unwrap_or_default(),
            title: item.album.clone(),
            artists: primary,
            upc: None,
            total_tracks: None,
        },
        isrc: item.isrc.clone(),
        seconds: item
            .seconds
            .filter(|s| s.is_finite() && *s > 0.0)
            .map(|s| s.round() as u64),
    }
}

/// Resolve an entry: its ISRC at MusicBrainz then Deezer (skipped when it
/// is the ISRC the file already failed with), else its Spotify album by
/// link and name through the album resolution.
///
/// # Errors
///
/// A service failed.
pub async fn resolve_item(
    item: &Item,
    file_isrc: Option<&str>,
    lookups: &dyn Lookups,
    check: &dyn StandInCheck,
) -> Result<Option<Resolved>, String> {
    let facts = item_facts(item);
    let tried = |code: &str| {
        file_isrc.is_some_and(|known| known.replace('-', "").eq_ignore_ascii_case(code))
    };
    if facts.isrc.as_deref().is_some_and(|code| !tried(code)) {
        if let Some((album, recording)) = lookups
            .mb_track_by_isrc(&facts)
            .await
            .map_err(|error| error.to_string())?
        {
            return Ok(Some(Resolved {
                via: Via::SpotifyPlaylist,
                album,
                stand_in: None,
                recording,
            }));
        }
        if let Some((stand_in, recording)) = lookups
            .deezer_track_by_isrc(&facts)
            .await
            .map_err(|error| error.to_string())?
        {
            let mut found =
                resolve::from_deezer(Via::SpotifyPlaylist, stand_in, recording, check).await?;
            found.via = Via::SpotifyPlaylist;
            return Ok(Some(found));
        }
    }
    let album = crate::imports::spotify_resolve::resolve_album(&facts.album, lookups)
        .await
        .map_err(|error| error.to_string())?;
    match album {
        None => Ok(None),
        Some(found) if found.source == FoundSource::Deezer => Ok(Some(
            resolve::from_deezer(Via::SpotifyPlaylist, found.id, String::new(), check).await?,
        )),
        Some(found) => Ok(Some(Resolved {
            via: Via::SpotifyPlaylist,
            album: found.id,
            stand_in: None,
            recording: String::new(),
        })),
    }
}

/// Whether the step looks at a file: not a duplicate, not held already,
/// not imported, tags read, and nothing found (or never looked up).
pub fn is_leftover(file: &FileState) -> bool {
    file.duplicate_of.is_none()
        && !file.in_library
        && file.outcome.is_none()
        && file.tags.is_some()
        && !matches!(file.resolved, Some(Resolution::Found(_)))
}

/// The Spotify side the step calls through.
pub struct PublicMatch {
    pub source: PublicSource,
    pub service: Arc<SpotifyImportService>,
    pub library: Arc<SpotifyLibrary>,
}

/// Why the Spotify side stopped.
enum Halt {
    /// Spotify refused to read the playlists (403/404 on the probe).
    Refused,
    /// The run was stopped by an admin.
    Stopped,
    /// Anything else: the note to show.
    Failed(String),
}

impl PublicMatch {
    /// The admin whose Spotify link the step uses: the first admin with an
    /// enabled link.
    async fn admin(&self, import: &PlaylistImport) -> Result<Option<String>, String> {
        import
            .db
            .db()
            .read("playlist_import.spotify_admin", |conn| {
                use rusqlite::OptionalExtension;
                conn.query_row(crate::imports::sql::ADMIN_WITH_SPOTIFY.text(), [], |row| {
                    row.get::<_, String>(0)
                })
                .optional()
                .map_err(DbError::from)
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// One call, waiting out any wait Spotify asked for (kept across a
    /// restart) and calling again after it.
    async fn call<T, F, Fut>(&self, import: &PlaylistImport, mut call: F) -> Result<T, Halt>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, SpotifyError>>,
    {
        loop {
            self.wait_cooldown(import).await?;
            match call().await {
                Err(SpotifyError::RateLimited { retry_after, .. }) => {
                    self.library.keep_cooldown().await;
                    if self.library.cooling_for().is_zero() {
                        // Nothing set a wait (the row turned it down
                        // locally): wait what Spotify said, else a minute.
                        let wait = Duration::from_secs(retry_after.unwrap_or(60).max(1));
                        self.service.client().cool_down(wait);
                    }
                    tracing::info!(
                        wait_secs = self.library.cooling_for().as_secs(),
                        "playlist import: spotify asked for a wait; waiting it out"
                    );
                }
                Err(error) if is_refused(&error) => return Err(Halt::Refused),
                Err(SpotifyError::NotLinked) => {
                    return Err(Halt::Failed(
                        "the admin's Spotify link is gone; link it again".to_owned(),
                    ));
                }
                Err(error) => return Err(Halt::Failed(format!("Spotify failed: {error:?}"))),
                Ok(answer) => return Ok(answer),
            }
        }
    }

    async fn wait_cooldown(&self, import: &PlaylistImport) -> Result<(), Halt> {
        loop {
            if import.stop.load(Ordering::SeqCst) {
                return Err(Halt::Stopped);
            }
            let left = self.library.cooling_for();
            if left.is_zero() {
                return Ok(());
            }
            import.set_note(&format!(
                "waiting {} s for Spotify's rate limit",
                left.as_secs()
            ));
            tokio::time::sleep(left.min(Duration::from_secs(5))).await;
        }
    }

    /// Bring the catalogue up to date: list the playlists (unless the
    /// listing is fresh), map them to the folders, and read the entries of
    /// each mapped playlist whose snapshot changed.
    async fn refresh(
        &self,
        import: &PlaylistImport,
        refresher: &TokenRefresher<'_>,
        catalogue: &mut Catalogue,
        folders: &BTreeSet<String>,
        now: i64,
    ) -> Result<(), Halt> {
        let client = self.service.client();
        let key = self.source.key();
        let fresh = catalogue.source == key
            && now - catalogue.listed_at < LISTING_FRESH_SECS
            && catalogue
                .playlists
                .values()
                .filter(|cached| cached.folder.is_some())
                .all(|cached| cached.items.is_some());
        if !fresh {
            // The first call is the probe: a refusal ends the step here.
            let listed: Vec<(String, String, Option<String>)> = match &self.source {
                PublicSource::User(user) => self
                    .call(import, || client.public_playlists(refresher, user))
                    .await?
                    .into_iter()
                    .map(|p| (p.id, p.name, p.snapshot_id))
                    .collect(),
                PublicSource::Playlists(ids) => {
                    let mut out = Vec::new();
                    for id in ids {
                        let head = self
                            .call(import, || client.playlist_head(refresher, id))
                            .await?;
                        out.push((head.id, head.name, head.snapshot_id));
                    }
                    out
                }
            };
            if catalogue.source != key {
                catalogue.playlists.clear();
            }
            let mut next = BTreeMap::new();
            for (id, name, snapshot_id) in listed {
                let kept = catalogue.playlists.remove(&id).unwrap_or_default();
                next.insert(
                    id,
                    Cached {
                        name,
                        snapshot_id,
                        ..kept
                    },
                );
            }
            catalogue.playlists = next;
            catalogue.source = key;
            catalogue.listed_at = now;
        }
        map_folders(&mut catalogue.playlists, folders);
        let stale: Vec<String> = catalogue
            .playlists
            .iter()
            .filter(|(_, cached)| {
                cached.folder.is_some()
                    && (cached.items.is_none()
                        || cached.snapshot_id.is_none()
                        || cached.items_snapshot != cached.snapshot_id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let total = stale.len();
        for (index, id) in stale.iter().enumerate() {
            import.set("spotify", index, total);
            let tracks = self
                .call(import, || {
                    client.playlist_tracks_with(refresher, id, MATCH_ITEM_FIELDS)
                })
                .await?;
            if let Some(cached) = catalogue.playlists.get_mut(id) {
                cached.items = Some(tracks.iter().map(Item::of).collect());
                cached.items_snapshot = cached.snapshot_id.clone();
            }
        }
        Ok(())
    }
}

/// How the step ended, for the run.
pub enum StepEnd {
    /// Done (or noted and skipped).
    Done,
    /// The run was stopped.
    Stopped,
}

impl PlaylistImport {
    /// The public-playlist match over the state's leftovers.
    pub(super) async fn public_match(&self, state: &mut State) -> Result<StepEnd, String> {
        let Some(public) = self.public.as_ref() else {
            return Ok(StepEnd::Done);
        };
        let mut catalogue = state.spotify.take().unwrap_or_default();
        catalogue.note = None;
        let folders: BTreeSet<String> = state
            .files
            .values()
            .map(|file| file.playlist.clone())
            .collect();
        let now = crate::db::to_i64(self.db.now_epoch());
        let before = total_calls(public);
        let refreshed = match public.admin(self).await {
            Err(error) => Err(Halt::Failed(format!(
                "the admin list did not read: {error}"
            ))),
            Ok(None) => Err(Halt::Failed(
                "no admin has a Spotify account linked".to_owned(),
            )),
            Ok(Some(admin)) => {
                match public
                    .service
                    .access(&admin, RequestPriority::BackgroundSync)
                    .await
                {
                    Err(_) => Err(Halt::Failed(
                        "the admin's Spotify link or the Spotify settings do not read".to_owned(),
                    )),
                    Ok((refresher, _)) => {
                        public
                            .refresh(self, &refresher, &mut catalogue, &folders, now)
                            .await
                    }
                }
            }
        };
        catalogue.calls = total_calls(public).saturating_sub(before);
        let halted = match settle(refreshed) {
            Ok(note) => note,
            Err(stopped) => {
                state.spotify = Some(catalogue);
                return Ok(stopped);
            }
        };
        if let Some(note) = &halted {
            tracing::warn!(%note, "playlist import: public playlist match skipped");
            catalogue.note = Some(note.clone());
        }
        self.hold.set_playlists(catalogue.mapped_ids());
        state.spotify = Some(catalogue);
        if halted.is_some() {
            // Entries kept from an earlier run still match.
            if state
                .spotify
                .as_ref()
                .is_none_or(|c| c.playlists.values().all(|p| p.items.is_none()))
            {
                return Ok(StepEnd::Done);
            }
        }
        self.match_leftovers(state).await
    }

    /// Match and resolve each leftover against the kept entries.
    async fn match_leftovers(&self, state: &mut State) -> Result<StepEnd, String> {
        let Some(catalogue) = state.spotify.clone() else {
            return Ok(StepEnd::Done);
        };
        let todo: Vec<String> = state
            .files
            .iter()
            .filter(|(_, file)| is_leftover(file))
            .map(|(rel, _)| rel.clone())
            .collect();
        let total = todo.len();
        for (index, rel) in todo.iter().enumerate() {
            if self.stop.load(Ordering::SeqCst) {
                return Ok(StepEnd::Stopped);
            }
            self.set("matching", index, total);
            let Some(file) = state.files.get(rel) else {
                continue;
            };
            let Some(tags) = file.tags.clone() else {
                continue;
            };
            let Some((playlist_id, item, same_folder)) = find(&catalogue, &file.playlist, &tags)
            else {
                if let Some(file) = state.files.get_mut(rel) {
                    file.playlist_match = None;
                }
                continue;
            };
            let already = file.playlist_match.as_ref().is_some_and(|known| {
                known.looked_up && known.track_id == item.id && known.playlist_id == *playlist_id
            });
            let mut matched = PlaylistMatch {
                playlist_id: playlist_id.clone(),
                track_id: item.id.clone(),
                same_folder,
                looked_up: already,
            };
            if !already {
                let answer = resolve_item(
                    item,
                    tags.isrc.as_deref(),
                    self.lookups.as_ref(),
                    self.lookups.as_ref(),
                )
                .await
                .map_err(|error| format!("a lookup failed (run again to go on): {error}"))?;
                matched.looked_up = true;
                if let Some(file) = state.files.get_mut(rel) {
                    match answer {
                        Some(found) => file.resolved = Some(Resolution::Found(found)),
                        None => {
                            if file.resolved.is_none() {
                                file.resolved = Some(Resolution::NotFound);
                            }
                        }
                    }
                }
                tokio::time::sleep(super::LOOKUP_GAP).await;
            }
            if let Some(file) = state.files.get_mut(rel) {
                file.playlist_match = Some(matched);
            }
            if (index + 1) % super::SAVE_EVERY == 0 {
                self.save(state).await?;
            }
        }
        Ok(StepEnd::Done)
    }
}

/// What a Spotify side that ended with `refreshed` means for the run: the
/// note for the report (the run goes on), or that the run was stopped. A
/// refusal is a note, never a failure of the run.
fn settle(refreshed: Result<(), Halt>) -> Result<Option<String>, StepEnd> {
    match refreshed {
        Ok(()) => Ok(None),
        Err(Halt::Stopped) => Err(StepEnd::Stopped),
        Err(Halt::Refused) => Ok(Some(REFUSED_NOTE.to_owned())),
        Err(Halt::Failed(note)) => Ok(Some(note)),
    }
}

fn total_calls(public: &PublicMatch) -> u64 {
    public.service.client().usage().counts().values().sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, title: &str, artists: &[&str], seconds: f64, isrc: Option<&str>) -> Item {
        Item {
            id: id.to_owned(),
            title: title.to_owned(),
            artists: artists.iter().map(|a| (*a).to_owned()).collect(),
            album: "Album".to_owned(),
            album_id: Some("alb".to_owned()),
            seconds: Some(seconds),
            isrc: isrc.map(str::to_owned),
        }
    }

    fn tags(title: &str, artist: &str, seconds: f64) -> Tags {
        Tags {
            title: title.to_owned(),
            artist: artist.to_owned(),
            seconds: Some(seconds),
            ..Tags::default()
        }
    }

    #[test]
    fn a_refused_probe_is_a_note_and_the_run_goes_on() {
        let refused = SpotifyError::Unavailable(
            "/users/{id}/playlists: spotify refused the request (403)".to_owned(),
        );
        assert!(is_refused(&refused));
        let missing = SpotifyError::Unavailable("spotify has no such record (404)".to_owned());
        assert!(is_refused(&missing));
        assert!(!is_refused(&SpotifyError::Unavailable(
            "spotify could not be reached (503)".to_owned()
        )));
        match settle(Err(Halt::Refused)) {
            Ok(Some(note)) => assert!(note.contains("needs the owner to sign in")),
            _ => panic!("a refusal is a note"),
        }
        assert!(matches!(settle(Ok(())), Ok(None)));
        assert!(matches!(settle(Err(Halt::Stopped)), Err(StepEnd::Stopped)));
    }

    #[test]
    fn titles_lose_version_notes_and_featured_credits() {
        for (raw, folded) in [
            ("Strobe (Original Mix)", "strobe"),
            ("Strobe - Original Mix", "strobe"),
            ("Strobe [Extended Mix]", "strobe"),
            ("Song (feat. Someone)", "song"),
            ("Song feat. Someone", "song"),
            ("Song ft. Someone", "song"),
            ("Song - Radio Edit", "song"),
            ("Song - 2011 Remaster", "song"),
            ("Song (Remastered 2009)", "song"),
            ("Café del Mar!", "cafe del mar"),
            ("Song (Someone Remix)", "song someone remix"),
            ("Song - Someone Remix", "song someone remix"),
        ] {
            assert_eq!(normalize_title(raw), folded, "{raw}");
        }
    }

    #[test]
    fn artists_split_and_overlap_on_the_primary() {
        assert_eq!(
            split_artists("Above & Beyond feat. Zoë Johnston"),
            vec![
                "above".to_owned(),
                "beyond".to_owned(),
                "zoe johnston".to_owned()
            ]
        );
        assert!(artists_overlap(
            "Deadmau5, Kaskade",
            &["deadmau5".to_owned()]
        ));
        assert!(artists_overlap(
            "Kaskade",
            &["Deadmau5".to_owned(), "Kaskade".to_owned()]
        ));
        assert!(!artists_overlap("Someone Else", &["Deadmau5".to_owned()]));
    }

    #[test]
    fn a_match_needs_title_artist_and_length() {
        let entry = item("1", "Strobe - Radio Edit", &["deadmau5"], 200.0, None);
        assert!(is_match(
            &tags("Strobe (Radio Edit)", "Deadmau5", 202.5),
            &entry
        ));
        assert!(!is_match(&tags("Strobe", "Deadmau5", 204.0), &entry));
        assert!(!is_match(&tags("Strobe", "Kaskade", 200.0), &entry));
        assert!(!is_match(&tags("Ghosts", "Deadmau5", 200.0), &entry));
        let mut lengthless = tags("Strobe", "Deadmau5", 200.0);
        lengthless.seconds = None;
        assert!(!is_match(&lengthless, &entry));
    }

    fn catalogue() -> Catalogue {
        let mut playlists = BTreeMap::new();
        playlists.insert(
            "p1".to_owned(),
            Cached {
                name: "Late Night".to_owned(),
                folder: Some("Late Night".to_owned()),
                items: Some(vec![item("t1", "Song", &["A"], 200.0, Some("X1"))]),
                ..Cached::default()
            },
        );
        playlists.insert(
            "p2".to_owned(),
            Cached {
                name: "Drive".to_owned(),
                folder: Some("Drive".to_owned()),
                items: Some(vec![
                    item("t2", "Other", &["B"], 180.0, None),
                    item("t3", "Twin", &["C"], 150.0, Some("Y1")),
                ]),
                ..Cached::default()
            },
        );
        playlists.insert(
            "p3".to_owned(),
            Cached {
                name: "Spare".to_owned(),
                folder: None,
                items: Some(vec![item("t4", "Twin", &["C"], 151.0, Some("Y2"))]),
                ..Cached::default()
            },
        );
        Catalogue {
            playlists,
            ..Catalogue::default()
        }
    }

    #[test]
    fn the_own_folder_wins_and_across_playlists_only_when_unambiguous() {
        let catalogue = catalogue();
        let (id, entry, own) = find(&catalogue, "Late Night", &tags("Song", "A", 201.0)).unwrap();
        assert_eq!((id.as_str(), entry.id.as_str(), own), ("p1", "t1", true));
        // In another folder: found across playlists.
        let (id, _, own) = find(&catalogue, "Drive", &tags("Song", "A", 199.0)).unwrap();
        assert_eq!((id.as_str(), own), ("p1", false));
        // Own folder first even when another playlist has a twin.
        let (_, entry, own) = find(&catalogue, "Drive", &tags("Twin", "C", 150.0)).unwrap();
        assert_eq!((entry.id.as_str(), own), ("t3", true));
        // Two different songs elsewhere: ambiguous, none.
        assert!(find(&catalogue, "Late Night", &tags("Twin", "C", 150.5)).is_none());
    }

    #[test]
    fn folders_map_to_playlists_by_folded_name_only() {
        let mut playlists = BTreeMap::new();
        for (id, name) in [
            ("a", "Late-Night Drive!"),
            ("b", "d&b"),
            ("c", "Nowhere"),
            ("d", "Twice"),
            ("e", "twice"),
        ] {
            playlists.insert(
                id.to_owned(),
                Cached {
                    name: name.to_owned(),
                    ..Cached::default()
                },
            );
        }
        let folders: BTreeSet<String> = ["late night drive", "D&B", "Twice", "Leftover"]
            .iter()
            .map(|f| (*f).to_owned())
            .collect();
        let unmapped = map_folders(&mut playlists, &folders);
        assert_eq!(playlists["a"].folder.as_deref(), Some("late night drive"));
        assert_eq!(playlists["b"].folder.as_deref(), Some("D&B"));
        assert_eq!(playlists["c"].folder, None);
        // Two playlists folding to one name map neither.
        assert_eq!(playlists["d"].folder, None);
        assert_eq!(unmapped, vec!["Leftover".to_owned(), "Twice".to_owned()]);
    }

    #[test]
    fn the_setting_reads_a_user_or_playlist_links() {
        assert_eq!(
            PublicSource::parse(
                Some("https://open.spotify.com/user/someone/playlists"),
                None
            ),
            Some(PublicSource::User("someone".to_owned()))
        );
        assert_eq!(
            PublicSource::parse(
                Some("someone"),
                Some(
                    "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M?si=x, spotify:playlist:abc123"
                )
            ),
            Some(PublicSource::Playlists(vec![
                "37i9dQZF1DXcBWIGoYBM5M".to_owned(),
                "abc123".to_owned()
            ]))
        );
        assert_eq!(PublicSource::parse(Some("  "), None), None);
        assert_eq!(playlist_id_of("bad/../id"), None);
    }
}
