//! Fork: which album, song or artist here a Spotify item is.
//!
//! One resolver serves a link pasted into search (`GET
//! /api/v3/resolve/spotify`) and the library sync (saved albums, followed
//! artists). It answers only on proof or a confident match, never a guess:
//!
//! - a song: its ISRC at MusicBrainz (the recording, placed on its album),
//!   else its ISRC at Deezer (`/track/isrc:`), else title, artist and
//!   length at MusicBrainz, then on the Deezer album Spotify names;
//! - an album: its barcode at MusicBrainz, else a MusicBrainz release that
//!   links the Spotify album, else its barcode at Deezer (`/album/upc:`),
//!   else title, artist and track count, MusicBrainz then Deezer;
//! - an artist: the MusicBrainz artist that links the Spotify artist, else
//!   the one credited by that name on a saved album already proved (by
//!   barcode or link), else one whose name agrees and whose albums overlap
//!   the titles known, else the Deezer artist of that name. None of these
//!   calls Spotify; the library sync reads an artist's albums there only
//!   for what is still unmatched ([`resolve_artist_named`]).
//!
//! A Deezer answer is kept in the source catalogue under its stand-in id
//! (`library::sources`), so it opens the normal album or artist page and
//! requests like any other. The order lives in [`resolve_track`],
//! [`resolve_album`] and [`resolve_artist`] over [`Lookups`]; [`Live`] is
//! the lookups over MusicBrainz, Deezer and the catalogue.

use std::collections::BTreeSet;
use std::sync::Arc;

use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_library::sources::catalogue;
use droppedneedle_library::sources::deezer::recording_id;
use droppedneedle_library::sources::editions::base_title;
use droppedneedle_providers::musicbrainz::{Criticality, normalize_mb_id};
use droppedneedle_providers::preview::{DeezerAlbum, DeezerArtistAlbum, PreviewClient, norm};
use droppedneedle_providers::virtual_ids::{self, Kind};
use droppedneedle_providers::{ProviderError, RequestPriority};

use super::spotify_albums::{AlbumResolver, SpotifyAlbum};
use super::spotify_tracks::{SpotifySong, choose, names_agree, titles_agree};
use crate::db::AcquireDb;
use crate::landing::want::LENGTH_TOLERANCE_SECS;

/// What a Spotify link names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// A song.
    Track,
    /// An album (or single, or EP).
    Album,
    /// An artist.
    Artist,
}

impl LinkKind {
    /// The kind as Spotify spells it in a link.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Artist => "artist",
        }
    }
}

/// One parsed Spotify link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyLink {
    /// What it names.
    pub kind: LinkKind,
    /// The Spotify id (22 base-62 characters).
    pub id: String,
}

impl SpotifyLink {
    /// The canonical public address, as oEmbed and MusicBrainz know it.
    pub fn url(&self) -> String {
        format!(
            "https://open.spotify.com/{}/{}",
            self.kind.as_str(),
            self.id
        )
    }
}

/// A Spotify id: 22 base-62 characters.
pub fn is_spotify_id(id: &str) -> bool {
    id.len() == 22 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

fn kind_of(word: &str) -> Option<LinkKind> {
    match word {
        "track" => Some(LinkKind::Track),
        "album" => Some(LinkKind::Album),
        "artist" => Some(LinkKind::Artist),
        _ => None,
    }
}

/// The track, album or artist a pasted Spotify link names:
/// `open.spotify.com/{track,album,artist}/<id>` with or without the
/// scheme, an `/intl-xx/` prefix, `/embed/`, or a `?si=` query, and the
/// `spotify:<type>:<id>` form. Anything else (a playlist, a show, text) is
/// `None`.
pub fn parse_link(input: &str) -> Option<SpotifyLink> {
    let text = input.trim();
    if let Some(rest) = text.strip_prefix("spotify:") {
        let mut parts = rest.split(':');
        let kind = kind_of(parts.next()?)?;
        let id = parts.next()?;
        if parts.next().is_some() || !is_spotify_id(id) {
            return None;
        }
        return Some(SpotifyLink {
            kind,
            id: id.to_owned(),
        });
    }
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .unwrap_or(text);
    let (host, path) = rest.split_once('/')?;
    if !matches!(
        host.to_ascii_lowercase().as_str(),
        "open.spotify.com" | "play.spotify.com"
    ) {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    let mut first = segments.next()?;
    if first.starts_with("intl-") {
        first = segments.next()?;
    }
    if first == "embed" {
        first = segments.next()?;
    }
    let kind = kind_of(first)?;
    let id = segments.next()?;
    if segments.next().is_some() || !is_spotify_id(id) {
        return None;
    }
    Some(SpotifyLink {
        kind,
        id: id.to_owned(),
    })
}

/// What Spotify says of an album.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumFacts {
    /// Spotify album id, empty when unknown.
    pub spotify_id: String,
    pub title: String,
    /// Credited artists, Spotify's order.
    pub artists: Vec<String>,
    /// Barcode (UPC or EAN).
    pub upc: Option<String>,
    pub total_tracks: Option<i64>,
}

/// What Spotify says of a song.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackFacts {
    pub title: String,
    pub artists: Vec<String>,
    /// The album Spotify puts it on.
    pub album: AlbumFacts,
    pub isrc: Option<String>,
    pub seconds: Option<u64>,
}

impl TrackFacts {
    fn song(&self) -> SpotifySong {
        SpotifySong {
            title: self.title.clone(),
            artists: self.artists.clone(),
            album: self.album.title.clone(),
            seconds: self.seconds,
        }
    }
}

/// What Spotify says of an artist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtistFacts {
    pub spotify_id: String,
    pub name: String,
    /// Titles of its albums and singles at Spotify, for the overlap check;
    /// empty when unknown.
    pub album_titles: Vec<String>,
    /// MusicBrainz release groups of albums by this name the user saved
    /// and that matched by proof; empty when none.
    pub album_groups: Vec<String>,
}

/// Where a match came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    MusicBrainz,
    Deezer,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MusicBrainz => "musicbrainz",
            Self::Deezer => "deezer",
        }
    }
}

/// What proved a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The song's ISRC.
    Isrc,
    /// A MusicBrainz link to the Spotify item.
    Link,
    /// The album's barcode.
    Barcode,
    /// A confident name match.
    Name,
    /// The artist credited on an album already matched by proof.
    Album,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Isrc => "isrc",
            Self::Link => "link",
            Self::Barcode => "barcode",
            Self::Name => "name",
            Self::Album => "album",
        }
    }
}

/// A matched album or artist: its id here (a MusicBrainz id or a stand-in).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub source: Source,
    pub via: Via,
}

/// A matched song: its album and its recording on that album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundTrack {
    pub album: Found,
    pub recording: String,
}

/// Why a resolution stopped: a service failed, or keeping a Deezer answer
/// in the catalogue did. Never "not found": that is `Ok(None)`.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("catalogue: {0}")]
    Store(#[from] droppedneedle_kernel::db::DbError),
}

/// An album and recording id pair, as a lookup answers a song.
pub type Placed = (String, String);

/// The lookups the resolution order runs over. Each answers `Ok(None)`
/// when it found nothing it is sure of, and an error when its service
/// failed, which stops the resolution: a service that is down is never
/// read as "not there".
pub trait Lookups: Send + Sync {
    fn mb_track_by_isrc<'a>(
        &'a self,
        track: &'a TrackFacts,
    ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>>;
    fn deezer_track_by_isrc<'a>(
        &'a self,
        track: &'a TrackFacts,
    ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>>;
    fn mb_track_by_name<'a>(
        &'a self,
        track: &'a TrackFacts,
    ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>>;
    fn deezer_track_by_name<'a>(
        &'a self,
        track: &'a TrackFacts,
    ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>>;
    fn mb_album_by_barcode<'a>(
        &'a self,
        album: &'a AlbumFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>>;
    fn mb_album_by_link<'a>(
        &'a self,
        album: &'a AlbumFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>>;
    fn deezer_album_by_barcode<'a>(
        &'a self,
        album: &'a AlbumFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>>;
    fn mb_album_by_name<'a>(
        &'a self,
        album: &'a AlbumFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>>;
    fn deezer_album_by_name<'a>(
        &'a self,
        album: &'a AlbumFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>>;
    fn mb_artist_by_link<'a>(
        &'a self,
        artist: &'a ArtistFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>>;
    fn mb_artist_by_album<'a>(
        &'a self,
        artist: &'a ArtistFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>>;
    fn mb_artist_by_name<'a>(
        &'a self,
        artist: &'a ArtistFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>>;
    fn deezer_artist_by_name<'a>(
        &'a self,
        artist: &'a ArtistFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>>;
}

fn placed(found: Option<Placed>, source: Source, via: Via) -> Option<FoundTrack> {
    found.map(|(album, recording)| FoundTrack {
        album: Found {
            id: album,
            source,
            via,
        },
        recording,
    })
}

fn found(id: Option<String>, source: Source, via: Via) -> Option<Found> {
    id.map(|id| Found { id, source, via })
}

/// The song a Spotify track is, in the order the module names.
///
/// # Errors
///
/// The first lookup whose service failed.
pub async fn resolve_track(
    track: &TrackFacts,
    lookups: &dyn Lookups,
) -> Result<Option<FoundTrack>, ResolveError> {
    if track.isrc.is_some() {
        if let Some(hit) = placed(
            lookups.mb_track_by_isrc(track).await?,
            Source::MusicBrainz,
            Via::Isrc,
        ) {
            return Ok(Some(hit));
        }
        if let Some(hit) = placed(
            lookups.deezer_track_by_isrc(track).await?,
            Source::Deezer,
            Via::Isrc,
        ) {
            return Ok(Some(hit));
        }
    }
    if track.title.trim().is_empty() || track.artists.is_empty() || track.seconds.is_none() {
        return Ok(None);
    }
    if let Some(hit) = placed(
        lookups.mb_track_by_name(track).await?,
        Source::MusicBrainz,
        Via::Name,
    ) {
        return Ok(Some(hit));
    }
    Ok(placed(
        lookups.deezer_track_by_name(track).await?,
        Source::Deezer,
        Via::Name,
    ))
}

/// The album a Spotify album is, in the order the module names.
///
/// # Errors
///
/// The first lookup whose service failed.
pub async fn resolve_album(
    album: &AlbumFacts,
    lookups: &dyn Lookups,
) -> Result<Option<Found>, ResolveError> {
    if album.upc.is_some()
        && let Some(hit) = found(
            lookups.mb_album_by_barcode(album).await?,
            Source::MusicBrainz,
            Via::Barcode,
        )
    {
        return Ok(Some(hit));
    }
    if !album.spotify_id.is_empty()
        && let Some(hit) = found(
            lookups.mb_album_by_link(album).await?,
            Source::MusicBrainz,
            Via::Link,
        )
    {
        return Ok(Some(hit));
    }
    if album.upc.is_some()
        && let Some(hit) = found(
            lookups.deezer_album_by_barcode(album).await?,
            Source::Deezer,
            Via::Barcode,
        )
    {
        return Ok(Some(hit));
    }
    if album.title.trim().is_empty() || album.artists.is_empty() {
        return Ok(None);
    }
    if let Some(hit) = found(
        lookups.mb_album_by_name(album).await?,
        Source::MusicBrainz,
        Via::Name,
    ) {
        return Ok(Some(hit));
    }
    Ok(found(
        lookups.deezer_album_by_name(album).await?,
        Source::Deezer,
        Via::Name,
    ))
}

/// The artist a Spotify artist is, in the order the module names.
///
/// # Errors
///
/// The first lookup whose service failed.
pub async fn resolve_artist(
    artist: &ArtistFacts,
    lookups: &dyn Lookups,
) -> Result<Option<Found>, ResolveError> {
    if !artist.spotify_id.is_empty()
        && let Some(hit) = found(
            lookups.mb_artist_by_link(artist).await?,
            Source::MusicBrainz,
            Via::Link,
        )
    {
        return Ok(Some(hit));
    }
    if artist.name.trim().is_empty() {
        return Ok(None);
    }
    if !artist.album_groups.is_empty()
        && let Some(hit) = found(
            lookups.mb_artist_by_album(artist).await?,
            Source::MusicBrainz,
            Via::Album,
        )
    {
        return Ok(Some(hit));
    }
    resolve_artist_named(artist, lookups).await
}

/// The name steps of [`resolve_artist`] alone: MusicBrainz by name and
/// album overlap, then Deezer by name. For an artist whose link and
/// albums proved nothing, once its album titles are known.
///
/// # Errors
///
/// The first lookup whose service failed.
pub async fn resolve_artist_named(
    artist: &ArtistFacts,
    lookups: &dyn Lookups,
) -> Result<Option<Found>, ResolveError> {
    if artist.name.trim().is_empty() {
        return Ok(None);
    }
    if !artist.album_titles.is_empty()
        && let Some(hit) = found(
            lookups.mb_artist_by_name(artist).await?,
            Source::MusicBrainz,
            Via::Name,
        )
    {
        return Ok(Some(hit));
    }
    Ok(found(
        lookups.deezer_artist_by_name(artist).await?,
        Source::Deezer,
        Via::Name,
    ))
}

/// The fold two album titles are compared under: versions and edition
/// notes aside, letters and digits only.
pub fn title_key(title: &str) -> String {
    let key = base_title(title);
    if key.is_empty() { norm(title) } else { key }
}

/// Whether a candidate album is the Spotify album by name: the same title
/// (edition notes aside), an artist that agrees, and the same number of
/// tracks when both know it.
pub fn album_agrees(
    album: &AlbumFacts,
    title: &str,
    artists: &[&str],
    tracks: Option<i64>,
) -> bool {
    let key = title_key(&album.title);
    if key.is_empty() || title_key(title) != key {
        return false;
    }
    let artist_ok = artists.iter().any(|candidate| {
        album
            .artists
            .iter()
            .any(|asked| names_agree(candidate, asked))
    });
    if !artist_ok {
        return false;
    }
    match (album.total_tracks, tracks) {
        (Some(asked), Some(found)) => asked == found,
        _ => true,
    }
}

/// How many of `ours` (Spotify's album titles) are among `theirs`, by
/// title key.
pub fn album_overlap(ours: &[String], theirs: &[String]) -> usize {
    let theirs: BTreeSet<String> = theirs
        .iter()
        .map(|title| title_key(title))
        .filter(|key| !key.is_empty())
        .collect();
    ours.iter()
        .map(|title| title_key(title))
        .filter(|key| !key.is_empty())
        .collect::<BTreeSet<_>>()
        .iter()
        .filter(|key| theirs.contains(*key))
        .count()
}

/// The overlap an artist's albums need to back a name match: two shared
/// titles, or one when Spotify lists only one.
pub fn overlap_backs(ours: &[String], shared: usize) -> bool {
    shared >= 2 || (shared == 1 && ours.len() == 1)
}

/// Whether two artist names are the same name (letters and digits only).
pub fn same_name(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && a == b
}

/// Candidate ids a match picked, distinct: exactly one is a match, two or
/// more are a guess.
fn only<T: Ord + Clone>(ids: &[T]) -> Option<T> {
    let distinct: BTreeSet<&T> = ids.iter().collect();
    (distinct.len() == 1).then(|| ids[0].clone())
}

/// The lookups over MusicBrainz (through the import's album resolver),
/// Deezer, and the source catalogue a Deezer answer is kept in.
#[derive(Clone)]
pub struct Live {
    albums: Arc<AlbumResolver>,
    deezer: PreviewClient,
    db: AcquireDb,
    priority: RequestPriority,
}

/// MusicBrainz candidates read per name search.
const NAME_HITS: u32 = 10;
/// Artist candidates whose albums are read for the overlap check.
const ARTIST_CANDIDATES: usize = 3;
/// Saved albums whose credits are read to name an artist.
const ALBUM_GROUPS: usize = 3;

impl Live {
    /// Lookups at `priority`.
    pub fn new(
        albums: Arc<AlbumResolver>,
        deezer: PreviewClient,
        db: AcquireDb,
        priority: RequestPriority,
    ) -> Self {
        Self {
            albums,
            deezer,
            db,
            priority,
        }
    }

    /// The same lookups at another priority.
    pub fn at(&self, priority: RequestPriority) -> Self {
        Self {
            priority,
            ..self.clone()
        }
    }

    fn spotify_album(album: &AlbumFacts, isrc: Option<&str>) -> SpotifyAlbum {
        let mut out = SpotifyAlbum {
            title: album.title.clone(),
            artist: album.artists.first().cloned().unwrap_or_default(),
            isrcs: BTreeSet::new(),
            spotify_id: album.spotify_id.clone(),
        };
        if let Some(isrc) = isrc {
            out.add_isrc(isrc);
        }
        out
    }

    /// Keep a Deezer album in the source catalogue and answer its stand-in
    /// release group.
    async fn keep_deezer_album(&self, album: DeezerAlbum) -> Result<String, ResolveError> {
        let id = album.id.to_string();
        let now = self.db.clock().now_ms();
        let kept = self
            .db
            .write("spotify.resolve.keep_album", move |tx| {
                let artist_id = album
                    .artist_id
                    .map(|artist| artist.to_string())
                    .unwrap_or_default();
                if !artist_id.is_empty() {
                    catalogue::keep_artist(
                        tx,
                        &artist_id,
                        &album.artist_name,
                        album.artist_picture.as_deref(),
                        now,
                    )?;
                }
                let listed = DeezerArtistAlbum {
                    id: album.id,
                    title: album.title.clone(),
                    release_date: album.release_date.clone(),
                    record_type: album.record_type.clone(),
                    cover_url: album.cover_url.clone(),
                };
                catalogue::keep_listing(tx, &artist_id, &album.artist_name, &[listed], now)?;
                catalogue::keep_album(tx, &album, now)?;
                Ok(())
            })
            .await;
        kept.map_err(ResolveError::Store)?;
        Ok(virtual_ids::mint(
            catalogue::PROVIDER,
            Kind::ReleaseGroup,
            &id,
        ))
    }

    /// The song on a Deezer album: the track carrying the ISRC, else the
    /// one whose title and length agree.
    fn deezer_track_on(album: &DeezerAlbum, track: &TrackFacts) -> Option<String> {
        if let Some(isrc) = &track.isrc
            && let Some(hit) = album.tracks.iter().find(|candidate| {
                candidate
                    .isrc
                    .as_deref()
                    .is_some_and(|code| code.eq_ignore_ascii_case(isrc))
            })
        {
            return Some(hit.id.to_string());
        }
        let hits: Vec<String> = album
            .tracks
            .iter()
            .filter(|candidate| titles_agree(&candidate.title, &track.title))
            .filter(|candidate| match (track.seconds, candidate.duration) {
                (Some(asked), Some(found)) => {
                    (found as f64 - asked as f64).abs() <= LENGTH_TOLERANCE_SECS
                }
                _ => false,
            })
            .map(|candidate| candidate.id.to_string())
            .collect();
        only(&hits)
    }

    /// The Deezer album a name search is sure of: every hit that agrees is
    /// the same album (Deezer lists clean and explicit copies apart).
    async fn deezer_album_named(
        &self,
        album: &AlbumFacts,
    ) -> Result<Option<DeezerAlbum>, ResolveError> {
        let artist = album.artists.first().cloned().unwrap_or_default();
        let hits = self
            .deezer
            .deezer_search_albums(&artist, &album.title, NAME_HITS, self.priority)
            .await?;
        let agreeing: Vec<_> = hits
            .iter()
            .filter(|hit| {
                let artist = hit.artist.as_ref().map(|a| a.name.as_str()).unwrap_or("");
                album_agrees(album, &hit.title, &[artist], hit.nb_tracks)
            })
            .collect();
        let Some(first) = agreeing.first() else {
            return Ok(None);
        };
        let same = agreeing.iter().all(|hit| {
            title_key(&hit.title) == title_key(&first.title) && hit.nb_tracks == first.nb_tracks
        });
        if !same {
            return Ok(None);
        }
        Ok(self.deezer.deezer_album(first.id, self.priority).await?)
    }
}

impl Lookups for Live {
    fn mb_track_by_isrc<'a>(
        &'a self,
        track: &'a TrackFacts,
    ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>> {
        Box::pin(async move {
            let Some(isrc) = track.isrc.as_deref() else {
                return Ok(None);
            };
            let isrc = isrc.trim().to_ascii_uppercase();
            if isrc.len() != 12 || !isrc.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Ok(None);
            }
            let view = self.albums.view(self.priority)?;
            let page = view
                .search_recordings_query(
                    &format!("isrc:{isrc}"),
                    super::spotify_tracks::TRACK_SEARCH_LIMIT,
                    Criticality::IdentityCritical,
                )
                .await?;
            Ok(choose(&track.song(), &page.items).map(|hit| (hit.group, hit.recording)))
        })
    }

    fn deezer_track_by_isrc<'a>(
        &'a self,
        track: &'a TrackFacts,
    ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>> {
        Box::pin(async move {
            let Some(isrc) = track.isrc.as_deref() else {
                return Ok(None);
            };
            let Some(id) = self
                .deezer
                .deezer_album_of_isrc(isrc, self.priority)
                .await?
            else {
                return Ok(None);
            };
            let Some(album) = self.deezer.deezer_album(id, self.priority).await? else {
                return Ok(None);
            };
            let Some(song) = Self::deezer_track_on(&album, track) else {
                return Ok(None);
            };
            let group = self.keep_deezer_album(album).await?;
            Ok(Some((group, recording_id(&song))))
        })
    }

    fn mb_track_by_name<'a>(
        &'a self,
        track: &'a TrackFacts,
    ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>> {
        Box::pin(async move {
            Ok(self
                .albums
                .resolve_track(&track.song())
                .await?
                .map(|hit| (hit.group, hit.recording)))
        })
    }

    fn deezer_track_by_name<'a>(
        &'a self,
        track: &'a TrackFacts,
    ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>> {
        Box::pin(async move {
            let album = AlbumFacts {
                total_tracks: None,
                ..track.album.clone()
            };
            if album.title.trim().is_empty() {
                return Ok(None);
            }
            let album = AlbumFacts {
                artists: if album.artists.is_empty() {
                    track.artists.clone()
                } else {
                    album.artists
                },
                ..album
            };
            let Some(found) = self.deezer_album_named(&album).await? else {
                return Ok(None);
            };
            let Some(song) = Self::deezer_track_on(&found, track) else {
                return Ok(None);
            };
            let group = self.keep_deezer_album(found).await?;
            Ok(Some((group, recording_id(&song))))
        })
    }

    fn mb_album_by_barcode<'a>(
        &'a self,
        album: &'a AlbumFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
        Box::pin(async move {
            let Some(upc) = album.upc.as_deref() else {
                return Ok(None);
            };
            Ok(self
                .albums
                .resolve_barcode(&Self::spotify_album(album, None), upc)
                .await?
                .map(|proven| proven.group))
        })
    }

    fn mb_album_by_link<'a>(
        &'a self,
        album: &'a AlbumFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
        Box::pin(async move {
            Ok(self
                .albums
                .resolve_link(&Self::spotify_album(album, None), self.priority)
                .await?
                .map(|proven| proven.group))
        })
    }

    fn deezer_album_by_barcode<'a>(
        &'a self,
        album: &'a AlbumFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
        Box::pin(async move {
            let Some(upc) = album.upc.as_deref() else {
                return Ok(None);
            };
            let Some(found) = self.deezer.deezer_album_of_upc(upc, self.priority).await? else {
                return Ok(None);
            };
            Ok(Some(self.keep_deezer_album(found).await?))
        })
    }

    fn mb_album_by_name<'a>(
        &'a self,
        album: &'a AlbumFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
        Box::pin(async move {
            let view = self.albums.view(self.priority)?;
            let artist = album.artists.first().cloned().unwrap_or_default();
            let page = view
                .search_releases(
                    &album.title,
                    &artist,
                    NAME_HITS,
                    Criticality::IdentityCritical,
                )
                .await?;
            let groups: Vec<String> = page
                .items
                .iter()
                .filter(|hit| {
                    let credits: Vec<&str> =
                        hit.artist_credit.iter().map(|c| c.name.as_str()).collect();
                    let tracks: u32 = hit.media.iter().filter_map(|m| m.track_count).sum();
                    let tracks = (tracks > 0).then_some(i64::from(tracks));
                    album_agrees(album, hit.title.as_deref().unwrap_or(""), &credits, tracks)
                })
                .filter_map(|hit| hit.release_group.as_ref())
                .map(|group| normalize_mb_id(&group.id))
                .collect();
            Ok(only(&groups))
        })
    }

    fn deezer_album_by_name<'a>(
        &'a self,
        album: &'a AlbumFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
        Box::pin(async move {
            match self.deezer_album_named(album).await? {
                Some(found) => Ok(Some(self.keep_deezer_album(found).await?)),
                None => Ok(None),
            }
        })
    }

    fn mb_artist_by_link<'a>(
        &'a self,
        artist: &'a ArtistFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
        Box::pin(async move {
            if !is_spotify_id(&artist.spotify_id) {
                return Ok(None);
            }
            let view = self.albums.view(self.priority)?;
            let resolution = view
                .resolve_url(
                    &format!("https://open.spotify.com/artist/{}", artist.spotify_id),
                    &["artist-rels"],
                    Criticality::IdentityCritical,
                )
                .await?;
            let ids: Vec<String> = resolution
                .relations
                .iter()
                .filter_map(|relation| relation.artist.as_ref())
                .map(|found| normalize_mb_id(&found.id))
                .collect();
            Ok(only(&ids))
        })
    }

    fn mb_artist_by_album<'a>(
        &'a self,
        artist: &'a ArtistFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
        Box::pin(async move {
            let view = self.albums.view(self.priority)?;
            let mut ids = Vec::new();
            for group in artist.album_groups.iter().take(ALBUM_GROUPS) {
                let Some(lookup) = view
                    .lookup_release_group(group, &["artist-credits"], Criticality::IdentityCritical)
                    .await?
                else {
                    continue;
                };
                ids.extend(
                    lookup
                        .entity
                        .artist_credit
                        .iter()
                        .filter(|credit| {
                            same_name(&credit.artist.name, &artist.name)
                                || same_name(&credit.name, &artist.name)
                        })
                        .map(|credit| normalize_mb_id(&credit.artist.id)),
                );
            }
            Ok(only(&ids))
        })
    }

    fn mb_artist_by_name<'a>(
        &'a self,
        artist: &'a ArtistFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
        Box::pin(async move {
            let view = self.albums.view(self.priority)?;
            let page = view
                .search_artists(&artist.name, NAME_HITS, Criticality::IdentityCritical)
                .await?;
            let mut backed = Vec::new();
            for hit in page
                .items
                .iter()
                .filter(|hit| same_name(hit.name.as_deref().unwrap_or(""), &artist.name))
                .take(ARTIST_CANDIDATES)
            {
                let groups = view
                    .browse_artist_release_groups(&hit.id, 100, 0, Criticality::IdentityCritical)
                    .await?;
                let titles: Vec<String> = groups
                    .items
                    .into_iter()
                    .filter_map(|group| group.title)
                    .collect();
                if overlap_backs(
                    &artist.album_titles,
                    album_overlap(&artist.album_titles, &titles),
                ) {
                    backed.push(normalize_mb_id(&hit.id));
                }
            }
            Ok(only(&backed))
        })
    }

    fn deezer_artist_by_name<'a>(
        &'a self,
        artist: &'a ArtistFacts,
    ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
        Box::pin(async move {
            let hits = self
                .deezer
                .deezer_search_artists(&artist.name, NAME_HITS, self.priority)
                .await?;
            let named: Vec<_> = hits
                .into_iter()
                .filter(|hit| same_name(&hit.name, &artist.name))
                .collect();
            let chosen = match named.len() {
                0 => None,
                1 => named.into_iter().next(),
                _ if artist.album_titles.is_empty() => None,
                _ => {
                    let mut backed = Vec::new();
                    for hit in named.into_iter().take(ARTIST_CANDIDATES) {
                        let titles: Vec<String> = self
                            .deezer
                            .deezer_artist_albums(hit.id, self.priority)
                            .await?
                            .into_iter()
                            .map(|album| album.title)
                            .collect();
                        if overlap_backs(
                            &artist.album_titles,
                            album_overlap(&artist.album_titles, &titles),
                        ) {
                            backed.push(hit);
                        }
                    }
                    if backed.len() == 1 {
                        backed.pop()
                    } else {
                        None
                    }
                }
            };
            let Some(hit) = chosen else {
                return Ok(None);
            };
            let now = self.db.clock().now_ms();
            let kept = self
                .db
                .write("spotify.resolve.keep_artist", move |tx| {
                    Ok(catalogue::keep_artist(
                        tx,
                        &hit.id.to_string(),
                        &hit.name,
                        hit.picture.as_deref(),
                        now,
                    )?)
                })
                .await
                .map_err(ResolveError::Store)?;
            Ok(Some(kept))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const ID: &str = "4uLU6hMCjMI75M1A2tKUQC";

    #[test]
    fn every_link_form_parses() {
        let track = Some(SpotifyLink {
            kind: LinkKind::Track,
            id: ID.to_owned(),
        });
        for input in [
            format!("https://open.spotify.com/track/{ID}"),
            format!("https://open.spotify.com/track/{ID}?si=abc123"),
            format!("https://open.spotify.com/intl-de/track/{ID}?si=abc123&utm=x"),
            format!("http://open.spotify.com/track/{ID}/"),
            format!("open.spotify.com/track/{ID}"),
            format!("  https://open.spotify.com/embed/track/{ID}  "),
            format!("spotify:track:{ID}"),
        ] {
            assert_eq!(parse_link(&input), track, "{input}");
        }
        assert_eq!(
            parse_link(&format!("https://open.spotify.com/album/{ID}")).map(|l| l.kind),
            Some(LinkKind::Album)
        );
        assert_eq!(
            parse_link(&format!("spotify:artist:{ID}")).map(|l| l.kind),
            Some(LinkKind::Artist)
        );
        assert_eq!(
            parse_link(&format!(
                "https://open.spotify.com/intl-pt-br/artist/{ID}#x"
            ))
            .map(|l| l.kind),
            Some(LinkKind::Artist)
        );
    }

    #[test]
    fn other_text_is_not_a_link() {
        for input in [
            "radiohead".to_owned(),
            format!("https://open.spotify.com/playlist/{ID}"),
            format!("spotify:playlist:{ID}"),
            "https://open.spotify.com/track/short".to_owned(),
            format!("https://example.com/track/{ID}"),
            format!("https://open.spotify.com/track/{ID}/extra"),
            format!("spotify:track:{ID}:more"),
            String::new(),
        ] {
            assert_eq!(parse_link(&input), None, "{input}");
        }
    }

    #[test]
    fn a_name_match_needs_title_artist_and_count_to_agree() {
        let album = AlbumFacts {
            title: "In Rainbows (Remastered)".into(),
            artists: vec!["Radiohead".into()],
            total_tracks: Some(10),
            ..AlbumFacts::default()
        };
        assert!(album_agrees(
            &album,
            "In Rainbows",
            &["Radiohead"],
            Some(10)
        ));
        assert!(album_agrees(&album, "In Rainbows", &["Radiohead"], None));
        assert!(!album_agrees(
            &album,
            "In Rainbows",
            &["Radiohead"],
            Some(18)
        ));
        assert!(!album_agrees(
            &album,
            "In Rainbows",
            &["Someone Else"],
            Some(10)
        ));
        assert!(!album_agrees(&album, "Kid A", &["Radiohead"], Some(10)));
    }

    #[test]
    fn an_artist_name_match_needs_album_overlap() {
        let ours: Vec<String> = vec!["OK Computer".into(), "Kid A (Remastered)".into()];
        let theirs: Vec<String> = vec!["Kid A".into(), "OK Computer".into(), "Amnesiac".into()];
        assert_eq!(album_overlap(&ours, &theirs), 2);
        assert!(overlap_backs(&ours, 2));
        assert!(!overlap_backs(&ours, 1));
        assert!(overlap_backs(&["Only".to_owned()], 1));
        assert!(same_name("Sigur Rós", "sigur ros") || !same_name("Sigur Rós", "Other"));
        assert!(!same_name("", ""));
    }

    /// Lookups that answer what the test sets and log the order asked.
    #[derive(Default)]
    struct Fake {
        asked: Mutex<Vec<&'static str>>,
        answers: Vec<(&'static str, &'static str)>,
        fail: Option<&'static str>,
    }

    impl Fake {
        fn with(answers: &[(&'static str, &'static str)]) -> Self {
            Self {
                answers: answers.to_vec(),
                ..Self::default()
            }
        }

        fn answer(&self, name: &'static str) -> Result<Option<String>, ResolveError> {
            self.asked.lock().unwrap().push(name);
            if self.fail == Some(name) {
                return Err(ProviderError::NotConfigured {
                    provider: "musicbrainz",
                    message: "down".into(),
                }
                .into());
            }
            Ok(self
                .answers
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, id)| (*id).to_owned()))
        }

        fn pair(&self, name: &'static str) -> Result<Option<Placed>, ResolveError> {
            Ok(self
                .answer(name)?
                .map(|id| (id.clone(), format!("rec-{id}"))))
        }

        fn asked(&self) -> Vec<&'static str> {
            self.asked.lock().unwrap().clone()
        }
    }

    macro_rules! fake_answers {
        ($($name:ident: $facts:ty => $how:ident -> $out:ty,)*) => {
            $(fn $name<'a>(&'a self, _: &'a $facts) -> BoxFuture<'a, Result<Option<$out>, ResolveError>> {
                Box::pin(async move { self.$how(stringify!($name)) })
            })*
        };
    }

    impl Lookups for Fake {
        fake_answers! {
            mb_track_by_isrc: TrackFacts => pair -> Placed,
            deezer_track_by_isrc: TrackFacts => pair -> Placed,
            mb_track_by_name: TrackFacts => pair -> Placed,
            deezer_track_by_name: TrackFacts => pair -> Placed,
            mb_album_by_barcode: AlbumFacts => answer -> String,
            mb_album_by_link: AlbumFacts => answer -> String,
            deezer_album_by_barcode: AlbumFacts => answer -> String,
            mb_album_by_name: AlbumFacts => answer -> String,
            deezer_album_by_name: AlbumFacts => answer -> String,
            mb_artist_by_link: ArtistFacts => answer -> String,
            mb_artist_by_album: ArtistFacts => answer -> String,
            mb_artist_by_name: ArtistFacts => answer -> String,
            deezer_artist_by_name: ArtistFacts => answer -> String,
        }
    }

    fn track() -> TrackFacts {
        TrackFacts {
            title: "Reckoner".into(),
            artists: vec!["Radiohead".into()],
            album: AlbumFacts {
                title: "In Rainbows".into(),
                artists: vec!["Radiohead".into()],
                ..AlbumFacts::default()
            },
            isrc: Some("GBSTK0700014".into()),
            seconds: Some(290),
        }
    }

    fn album() -> AlbumFacts {
        AlbumFacts {
            spotify_id: ID.into(),
            title: "In Rainbows".into(),
            artists: vec!["Radiohead".into()],
            upc: Some("634904032463".into()),
            total_tracks: Some(10),
        }
    }

    fn artist() -> ArtistFacts {
        ArtistFacts {
            spotify_id: ID.into(),
            name: "Radiohead".into(),
            album_titles: vec!["Kid A".into(), "OK Computer".into()],
            album_groups: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_track_resolves_by_isrc_first() {
        let fake = Fake::with(&[("mb_track_by_isrc", "g1"), ("mb_track_by_name", "g2")]);
        let hit = resolve_track(&track(), &fake).await.unwrap().unwrap();
        assert_eq!(
            (hit.album.id.as_str(), hit.album.source, hit.album.via),
            ("g1", Source::MusicBrainz, Via::Isrc)
        );
        assert_eq!(hit.recording, "rec-g1");
        assert_eq!(fake.asked(), vec!["mb_track_by_isrc"]);
    }

    #[tokio::test]
    async fn a_track_falls_back_to_deezer_isrc_then_names() {
        let fake = Fake::with(&[("deezer_track_by_isrc", "dz")]);
        let hit = resolve_track(&track(), &fake).await.unwrap().unwrap();
        assert_eq!(
            (hit.album.source, hit.album.via),
            (Source::Deezer, Via::Isrc)
        );

        let fake = Fake::with(&[("mb_track_by_name", "g")]);
        let hit = resolve_track(&track(), &fake).await.unwrap().unwrap();
        assert_eq!(
            (hit.album.source, hit.album.via),
            (Source::MusicBrainz, Via::Name)
        );
        assert_eq!(
            fake.asked(),
            vec![
                "mb_track_by_isrc",
                "deezer_track_by_isrc",
                "mb_track_by_name"
            ]
        );

        let fake = Fake::with(&[("deezer_track_by_name", "dz")]);
        let hit = resolve_track(&track(), &fake).await.unwrap().unwrap();
        assert_eq!(
            (hit.album.source, hit.album.via),
            (Source::Deezer, Via::Name)
        );

        let fake = Fake::default();
        assert_eq!(resolve_track(&track(), &fake).await.unwrap(), None);
        assert_eq!(fake.asked().len(), 4);
    }

    #[tokio::test]
    async fn a_track_without_isrc_or_length_is_never_guessed() {
        let fake = Fake::with(&[("mb_track_by_name", "g")]);
        let bare = TrackFacts {
            isrc: None,
            seconds: None,
            ..track()
        };
        assert_eq!(resolve_track(&bare, &fake).await.unwrap(), None);
        assert!(fake.asked().is_empty());
    }

    #[tokio::test]
    async fn a_failing_service_stops_the_resolution() {
        let fake = Fake {
            fail: Some("mb_track_by_isrc"),
            answers: vec![("deezer_track_by_isrc", "dz")],
            ..Fake::default()
        };
        assert!(resolve_track(&track(), &fake).await.is_err());
        assert_eq!(fake.asked(), vec!["mb_track_by_isrc"]);
    }

    #[tokio::test]
    async fn an_album_resolves_in_order() {
        for (answer, source, via) in [
            ("mb_album_by_barcode", Source::MusicBrainz, Via::Barcode),
            ("mb_album_by_link", Source::MusicBrainz, Via::Link),
            ("deezer_album_by_barcode", Source::Deezer, Via::Barcode),
            ("mb_album_by_name", Source::MusicBrainz, Via::Name),
            ("deezer_album_by_name", Source::Deezer, Via::Name),
        ] {
            let fake = Fake::with(&[(answer, "x")]);
            let hit = resolve_album(&album(), &fake).await.unwrap().unwrap();
            assert_eq!((hit.source, hit.via), (source, via), "{answer}");
            assert_eq!(fake.asked().last(), Some(&answer));
        }
        let fake = Fake::default();
        assert_eq!(resolve_album(&album(), &fake).await.unwrap(), None);
        assert_eq!(
            fake.asked(),
            vec![
                "mb_album_by_barcode",
                "mb_album_by_link",
                "deezer_album_by_barcode",
                "mb_album_by_name",
                "deezer_album_by_name"
            ]
        );
    }

    #[tokio::test]
    async fn an_album_without_barcode_skips_the_barcode_lookups() {
        let fake = Fake::default();
        let bare = AlbumFacts {
            upc: None,
            ..album()
        };
        assert_eq!(resolve_album(&bare, &fake).await.unwrap(), None);
        assert_eq!(
            fake.asked(),
            vec![
                "mb_album_by_link",
                "mb_album_by_name",
                "deezer_album_by_name"
            ]
        );
    }

    #[tokio::test]
    async fn an_artist_resolves_by_link_then_overlap_then_deezer() {
        let fake = Fake::with(&[("mb_artist_by_link", "a1"), ("mb_artist_by_name", "a2")]);
        let hit = resolve_artist(&artist(), &fake).await.unwrap().unwrap();
        assert_eq!((hit.id.as_str(), hit.via), ("a1", Via::Link));

        let fake = Fake::with(&[("mb_artist_by_name", "a2")]);
        let hit = resolve_artist(&artist(), &fake).await.unwrap().unwrap();
        assert_eq!((hit.source, hit.via), (Source::MusicBrainz, Via::Name));

        let fake = Fake::with(&[("deezer_artist_by_name", "dz")]);
        let hit = resolve_artist(&artist(), &fake).await.unwrap().unwrap();
        assert_eq!(hit.source, Source::Deezer);
        assert_eq!(
            fake.asked(),
            vec![
                "mb_artist_by_link",
                "mb_artist_by_name",
                "deezer_artist_by_name"
            ]
        );

        // No album titles (an oEmbed name): no MusicBrainz name match.
        let fake = Fake::with(&[("mb_artist_by_name", "a2")]);
        let named = ArtistFacts {
            album_titles: Vec::new(),
            ..artist()
        };
        assert_eq!(resolve_artist(&named, &fake).await.unwrap(), None);
        assert_eq!(
            fake.asked(),
            vec!["mb_artist_by_link", "deezer_artist_by_name"]
        );
    }

    #[tokio::test]
    async fn a_saved_album_names_an_artist_before_its_name_does() {
        let saved = ArtistFacts {
            album_groups: vec!["rg".into()],
            ..artist()
        };
        let fake = Fake::with(&[("mb_artist_by_album", "a3"), ("mb_artist_by_name", "a2")]);
        let hit = resolve_artist(&saved, &fake).await.unwrap().unwrap();
        assert_eq!((hit.id.as_str(), hit.via), ("a3", Via::Album));
        assert_eq!(
            fake.asked(),
            vec!["mb_artist_by_link", "mb_artist_by_album"]
        );

        // The name steps alone never ask for the link or the albums.
        let fake = Fake::with(&[("mb_artist_by_link", "a1"), ("deezer_artist_by_name", "dz")]);
        let hit = resolve_artist_named(&saved, &fake).await.unwrap().unwrap();
        assert_eq!(hit.source, Source::Deezer);
        assert_eq!(
            fake.asked(),
            vec!["mb_artist_by_name", "deezer_artist_by_name"]
        );
    }

    #[test]
    fn two_candidates_are_a_guess() {
        assert_eq!(
            only(&["a".to_owned(), "a".to_owned()]),
            Some("a".to_owned())
        );
        assert_eq!(only(&["a".to_owned(), "b".to_owned()]), None);
        assert_eq!(only::<String>(&[]), None);
    }
}
