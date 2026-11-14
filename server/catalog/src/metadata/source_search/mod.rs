//! Fork: Deezer beside MusicBrainz in search.
//!
//! Each remote search bucket (artists, albums) asks Deezer's free-text
//! search at the same time as MusicBrainz, under the same deadline, with
//! the hits cached like MusicBrainz's. MusicBrainz wins: a Deezer hit is
//! added only when no MusicBrainz hit is the same artist (folded name) or
//! the same album (folded artist and title, version words aside; Deezer's
//! search has no year). Each added hit gets its stand-in id, minted and
//! kept in the source catalogue, so it opens the normal artist or album
//! page and can be requested. Only the first page asks Deezer, and the
//! advanced setting `deezer_search_enabled` switches it off.

use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;

use droppedneedle_kernel::clock::{Clock as _, SystemClock};
use droppedneedle_kernel::db::Lane;
use droppedneedle_library::sources::catalogue;
use droppedneedle_library::sources::editions::base_title;
use droppedneedle_providers::preview::{DeezerArtistAlbum, DeezerSearchHit, norm};
use droppedneedle_providers::virtual_ids::{self, Kind};
use droppedneedle_providers::{CacheKey, Fetched, Policy, RequestPriority, Source};
use serde::{Deserialize, Serialize};

use super::Catalog;
use super::error::CatalogError;
use super::mapping;
use super::search::{RemoteBucket, RemoteHit, RemoteKind};

/// Deezer hits asked for per bucket.
const DEEZER_HITS: u32 = 15;

/// One bucket's Deezer hits as cached.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SourceHits {
    items: Vec<DeezerSearchHit>,
}

/// The fold two names are compared under.
fn fold(text: &str) -> String {
    norm(text)
}

/// An album's dedupe key: folded artist and title, version words aside.
fn album_key(artist: Option<&str>, title: &str) -> (String, String) {
    (fold(artist.unwrap_or_default()), base_title(title))
}

/// Deezer's record type as MusicBrainz's primary and secondary types.
fn types(record_type: Option<&str>) -> (&'static str, Vec<String>) {
    match record_type.map(str::to_ascii_lowercase).as_deref() {
        Some("ep") => ("EP", Vec::new()),
        Some("single") => ("Single", Vec::new()),
        Some("compile") => ("Album", vec!["Compilation".to_owned()]),
        _ => ("Album", Vec::new()),
    }
}

/// The Deezer hits of `kind` with no MusicBrainz equivalent among
/// `musicbrainz`, as search hits on their stand-in ids, scored below
/// MusicBrainz's confident hits and in Deezer's order. `allowed` says
/// whether an album of the given types passes the release-type filter.
pub(crate) fn merge_hits(
    kind: RemoteKind,
    musicbrainz: &[RemoteHit],
    deezer: &[DeezerSearchHit],
    allowed: impl Fn(&str, &[String]) -> bool,
) -> Vec<RemoteHit> {
    let mut seen: HashSet<(String, String)> = musicbrainz
        .iter()
        .map(|hit| match kind {
            RemoteKind::Artist => (fold(&hit.title), String::new()),
            RemoteKind::Album => album_key(hit.artist.as_deref(), &hit.title),
        })
        .collect();
    let mut out = Vec::new();
    for (rank, hit) in deezer.iter().enumerate() {
        let id = hit.id.to_string();
        let artist = hit.artist.as_ref().map(|artist| artist.name.clone());
        let (key, mbid, type_info) = match kind {
            RemoteKind::Artist => (
                (fold(&hit.title), String::new()),
                virtual_ids::mint(catalogue::PROVIDER, Kind::Artist, &id),
                None,
            ),
            RemoteKind::Album => {
                let (primary, secondary) = types(hit.record_type.as_deref());
                if !allowed(primary, &secondary) {
                    continue;
                }
                (
                    album_key(artist.as_deref(), &hit.title),
                    virtual_ids::mint(catalogue::PROVIDER, Kind::ReleaseGroup, &id),
                    mapping::type_info(Some(primary), &secondary),
                )
            }
        };
        if key.0.is_empty() || !seen.insert(key) {
            continue;
        }
        out.push(RemoteHit {
            kind,
            mbid,
            title: hit.title.clone(),
            artist: match kind {
                RemoteKind::Artist => None,
                RemoteKind::Album => artist,
            },
            year: None,
            disambiguation: None,
            type_info,
            score: 60_i32
                .saturating_sub(i32::try_from(rank).unwrap_or(60) * 2)
                .max(1),
            // Fork: Deezer gives no release count; an exact title still
            // ranks as exact, after MusicBrainz's exact groups that have one.
            releases: None,
        });
    }
    out
}

impl Catalog {
    /// `musicbrainz`'s bucket of `kind` for `query`, with Deezer's hits
    /// that MusicBrainz lacks after it (first page only, when switched on).
    pub(super) async fn merge_source_hits(
        &self,
        kind: RemoteKind,
        query: &str,
        offset: u32,
        deadline: Duration,
        musicbrainz: impl Future<Output = RemoteBucket>,
    ) -> RemoteBucket {
        if offset > 0 || !self.upstream().settings().advanced().deezer_search_enabled {
            return musicbrainz.await;
        }
        let (mut bucket, deezer) = tokio::join!(
            musicbrainz,
            tokio::time::timeout(deadline, self.deezer_hits(kind, query))
        );
        let Ok(deezer) = deezer else {
            tracing::info!("deezer search missed its deadline");
            return bucket;
        };
        let preferences = self.upstream().settings().preferences();
        let primary = mapping::type_set(&preferences.primary_types);
        let secondary = mapping::type_set(&preferences.secondary_types);
        let added = merge_hits(kind, &bucket.hits, &deezer, |kind, secondaries| {
            mapping::should_include_release(
                Some(kind),
                secondaries,
                Some(&primary),
                Some(&secondary),
                true,
            )
        });
        if added.is_empty() {
            return bucket;
        }
        self.keep_found(kind, &deezer, &added).await;
        bucket.hits.extend(added);
        bucket
    }

    /// Deezer's hits of `kind` for `query`, cached like MusicBrainz's; a
    /// failed search is no hits.
    async fn deezer_hits(&self, kind: RemoteKind, query: &str) -> Vec<DeezerSearchHit> {
        let (label, path) = match kind {
            RemoteKind::Artist => ("deezer_artist_search", "artist"),
            RemoteKind::Album => ("deezer_album_search", "album"),
        };
        let text = query.trim().to_owned();
        let limit = DEEZER_HITS.to_string();
        let key = CacheKey::hashed(Source::Previews, label, &[text.as_str(), limit.as_str()]);
        let client = self.upstream().deezer().clone();
        let found = self
            .cached(
                &key,
                Policy::SEARCH,
                RequestPriority::UserInitiated,
                move |priority| async move {
                    let items = client
                        .deezer_search_text(path, &text, DEEZER_HITS, priority)
                        .await
                        .map_err(CatalogError::Unavailable)?;
                    Ok(if items.is_empty() {
                        Fetched::Missing
                    } else {
                        Fetched::Found(SourceHits { items })
                    })
                },
            )
            .await;
        match found {
            Ok(hits) => hits.unwrap_or_default().items,
            Err(error) => {
                tracing::info!(%error, "deezer search failed");
                Vec::new()
            }
        }
    }

    /// Keep the Deezer hits that were added, so their stand-in ids name
    /// them: albums in the source catalogue under their artist, artists
    /// with their name and picture.
    async fn keep_found(&self, kind: RemoteKind, deezer: &[DeezerSearchHit], added: &[RemoteHit]) {
        let ids: HashSet<&str> = added.iter().map(|hit| hit.mbid.as_str()).collect();
        let wanted: Vec<DeezerSearchHit> = deezer
            .iter()
            .filter(|hit| {
                let id = hit.id.to_string();
                let kind = match kind {
                    RemoteKind::Artist => Kind::Artist,
                    RemoteKind::Album => Kind::ReleaseGroup,
                };
                ids.contains(virtual_ids::mint(catalogue::PROVIDER, kind, &id).as_str())
            })
            .cloned()
            .collect();
        let now = SystemClock.now_ms();
        let written = self
            .local()
            .db()
            .write(Lane::Foreground, "catalog.source_search_keep", move |tx| {
                for hit in &wanted {
                    match kind {
                        RemoteKind::Artist => {
                            catalogue::keep_artist(
                                tx,
                                &hit.id.to_string(),
                                &hit.title,
                                hit.image_url.as_deref(),
                                now,
                            )?;
                        }
                        RemoteKind::Album => {
                            let Some(artist) = hit.artist.as_ref() else {
                                continue;
                            };
                            let Some(artist_id) = artist.id.map(|id| id.to_string()) else {
                                continue;
                            };
                            catalogue::keep_artist(tx, &artist_id, &artist.name, None, now)?;
                            let listed = DeezerArtistAlbum {
                                id: hit.id,
                                title: hit.title.clone(),
                                release_date: None,
                                record_type: hit.record_type.clone(),
                                cover_url: hit.image_url.clone(),
                            };
                            catalogue::keep_listing(tx, &artist_id, &artist.name, &[listed], now)?;
                        }
                    }
                }
                Ok::<_, droppedneedle_kernel::db::DbError>(())
            })
            .await;
        if let Err(error) = written {
            tracing::warn!(%error, "deezer search hits could not be kept");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_providers::preview::NamedRef;

    fn mb(kind: RemoteKind, title: &str, artist: Option<&str>) -> RemoteHit {
        RemoteHit {
            kind,
            mbid: "b1392450-e666-3926-a536-22c65f834433".into(),
            title: title.into(),
            artist: artist.map(str::to_owned),
            year: Some(1997),
            disambiguation: None,
            type_info: None,
            score: 100,
            releases: None,
        }
    }

    fn dz(id: i64, title: &str, artist: Option<&str>, kind: Option<&str>) -> DeezerSearchHit {
        DeezerSearchHit {
            id,
            title: title.into(),
            record_type: kind.map(str::to_owned),
            artist: artist.map(|name| NamedRef {
                id: Some(9),
                name: name.into(),
            }),
            image_url: None,
        }
    }

    #[test]
    fn musicbrainz_wins_and_only_new_albums_are_added() {
        let musicbrainz = vec![mb(RemoteKind::Album, "OK Computer", Some("Radiohead"))];
        let deezer = vec![
            dz(1, "OK Computer", Some("Radiohead"), Some("album")),
            dz(
                2,
                "OK Computer (Collector's Edition)",
                Some("radiohead"),
                Some("album"),
            ),
            dz(3, "Rare Record", Some("Akira Ræ"), Some("ep")),
            dz(4, "A Single", Some("Akira Ræ"), Some("single")),
        ];
        let added = merge_hits(RemoteKind::Album, &musicbrainz, &deezer, |kind, _| {
            kind != "Single"
        });
        assert_eq!(added.len(), 1, "{added:?}");
        assert_eq!(added[0].title, "Rare Record");
        assert_eq!(
            added[0].mbid,
            virtual_ids::mint("deezer", Kind::ReleaseGroup, "3")
        );
        assert_eq!(added[0].artist.as_deref(), Some("Akira Ræ"));
        assert!(added[0].score < 100);
    }

    #[test]
    fn artists_musicbrainz_knows_are_not_added() {
        let musicbrainz = vec![mb(RemoteKind::Artist, "Radiohead", None)];
        let deezer = vec![
            dz(5, "radiohead", None, None),
            dz(6, "BASECAMP", None, None),
            dz(7, "Basecamp", None, None),
        ];
        let added = merge_hits(RemoteKind::Artist, &musicbrainz, &deezer, |_, _| true);
        assert_eq!(added.len(), 1, "{added:?}");
        assert_eq!(
            added[0].mbid,
            virtual_ids::mint("deezer", Kind::Artist, "6")
        );
    }
}
