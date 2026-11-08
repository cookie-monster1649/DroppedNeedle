//! Fork: matching held files to a song from the owner's Spotify playlists.
//!
//! A held file MusicBrainz cannot place (no release, or a release whose
//! names disagree with the file) can be matched to a Spotify song the
//! owner imported in a playlist instead: the file goes in as tagged on a
//! release made from Spotify's title, artist, album and length, with the
//! Spotify track id and ISRC as source links, as a want's file does
//! (`service::want_release`). No MusicBrainz id is sealed, so the album is
//! offered to identification afterwards.

use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseTrack, SourceLink};
use rusqlite::{OptionalExtension as _, Row, params};
use serde::Serialize;
use utoipa::ToSchema;

use super::sql;
use super::store::HeldRows;
use crate::requests::identities::WantIdentityProvider;

/// How many songs one search answers.
pub const CANDIDATE_LIMIT: i64 = 20;

/// A song from the owner's Spotify playlists a held file can be matched to.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SpotifyCandidate {
    /// The playlist entry it was read from (what a match names).
    pub entry_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_seconds: Option<f64>,
    pub cover_url: Option<String>,
    pub spotify_track_id: Option<String>,
    pub isrc: Option<String>,
}

impl SpotifyCandidate {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let duration: Option<i64> = row.get(4)?;
        #[allow(clippy::cast_precision_loss)]
        Ok(Self {
            entry_id: row.get(0)?,
            title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            artist: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            album: row
                .get::<_, Option<String>>(3)?
                .filter(|album| !album.trim().is_empty()),
            duration_seconds: duration.filter(|ms| *ms > 0).map(|ms| ms as f64 / 1000.0),
            cover_url: row.get(5)?,
            spotify_track_id: row.get(6)?,
            isrc: row.get(7)?,
        })
    }

    /// Its ids as the source links a placed track adopts.
    pub fn links(&self) -> Vec<SourceLink> {
        [
            (
                WantIdentityProvider::Spotify,
                self.spotify_track_id.as_deref(),
            ),
            (WantIdentityProvider::Isrc, self.isrc.as_deref()),
        ]
        .into_iter()
        .filter_map(|(provider, id)| {
            let id = id?;
            Some(SourceLink {
                provider: provider.as_str().to_owned(),
                entity_type: provider.entity_type().to_owned(),
                canonical_url: provider.canonical_url(id),
                external_id: id.to_owned(),
            })
        })
        .collect()
    }

    /// The release `files` held files are placed on: one track per file,
    /// titled as on Spotify, on its Spotify album (a lone song with no
    /// album is its own single).
    pub fn release(&self, files: usize) -> Release {
        let links = self.links();
        let isrcs: Vec<String> = self.isrc.iter().cloned().collect();
        let credit = vec![CreditedArtist {
            name: if self.artist.trim().is_empty() {
                "Unknown Artist".to_owned()
            } else {
                self.artist.clone()
            },
            ..CreditedArtist::default()
        }];
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let length = self.duration_seconds.map(|s| (s * 1000.0) as u64);
        let tracks = (0..files)
            .map(|index| {
                let position = u32::try_from(index + 1).unwrap_or(u32::MAX);
                ReleaseTrack {
                    title: self.title.clone(),
                    artists: credit.clone(),
                    disc: 1,
                    position,
                    absolute_position: position,
                    length_ms: length,
                    isrcs: isrcs.clone(),
                    links: links.clone(),
                    ..ReleaseTrack::default()
                }
            })
            .collect();
        Release {
            title: self.album.clone().unwrap_or_else(|| self.title.clone()),
            artists: credit,
            primary_type: Some(
                if self.album.is_some() {
                    "Album"
                } else {
                    "Single"
                }
                .to_owned(),
            ),
            tracks,
            ..Release::default()
        }
    }
}

impl HeldRows<'_> {
    /// Songs of `user_id`'s Spotify playlists matching `query`.
    pub fn spotify_candidates(
        &self,
        user_id: &str,
        query: &str,
    ) -> rusqlite::Result<Vec<SpotifyCandidate>> {
        let pattern = format!("%{}%", query.trim().to_lowercase());
        let mut statement = self.conn().prepare_cached(sql::SPOTIFY_CANDIDATES.text())?;
        statement
            .query_map(
                params![user_id, pattern, CANDIDATE_LIMIT],
                SpotifyCandidate::from_row,
            )?
            .collect()
    }

    /// One song of `user_id`'s Spotify playlists.
    pub fn spotify_candidate(
        &self,
        user_id: &str,
        entry_id: &str,
    ) -> rusqlite::Result<Option<SpotifyCandidate>> {
        self.conn()
            .query_row(
                sql::SPOTIFY_CANDIDATE.text(),
                params![user_id, entry_id],
                SpotifyCandidate::from_row,
            )
            .optional()
    }
}
