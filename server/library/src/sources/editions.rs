//! Fork: the other editions Deezer has of an album on stand-in ids.
//!
//! Deezer often lists one record more than once: explicit and clean,
//! deluxe, remastered, a later reissue. Each album on stand-in ids is
//! looked up once a month: Deezer's albums by the same artist whose title,
//! version words aside, is the album's are kept in `source_editions` with
//! their track lists, and their releases minted. The release group the
//! album page reads then lists them as editions beside the matched one,
//! and a request can ask for any of them.

use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_providers::preview::{DeezerAlbum, PreviewClient, norm};
use droppedneedle_providers::virtual_ids::Kind;
use droppedneedle_providers::{ProviderError, RequestPriority};
use rusqlite::params;

use super::{mint, sql};
use crate::jobs::models::OperationError;
use crate::provider_match::{DEEZER, ProviderMatch, match_of};

/// How long an album's editions stand before Deezer is asked again.
pub const EDITION_RECHECK_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Search hits looked at per album.
const SEARCH_HITS: u32 = 15;
/// Editions kept per album at most.
const MAX_EDITIONS: usize = 6;

/// A title with its bracketed version words dropped, folded: "Halo
/// (Deluxe Edition) [Remastered]" and "Halo" read the same.
pub fn base_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut depth = 0usize;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let trimmed = out
        .split(" - ")
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    norm(&trimmed)
}

/// Whether Deezer album `found` (by `artist_id` / `artist_name`) is an
/// edition of `of`: another album, the same artist, the same title.
pub fn is_edition_of(of: &ProviderMatch, id: i64, title: &str, artist: Option<i64>) -> bool {
    if id.to_string() == of.provider_album_id {
        return false;
    }
    let same_artist = match (artist, of.provider_artist_id.as_deref()) {
        (Some(artist), Some(ours)) => artist.to_string() == ours,
        _ => false,
    };
    same_artist && !base_title(title).is_empty() && base_title(title) == base_title(&of.title)
}

/// Looks up and keeps the Deezer editions of albums on stand-in ids.
pub struct SourceEditions {
    db: Db,
    deezer: PreviewClient,
    clock: Arc<dyn Clock>,
}

impl SourceEditions {
    /// Editions read through `deezer`, kept in `db`.
    pub fn new(db: Db, deezer: PreviewClient, clock: Arc<dyn Clock>) -> Self {
        Self { db, deezer, clock }
    }

    /// Look up the editions of at most `batch` albums due a check.
    /// Answers how many albums were checked.
    ///
    /// # Errors
    ///
    /// A store fault. A Deezer failure leaves the album due.
    pub async fn check_some(&self, batch: u32) -> Result<usize, OperationError> {
        let now = self.clock.now_ms();
        let due: Vec<ProviderMatch> = self
            .db
            .read("sources.editions_due", move |conn| {
                let ids: Vec<String> = conn
                    .prepare_cached(sql::EDITIONS_DUE.text())?
                    .query_map(params![now - EDITION_RECHECK_MS, batch], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                let mut found = Vec::new();
                for id in ids {
                    if let Some(one) = match_of(conn, &id)? {
                        found.push(one);
                    }
                }
                Ok::<_, OperationError>(found)
            })
            .await?;
        let mut checked = 0;
        for album in due {
            match self.editions_of(&album).await {
                Ok(editions) => {
                    self.keep(album, editions, now).await?;
                    checked += 1;
                }
                Err(error) => {
                    tracing::info!(%error, album = %album.album_id, "Deezer editions unavailable; asking later");
                }
            }
        }
        Ok(checked)
    }

    /// Deezer's other albums that are editions of `album`.
    async fn editions_of(&self, album: &ProviderMatch) -> Result<Vec<DeezerAlbum>, ProviderError> {
        let priority = RequestPriority::BackgroundSync;
        let hits = self
            .deezer
            .deezer_search_albums(&album.artist_name, &album.title, SEARCH_HITS, priority)
            .await?;
        let mut out = Vec::new();
        for hit in hits {
            let artist = hit.artist.as_ref().and_then(|artist| artist.id);
            if !is_edition_of(album, hit.id, &hit.title, artist)
                || out.iter().any(|kept: &DeezerAlbum| kept.id == hit.id)
            {
                continue;
            }
            if let Some(found) = self.deezer.deezer_album(hit.id, priority).await?
                && !found.tracks.is_empty()
                && is_edition_of(album, found.id, &found.title, found.artist_id)
            {
                out.push(found);
            }
            if out.len() >= MAX_EDITIONS {
                break;
            }
        }
        Ok(out)
    }

    /// Replace `album`'s editions with `editions` and note the check.
    async fn keep(
        &self,
        album: ProviderMatch,
        editions: Vec<DeezerAlbum>,
        now: i64,
    ) -> Result<(), OperationError> {
        self.db
            .write(Lane::Background, "sources.keep_editions", move |tx| {
                keep_in(tx, &album, &editions, now)?;
                Ok::<_, OperationError>(())
            })
            .await
    }
}

/// Replace `album`'s editions with `editions` in `conn` and note the check
/// at `now`: each edition kept with its track list, and its release,
/// release tracks and recordings minted.
///
/// # Errors
///
/// A store fault.
pub fn keep_in(
    conn: &rusqlite::Connection,
    album: &ProviderMatch,
    editions: &[DeezerAlbum],
    now: i64,
) -> rusqlite::Result<()> {
    conn.prepare_cached(sql::DROP_EDITIONS.text())?
        .execute(params![DEEZER, album.provider_album_id])?;
    for edition in editions {
        let id = edition.id.to_string();
        let tracks: Vec<crate::provider_match::MatchTrack> = edition
            .tracks
            .iter()
            .map(|track| crate::provider_match::MatchTrack {
                provider_track_id: track.id.to_string(),
                title: track.title.clone(),
                duration_seconds: track.duration.map(|seconds| seconds as f64),
                disc: track.disc,
                position: track.position,
                isrc: track.isrc.clone(),
            })
            .collect();
        conn.prepare_cached(sql::KEEP_EDITION.text())?
            .execute(params![
                DEEZER,
                id,
                album.provider_album_id,
                edition.title,
                edition.release_date,
                edition.upc,
                edition.label,
                edition.record_type,
                edition.cover_url,
                serde_json::to_string(&tracks).unwrap_or_else(|_| "[]".to_owned()),
                now,
            ])?;
        mint(conn, DEEZER, Kind::Release, &id, Some(&id), now)?;
        for track in &tracks {
            mint(
                conn,
                DEEZER,
                Kind::ReleaseTrack,
                &track.provider_track_id,
                Some(&id),
                now,
            )?;
            mint(
                conn,
                DEEZER,
                Kind::Recording,
                &track.provider_track_id,
                Some(&id),
                now,
            )?;
        }
    }
    conn.prepare_cached(sql::NOTE_EDITION_CHECK.text())?
        .execute(params![album.album_id, now])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::base_title;

    #[test]
    fn version_words_aside_titles_read_the_same() {
        assert_eq!(base_title("Halo (Deluxe Edition)"), base_title("Halo"));
        assert_eq!(base_title("Halo [Remastered 2020]"), base_title("halo"));
        assert_eq!(base_title("Halo - 2020 Remaster"), base_title("Halo"));
        assert_ne!(base_title("Halo II"), base_title("Halo"));
    }
}
