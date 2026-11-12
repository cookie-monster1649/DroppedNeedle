//! Fork: artist pages on stand-in ids.
//!
//! An artist known only to a metadata source (a Deezer artist the
//! library sources minted a stand-in id for) has no MusicBrainz browse,
//! no AudioDB images and no Wikidata links. Its page takes the library's
//! own discography, the source's picture from the album matches, and
//! Last.fm's biography by name (the stand-in id is never sent out).

use rusqlite::{OptionalExtension, params};

use super::Catalog;
use super::artist::Discography;
use super::error::CatalogError;
use super::models::{ArtistImages, ReleaseItem};

pub(crate) mod sql;

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

    /// The held albums the source credits to stand-in artist `mbid`, in
    /// the sections the source's record type names.
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
        Ok(sections)
    }
}
