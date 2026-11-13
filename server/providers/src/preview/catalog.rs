//! Fork: Deezer's catalogue, for albums MusicBrainz does not know.
//!
//! Check albums suggests a Deezer album for an album with no MusicBrainz
//! candidate. These reads go through the same executor and rate row as the
//! previews. Deezer's API is keyless. Shapes verified against api.deezer.com
//! on 2026-11-11 (`/search/album`, `/album/153601242`, `/track/isrc:...`,
//! `/artist/8785546`). `/search/artist` is read as the artist detail's
//! fields in a `data` list, the shape the album search has; it was not
//! recorded. An unknown id answers the error object with code 800, which
//! the classifier reads as absence.

use serde::{Deserialize, Serialize};

use super::PreviewClient;
use crate::RequestPriority;
use crate::client::decode;
use crate::error::ProviderError;

/// One album hit of `/search/album`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeezerAlbumHit {
    pub id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub nb_tracks: Option<i64>,
    #[serde(default)]
    pub record_type: Option<String>,
    #[serde(default)]
    pub artist: Option<NamedRef>,
}

/// An id and a name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedRef {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default, alias = "title")]
    pub name: String,
}

#[derive(Debug, Deserialize)]
struct Hits<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct Genre {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct WireArtist {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    picture_xl: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireTrack {
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    duration: Option<i64>,
    #[serde(default)]
    isrc: Option<String>,
    #[serde(default)]
    track_position: Option<i64>,
    #[serde(default)]
    disk_number: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct WireAlbum {
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    upc: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    record_type: Option<String>,
    #[serde(default)]
    cover_xl: Option<String>,
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    genres: Option<Hits<Genre>>,
    #[serde(default)]
    artist: Option<WireArtist>,
    #[serde(default)]
    tracks: Option<Hits<WireTrack>>,
}

/// Fork: an answer read for its id alone; Deezer answers a miss with an
/// `error` object and no id.
#[derive(Debug, Deserialize)]
struct WireId {
    #[serde(default)]
    id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct WireIsrcTrack {
    #[allow(dead_code)]
    id: i64,
    #[serde(default)]
    album: Option<NamedRef>,
}

/// One track of a Deezer album, in album order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeezerAlbumTrack {
    pub id: i64,
    pub title: String,
    /// Seconds.
    pub duration: Option<i64>,
    pub isrc: Option<String>,
    pub position: i64,
    pub disc: i64,
}

/// One Deezer album with what an album page shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeezerAlbum {
    pub id: i64,
    pub title: String,
    pub artist_name: String,
    pub artist_id: Option<i64>,
    pub artist_picture: Option<String>,
    pub upc: Option<String>,
    pub label: Option<String>,
    /// `YYYY-MM-DD`.
    pub release_date: Option<String>,
    /// `album`, `ep`, `single` or `compile`.
    pub record_type: Option<String>,
    pub cover_url: Option<String>,
    pub link: Option<String>,
    pub genres: Vec<String>,
    pub tracks: Vec<DeezerAlbumTrack>,
}

/// One Deezer artist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeezerArtistDetail {
    pub id: i64,
    pub name: String,
    pub picture: Option<String>,
    pub link: Option<String>,
    pub nb_album: Option<i64>,
    pub nb_fan: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct WireArtistDetail {
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    picture_xl: Option<String>,
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    nb_album: Option<i64>,
    #[serde(default)]
    nb_fan: Option<i64>,
}

/// Fork: one album of an artist's Deezer discography (`/artist/{id}/albums`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeezerArtistAlbum {
    pub id: i64,
    #[serde(default)]
    pub title: String,
    /// `YYYY-MM-DD`.
    #[serde(default)]
    pub release_date: Option<String>,
    /// `album`, `ep`, `single` or `compile`.
    #[serde(default)]
    pub record_type: Option<String>,
    #[serde(default, alias = "cover_xl")]
    pub cover_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ArtistAlbumsPage {
    #[serde(default = "Vec::new")]
    data: Vec<DeezerArtistAlbum>,
    #[serde(default)]
    next: Option<String>,
}

/// Fork: one hit of Deezer's free-text album or artist search, as search
/// results show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeezerSearchHit {
    pub id: i64,
    /// Album title or artist name.
    #[serde(default, alias = "name")]
    pub title: String,
    /// Albums: `album`, `ep`, `single` or `compile`.
    #[serde(default)]
    pub record_type: Option<String>,
    /// Albums: the artist.
    #[serde(default)]
    pub artist: Option<NamedRef>,
    /// Album cover or artist picture.
    #[serde(default, alias = "cover_xl", alias = "picture_xl")]
    pub image_url: Option<String>,
}

/// Albums read from one artist's discography at most.
pub const ARTIST_ALBUMS_CAP: usize = 300;
const ARTIST_ALBUMS_PAGE: usize = 100;

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

impl PreviewClient {
    /// Albums by `artist` titled `title`, best first, at most `limit`.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn deezer_search_albums(
        &self,
        artist: &str,
        title: &str,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<DeezerAlbumHit>, ProviderError> {
        let clean = |value: &str| value.replace('"', " ");
        let query = if artist.trim().is_empty() {
            format!(r#"album:"{}""#, clean(title))
        } else {
            format!(r#"artist:"{}" album:"{}""#, clean(artist), clean(title))
        };
        let limit = limit.clamp(1, 25).to_string();
        let Some(reply) = self
            .deezer_get(
                &["search", "album"],
                &[("q", query.as_str()), ("limit", limit.as_str())],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let hits: Hits<DeezerAlbumHit> = decode(&reply)?;
        Ok(hits.data)
    }

    /// The album a track with this ISRC is on, by Deezer's own pick.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn deezer_album_of_isrc(
        &self,
        isrc: &str,
        priority: RequestPriority,
    ) -> Result<Option<i64>, ProviderError> {
        let isrc = isrc.trim();
        if isrc.is_empty() || !isrc.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Ok(None);
        }
        let segment = format!("isrc:{}", isrc.to_ascii_uppercase());
        let Some(reply) = self
            .deezer_get(&["track", segment.as_str()], &[], priority)
            .await?
        else {
            return Ok(None);
        };
        let track: WireIsrcTrack = decode(&reply)?;
        Ok(track.album.and_then(|album| album.id))
    }

    /// Fork: the Deezer album carrying barcode `upc` (`/album/upc:<UPC>`),
    /// with its track list; `None` when Deezer has none.
    ///
    /// # Errors
    ///
    /// The typed failure of a call or of its answer.
    pub async fn deezer_album_of_upc(
        &self,
        upc: &str,
        priority: RequestPriority,
    ) -> Result<Option<DeezerAlbum>, ProviderError> {
        let upc: String = upc.chars().filter(char::is_ascii_digit).collect();
        if upc.len() < 8 {
            return Ok(None);
        }
        let segment = format!("upc:{upc}");
        let Some(reply) = self
            .deezer_get(&["album", segment.as_str()], &[], priority)
            .await?
        else {
            return Ok(None);
        };
        let found: WireId = decode(&reply)?;
        match found.id {
            Some(id) if id > 0 => self.deezer_album(id, priority).await,
            _ => Ok(None),
        }
    }

    /// One Deezer album with its track list, `None` when Deezer has none.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn deezer_album(
        &self,
        album_id: i64,
        priority: RequestPriority,
    ) -> Result<Option<DeezerAlbum>, ProviderError> {
        let id = album_id.to_string();
        let Some(reply) = self
            .deezer_get(&["album", id.as_str()], &[], priority)
            .await?
        else {
            return Ok(None);
        };
        let album: WireAlbum = decode(&reply)?;
        if album.id != album_id {
            return Ok(None);
        }
        let artist = album.artist;
        let tracks = album
            .tracks
            .map(|tracks| tracks.data)
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(index, track)| DeezerAlbumTrack {
                id: track.id,
                title: track.title,
                duration: track.duration,
                isrc: nonempty(track.isrc),
                position: track
                    .track_position
                    .unwrap_or(i64::try_from(index).unwrap_or(0) + 1),
                disc: track.disk_number.unwrap_or(1),
            })
            .collect();
        Ok(Some(DeezerAlbum {
            id: album.id,
            title: album.title,
            artist_name: artist.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            artist_id: artist.as_ref().and_then(|a| a.id),
            artist_picture: nonempty(artist.and_then(|a| a.picture_xl)),
            upc: nonempty(album.upc),
            label: nonempty(album.label),
            release_date: nonempty(album.release_date).filter(|d| d != "0000-00-00"),
            record_type: nonempty(album.record_type),
            cover_url: nonempty(album.cover_xl),
            link: nonempty(album.link),
            genres: album
                .genres
                .map(|genres| genres.data)
                .unwrap_or_default()
                .into_iter()
                .map(|genre| genre.name)
                .filter(|name| !name.is_empty())
                .collect(),
            tracks,
        }))
    }

    /// One Deezer artist, `None` when Deezer has none.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn deezer_artist(
        &self,
        artist_id: i64,
        priority: RequestPriority,
    ) -> Result<Option<DeezerArtistDetail>, ProviderError> {
        let id = artist_id.to_string();
        let Some(reply) = self
            .deezer_get(&["artist", id.as_str()], &[], priority)
            .await?
        else {
            return Ok(None);
        };
        let artist: WireArtistDetail = decode(&reply)?;
        if artist.id != artist_id {
            return Ok(None);
        }
        Ok(Some(DeezerArtistDetail {
            id: artist.id,
            name: artist.name,
            picture: nonempty(artist.picture_xl),
            link: nonempty(artist.link),
            nb_album: artist.nb_album,
            nb_fan: artist.nb_fan,
        }))
    }

    /// Artists named like `name`, best first, at most `limit`.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn deezer_search_artists(
        &self,
        name: &str,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<DeezerArtistDetail>, ProviderError> {
        let query = format!(r#"artist:"{}""#, name.replace('"', " "));
        let limit = limit.clamp(1, 25).to_string();
        let Some(reply) = self
            .deezer_get(
                &["search", "artist"],
                &[("q", query.as_str()), ("limit", limit.as_str())],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let hits: Hits<WireArtistDetail> = decode(&reply)?;
        Ok(hits
            .data
            .into_iter()
            .map(|artist| DeezerArtistDetail {
                id: artist.id,
                name: artist.name,
                picture: nonempty(artist.picture_xl),
                link: nonempty(artist.link),
                nb_album: artist.nb_album,
                nb_fan: artist.nb_fan,
            })
            .collect())
    }

    /// Fork: the albums, EPs and singles of Deezer artist `artist_id`, as
    /// Deezer lists them, at most [`ARTIST_ALBUMS_CAP`]. Empty when Deezer
    /// has no such artist.
    ///
    /// # Errors
    ///
    /// The typed failure of a call or of its answer.
    pub async fn deezer_artist_albums(
        &self,
        artist_id: i64,
        priority: RequestPriority,
    ) -> Result<Vec<DeezerArtistAlbum>, ProviderError> {
        let id = artist_id.to_string();
        let limit = ARTIST_ALBUMS_PAGE.to_string();
        let mut out: Vec<DeezerArtistAlbum> = Vec::new();
        while out.len() < ARTIST_ALBUMS_CAP {
            let index = out.len().to_string();
            let Some(reply) = self
                .deezer_get(
                    &["artist", id.as_str(), "albums"],
                    &[("index", index.as_str()), ("limit", limit.as_str())],
                    priority,
                )
                .await?
            else {
                break;
            };
            let page: ArtistAlbumsPage = decode(&reply)?;
            let count = page.data.len();
            for mut album in page.data {
                album.release_date = nonempty(album.release_date).filter(|d| d != "0000-00-00");
                album.record_type = nonempty(album.record_type);
                album.cover_url = nonempty(album.cover_url);
                if !out.iter().any(|kept| kept.id == album.id) {
                    out.push(album);
                }
            }
            if count == 0 || page.next.is_none() {
                break;
            }
        }
        out.truncate(ARTIST_ALBUMS_CAP);
        Ok(out)
    }

    /// Fork: Deezer's free-text search of `kind` (`album` or `artist`),
    /// best first, at most `limit`.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn deezer_search_text(
        &self,
        kind: &str,
        text: &str,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<DeezerSearchHit>, ProviderError> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 50).to_string();
        let Some(reply) = self
            .deezer_get(
                &["search", kind],
                &[("q", text), ("limit", limit.as_str())],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let hits: Hits<DeezerSearchHit> = decode(&reply)?;
        Ok(hits
            .data
            .into_iter()
            .map(|mut hit| {
                hit.image_url = nonempty(hit.image_url);
                hit.record_type = nonempty(hit.record_type);
                hit
            })
            .filter(|hit| !hit.title.trim().is_empty())
            .collect())
    }
}
