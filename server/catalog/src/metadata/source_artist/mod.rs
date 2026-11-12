//! Fork: artist pages on stand-in ids.
//!
//! An artist known only to a metadata source (a Deezer artist the
//! library sources minted a stand-in id for) has no MusicBrainz browse,
//! no AudioDB images and no Wikidata links. Its page takes the library's
//! own discography, the source's picture from the album matches, and
//! Last.fm's biography by name (the stand-in id is never sent out).

use std::collections::HashSet;
use std::time::Duration;

use droppedneedle_kernel::clock::{Clock as _, SystemClock};
use droppedneedle_kernel::db::Lane;
use droppedneedle_library::sources::catalogue;
use droppedneedle_providers::preview::DeezerArtistAlbum;
use droppedneedle_providers::{CacheKey, Fetched, Policy, RequestPriority, Source};
use rusqlite::{OptionalExtension, params};

use super::Catalog;
use super::artist::Discography;
use super::error::CatalogError;
use super::models::{ArtistImages, ReleaseItem};

pub(crate) mod sql;

/// A source artist's discography listing: a day; an empty one an hour.
const SOURCE_CATALOGUE: Policy = Policy {
    fresh: Duration::from_secs(24 * 3600),
    miss: Duration::from_secs(3600),
};

impl Catalog {
    /// The source's picture of stand-in artist `mbid` as the page's
    /// images; `None` when no match carries one.
    pub(super) async fn source_artist_images(&self, mbid: &str) -> Option<ArtistImages> {
        let id = mbid.trim().to_lowercase();
        let url = self
            .local()
            .read("catalog.source_artist_picture", move |conn| {
                conn.prepare_cached(sql::SOURCE_ARTIST_PICTURE.text())?
                    .query_row(params![id], |row| row.get::<_, String>(0))
                    .optional()
            })
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "source artist picture unreadable");
                None
            })?;
        Some(ArtistImages {
            thumb_url: Some(url),
            ..ArtistImages::default()
        })
    }

    /// The albums the source credits to stand-in artist `mbid`, in the
    /// sections the source's record type names: the held ones, then every
    /// other album of the source's discography (fork: the catalogue).
    pub(super) async fn source_discography(&self, mbid: &str) -> Result<Discography, CatalogError> {
        let id = mbid.trim().to_lowercase();
        let rows: Vec<(String, String, Option<i32>, Option<String>)> = self
            .local()
            .read("catalog.source_artist_albums", move |conn| {
                conn.prepare_cached(sql::SOURCE_ARTIST_ALBUMS.text())?
                    .query_map(params![id], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })?
                    .collect()
            })
            .await
            .map_err(CatalogError::database)?;
        let mut sections = Discography::default();
        for (group, title, year, record_type) in rows {
            let (section, kind) = match record_type.as_deref() {
                Some("ep") => (&mut sections.eps, "EP"),
                Some("single") => (&mut sections.singles, "Single"),
                _ => (&mut sections.albums, "Album"),
            };
            if section.iter().any(|item| item.id == group) {
                continue;
            }
            section.push(ReleaseItem {
                id: group,
                title: Some(title),
                release_type: Some(kind.to_owned()),
                first_release_date: None,
                year,
                in_library: true,
                requested: false,
            });
        }
        self.add_source_catalogue(mbid, &mut sections).await;
        for section in [
            &mut sections.albums,
            &mut sections.eps,
            &mut sections.singles,
        ] {
            section.sort_by_key(|item| (item.year.is_none(), std::cmp::Reverse(item.year)));
        }
        Ok(sections)
    }

    /// Add the source's discography of stand-in artist `mbid` to
    /// `sections`: every listed album not already there, each on its own
    /// stand-in release group. The listing is cached a day; when the
    /// source is down the albums kept from an earlier listing still show.
    async fn add_source_catalogue(&self, mbid: &str, sections: &mut Discography) {
        let uuid = mbid.trim().to_lowercase();
        let artist = self
            .local()
            .read("catalog.source_catalogue_artist", move |conn| {
                catalogue::artist_of(conn, &uuid)
            })
            .await
            .ok()
            .flatten();
        let Some(artist) = artist else {
            return;
        };
        let Ok(deezer_id) = artist.parse::<i64>() else {
            return;
        };
        let key = CacheKey::new(Source::Previews, &["deezer", "artist-albums", &artist]);
        let client = self.upstream().deezer().clone();
        let listed = self
            .cached(
                &key,
                SOURCE_CATALOGUE,
                RequestPriority::UserInitiated,
                move |priority| async move {
                    let albums = client
                        .deezer_artist_albums(deezer_id, priority)
                        .await
                        .map_err(CatalogError::Unavailable)?;
                    Ok(if albums.is_empty() {
                        Fetched::Missing
                    } else {
                        Fetched::Found(albums)
                    })
                },
            )
            .await;
        match listed {
            Ok(Some(albums)) => self.keep_source_listing(&artist, albums).await,
            Ok(None) => {}
            Err(error) => tracing::info!(%error, "Deezer discography unavailable; showing kept"),
        }
        let key = artist.clone();
        let rows = match self
            .local()
            .read("catalog.source_catalogue_albums", move |conn| {
                catalogue::albums_of(conn, &key)
            })
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "source catalogue unreadable");
                return;
            }
        };
        let ids: Vec<String> = rows.iter().map(|row| row.release_group.clone()).collect();
        let (owned, requested) = self.album_flags(&ids).await;
        let listed: HashSet<String> = sections
            .albums
            .iter()
            .chain(&sections.eps)
            .chain(&sections.singles)
            .map(|item| item.id.to_ascii_lowercase())
            .collect();
        for row in rows {
            if listed.contains(&row.release_group) {
                continue;
            }
            let year = row.year();
            let (section, kind) = match row.record_type.as_deref() {
                Some("ep") => (&mut sections.eps, "EP"),
                Some("single") => (&mut sections.singles, "Single"),
                _ => (&mut sections.albums, "Album"),
            };
            let in_library = row.held || owned.contains(&row.release_group);
            section.push(ReleaseItem {
                requested: !in_library && requested.contains(&row.release_group),
                id: row.release_group,
                title: Some(row.title),
                release_type: Some(kind.to_owned()),
                first_release_date: row.release_date,
                year,
                in_library,
            });
        }
    }

    /// Keep Deezer artist `artist`'s listing, when it names an album not
    /// kept yet (each page view would otherwise write it again).
    async fn keep_source_listing(&self, artist: &str, albums: Vec<DeezerArtistAlbum>) {
        let key = artist.to_owned();
        let kept: HashSet<String> = self
            .local()
            .read("catalog.source_catalogue_kept", move |conn| {
                catalogue::albums_of(conn, &key).map(|rows| {
                    rows.into_iter()
                        .map(|row| row.provider_album_id)
                        .collect::<HashSet<_>>()
                })
            })
            .await
            .unwrap_or_default();
        if albums
            .iter()
            .all(|album| kept.contains(&album.id.to_string()))
        {
            return;
        }
        let artist = artist.to_owned();
        let now = SystemClock.now_ms();
        let written = self
            .local()
            .db()
            .write(
                Lane::Foreground,
                "catalog.source_catalogue_keep",
                move |tx| {
                    catalogue::keep_listing(tx, &artist, "", &albums, now)?;
                    Ok::<_, droppedneedle_kernel::db::DbError>(())
                },
            )
            .await;
        if let Err(error) = written {
            tracing::warn!(%error, "source catalogue could not be kept");
        }
    }
}
