//! Fork: an album's match outside MusicBrainz.
//!
//! An album identification found nothing for on MusicBrainz (`unmatched`
//! with no MusicBrainz link) gets a best guess from Deezer, which a
//! background loop looks for a few albums at a time. The strongest
//! evidence is an ISRC one of the album's files carries (from a source
//! link or the playlist entry it came from): Deezer names the album that
//! recording is on. Otherwise Deezer's album search by artist and title is
//! asked. Every candidate is scored on its title, artist, and how many of
//! the album's files its track list accounts for (titles, then lengths);
//! the best one at or over [`THRESHOLD`] is kept as a suggestion.
//!
//! Check albums shows the suggestion; a person accepts it ("Use this") or
//! says it is not the album ("Not it"). An accepted match gives the album
//! its cover, year, label, genres and track list (what the album page
//! shows), and links the album and its matched files to Deezer as source
//! links. A MusicBrainz link the album gets later wins; the match stays as
//! extra data.

pub mod edition;
pub mod sql;

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::error::ProviderError;
use droppedneedle_providers::itunes::{similarity, token_set_ratio};
use droppedneedle_providers::preview::{DeezerAlbum, PreviewClient, norm};

use crate::jobs::models::OperationError;

use self::sql::{HeldFileRow, MatchRow, PendingAlbumRow};

/// The provider the matches come from.
pub const DEEZER: &str = MatchProvider::Deezer.as_str();
/// The lowest score kept as a suggestion.
pub const THRESHOLD: f64 = 0.6;
/// How long an album Deezer had nothing for waits before it is asked again.
const NOT_FOUND_RETRY_MS: i64 = 30 * 24 * 3600 * 1000;
/// Album hits read per search.
const SEARCH_HITS: u32 = 5;
/// Album details read per album at most.
const DETAILS_PER_ALBUM: usize = 3;
/// Two lengths this close (seconds) are the same recording.
const LENGTH_SLACK: f64 = 10.0;

/// The catalogue a match comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchProvider {
    Deezer,
}

droppedneedle_kernel::spelled!(MatchProvider {
    Deezer => "deezer",
});

/// Where a match stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMatchState {
    /// Found by the loop; nobody decided yet.
    Suggested,
    /// A person accepted it.
    Confirmed,
    /// A person said it is not the album.
    Declined,
    /// The provider had nothing; asked again after a while.
    NotFound,
}

droppedneedle_kernel::spelled!(ProviderMatchState {
    Suggested => "suggested",
    Confirmed => "confirmed",
    Declined => "declined",
    NotFound => "not_found",
});

/// One track of the provider's album.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchTrack {
    pub provider_track_id: String,
    pub title: String,
    pub duration_seconds: Option<f64>,
    pub disc: i64,
    pub position: i64,
    pub isrc: Option<String>,
}

/// An album's stored match.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderMatch {
    pub album_id: String,
    pub provider: String,
    pub provider_album_id: String,
    pub title: String,
    pub artist_name: String,
    pub provider_artist_id: Option<String>,
    pub artist_picture_url: Option<String>,
    pub release_date: Option<String>,
    pub label: Option<String>,
    pub upc: Option<String>,
    pub record_type: Option<String>,
    pub cover_url: Option<String>,
    pub link_url: Option<String>,
    pub genres: Vec<String>,
    pub tracks: Vec<MatchTrack>,
    pub score: f64,
    pub state: ProviderMatchState,
}

impl ProviderMatch {
    /// The year of its release date.
    pub fn year(&self) -> Option<i32> {
        self.release_date
            .as_deref()
            .and_then(|date| date.get(..4))
            .and_then(|year| year.parse().ok())
            .filter(|year| *year > 0)
    }

    fn from_row(row: MatchRow) -> Option<Self> {
        Some(Self {
            state: ProviderMatchState::parse(&row.match_state)?,
            album_id: row.local_album_id,
            provider: row.provider,
            provider_album_id: row.provider_album_id,
            title: row.title,
            artist_name: row.artist_name,
            provider_artist_id: row.provider_artist_id,
            artist_picture_url: row.artist_picture_url,
            release_date: row.release_date,
            label: row.label,
            upc: row.upc,
            record_type: row.record_type,
            cover_url: row.cover_url,
            link_url: row.link_url,
            genres: serde_json::from_str(&row.genres_json).unwrap_or_default(),
            tracks: serde_json::from_str(&row.tracks_json).unwrap_or_default(),
            score: row.score,
        })
    }
}

/// One file the album holds.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldFile {
    pub track_id: String,
    pub title: String,
    pub duration_seconds: Option<f64>,
    pub isrc: Option<String>,
}

impl From<HeldFileRow> for HeldFile {
    fn from(row: HeldFileRow) -> Self {
        Self {
            track_id: row.track_id,
            title: row.title,
            duration_seconds: row.duration_seconds,
            isrc: row.isrc,
        }
    }
}

/// A provider track and whether the album holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldTrack {
    pub track: MatchTrack,
    /// The file that is this track, when the album holds it.
    pub track_id: Option<String>,
}

/// Whether `file` is `track`: the titles agree (one holds the other once
/// both are folded, or they are close), and the lengths, when both are
/// known, are within [`LENGTH_SLACK`].
pub fn same_track(file: &HeldFile, track: &MatchTrack) -> bool {
    if let (Some(a), Some(b)) = (&file.isrc, &track.isrc)
        && a.eq_ignore_ascii_case(b)
    {
        return true;
    }
    let (a, b) = (norm(&file.title), norm(&track.title));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let titles = a == b
        || (a.len().min(b.len()) >= 4 && (a.contains(&b) || b.contains(&a)))
        || similarity(&a, &b) >= 85.0;
    let lengths = match (file.duration_seconds, track.duration_seconds) {
        (Some(a), Some(b)) => (a - b).abs() <= LENGTH_SLACK,
        _ => true,
    };
    titles && lengths
}

/// Each provider track, with the file that is it. A file is used once.
pub fn held_tracks(files: &[HeldFile], tracks: &[MatchTrack]) -> Vec<HeldTrack> {
    let mut used = vec![false; files.len()];
    tracks
        .iter()
        .map(|track| {
            let found = files
                .iter()
                .enumerate()
                .find(|(index, file)| !used[*index] && same_track(file, track));
            let track_id = found.map(|(index, file)| {
                used[index] = true;
                file.track_id.clone()
            });
            HeldTrack {
                track: track.clone(),
                track_id,
            }
        })
        .collect()
}

/// How well a provider album fits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    /// 0 to 1.
    pub score: f64,
    /// Files its track list accounts for.
    pub matched_files: usize,
}

/// How well `tracks` (titled `title`, by `artist`) fits the album: title
/// 30%, artist 25%, the share of files it accounts for 45%. An ISRC hit
/// adds 0.2. Without the ISRC an artist that shares no word scores 0.
pub fn fit(
    album_title: &str,
    album_artist: &str,
    files: &[HeldFile],
    title: &str,
    artist: &str,
    tracks: &[MatchTrack],
    isrc_hit: bool,
) -> Fit {
    let title_score = token_set_ratio(album_title, title) / 100.0;
    let artist_score = if album_artist.trim().is_empty() {
        0.5
    } else {
        token_set_ratio(album_artist, artist) / 100.0
    };
    let matched_files = held_tracks(files, tracks)
        .iter()
        .filter(|held| held.track_id.is_some())
        .count();
    if !isrc_hit && (artist_score == 0.0 || matched_files == 0) {
        return Fit {
            score: 0.0,
            matched_files,
        };
    }
    let share = if files.is_empty() {
        0.0
    } else {
        matched_files as f64 / files.len() as f64
    };
    let score =
        0.30 * title_score + 0.25 * artist_score + 0.45 * share + if isrc_hit { 0.2 } else { 0.0 };
    Fit {
        score: score.min(1.0),
        matched_files,
    }
}

fn tracks_of(album: &DeezerAlbum) -> Vec<MatchTrack> {
    album
        .tracks
        .iter()
        .map(|track| MatchTrack {
            provider_track_id: track.id.to_string(),
            title: track.title.clone(),
            duration_seconds: track.duration.map(|seconds| seconds as f64),
            disc: track.disc,
            position: track.position,
            isrc: track.isrc.clone(),
        })
        .collect()
}

/// The album's indexed files.
pub fn held_files(conn: &Connection, album_id: &str) -> rusqlite::Result<Vec<HeldFile>> {
    let mut stmt = conn.prepare_cached(sql::HELD_FILES.text())?;
    let rows = stmt
        .query_map([album_id], HeldFileRow::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Album `album_id`'s match, whatever its state.
pub fn match_of(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<ProviderMatch>> {
    let row = conn
        .prepare_cached(sql::MATCH_OF.text())?
        .query_row([album_id], MatchRow::read)
        .optional()?;
    Ok(row.and_then(ProviderMatch::from_row))
}

/// The suggestions among `album_ids`, by album, with how many of the
/// album's files each accounts for.
pub fn suggestions(
    conn: &Connection,
    album_ids: &[String],
) -> rusqlite::Result<HashMap<String, (ProviderMatch, u32)>> {
    let mut found = HashMap::new();
    for id in album_ids {
        if let Some(found_match) = match_of(conn, id)?
            && found_match.state == ProviderMatchState::Suggested
        {
            let files = held_files(conn, id)?;
            let matched = held_tracks(&files, &found_match.tracks)
                .iter()
                .filter(|held| held.track_id.is_some())
                .count();
            found.insert(
                id.clone(),
                (found_match, u32::try_from(matched).unwrap_or(u32::MAX)),
            );
        }
    }
    Ok(found)
}

/// An accepted match, with the provider's tracks and which the album
/// holds: what the album page shows.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumProviderPage {
    pub found: ProviderMatch,
    pub tracks: Vec<HeldTrack>,
}

/// Album `album_id`'s accepted match, for its page.
pub fn album_page(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Option<AlbumProviderPage>> {
    let Some(found) = match_of(conn, album_id)? else {
        return Ok(None);
    };
    if found.state != ProviderMatchState::Confirmed {
        return Ok(None);
    }
    let files = held_files(conn, album_id)?;
    let tracks = held_tracks(&files, &found.tracks);
    Ok(Some(AlbumProviderPage { found, tracks }))
}

/// The provider page of an album or a track.
pub fn deezer_url(kind: &str, id: &str) -> String {
    format!("https://www.deezer.com/{kind}/{id}")
}

/// The album matches: the loop that finds them and a person's decision.
#[derive(Clone)]
pub struct ProviderMatches {
    db: Db,
    deezer: PreviewClient,
    clock: Arc<dyn Clock>,
}

/// One album the loop looks at.
#[derive(Debug, Clone)]
struct Pending {
    album_id: String,
    title: String,
    artist_name: String,
    files: Vec<HeldFile>,
}

impl ProviderMatches {
    pub fn new(db: Db, deezer: PreviewClient, clock: Arc<dyn Clock>) -> Self {
        Self { db, deezer, clock }
    }

    /// Look for a match for up to `batch` albums. Answers how many got a
    /// suggestion.
    ///
    /// # Errors
    ///
    /// A store fault, or Deezer failing in a way worth backing off for.
    pub async fn suggest_some(&self, batch: u32) -> Result<usize, String> {
        let before = self.clock.now_ms() - NOT_FOUND_RETRY_MS;
        let pending = self
            .db
            .read("provider_match.pending", move |conn| {
                let mut stmt = conn.prepare_cached(sql::PENDING_ALBUMS.text())?;
                let rows = stmt
                    .query_map(params![i64::from(batch), before], PendingAlbumRow::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    let files = held_files(conn, &row.album_id)?;
                    out.push(Pending {
                        album_id: row.album_id,
                        title: row.title,
                        artist_name: row.artist_name,
                        files,
                    });
                }
                Ok::<_, droppedneedle_kernel::db::DbError>(out)
            })
            .await
            .map_err(|error| error.to_string())?;
        let mut suggested = 0;
        for album in pending {
            if album.files.is_empty() {
                continue;
            }
            let best = match self.best_for(&album).await {
                Ok(best) => best,
                Err(error) if error.is_retriable() => return Err(error.to_string()),
                Err(error) => {
                    tracing::debug!(%error, album = album.album_id, "deezer match skipped");
                    None
                }
            };
            let now = self.clock.now_ms();
            let album_id = album.album_id.clone();
            let wrote = self
                .db
                .write(Lane::Background, "provider_match.put", move |tx| {
                    match &best {
                        Some((found, fit)) => put_match(tx, &album_id, found, fit.score, now)?,
                        None => {
                            tx.prepare_cached(sql::PUT_NOT_FOUND.text())?
                                .execute(params![album_id, DEEZER, now])?;
                        }
                    }
                    Ok::<_, droppedneedle_kernel::db::DbError>(best.is_some())
                })
                .await
                .map_err(|error| error.to_string())?;
            if wrote {
                suggested += 1;
            }
        }
        Ok(suggested)
    }

    /// The best Deezer album for `album`, when one scores at least
    /// [`THRESHOLD`].
    async fn best_for(&self, album: &Pending) -> Result<Option<(DeezerAlbum, Fit)>, ProviderError> {
        let priority = RequestPriority::BackgroundSync;
        let mut ids: Vec<(i64, bool)> = Vec::new();
        if let Some(isrc) = album.files.iter().find_map(|file| file.isrc.clone())
            && let Some(id) = self.deezer.deezer_album_of_isrc(&isrc, priority).await?
        {
            ids.push((id, true));
        }
        let hits = self
            .deezer
            .deezer_search_albums(&album.artist_name, &album.title, SEARCH_HITS, priority)
            .await?;
        for hit in hits {
            if ids.iter().all(|(id, _)| *id != hit.id) {
                ids.push((hit.id, false));
            }
        }
        let mut best: Option<(DeezerAlbum, Fit)> = None;
        for (id, isrc_hit) in ids.into_iter().take(DETAILS_PER_ALBUM) {
            let Some(found) = self.deezer.deezer_album(id, priority).await? else {
                continue;
            };
            let fit = fit(
                &album.title,
                &album.artist_name,
                &album.files,
                &found.title,
                &found.artist_name,
                &tracks_of(&found),
                isrc_hit,
            );
            if fit.score >= THRESHOLD
                && best.as_ref().is_none_or(|(_, best)| fit.score > best.score)
            {
                best = Some((found, fit));
            }
        }
        Ok(best)
    }

    /// The suggestions among `album_ids`, by album. Blocking.
    ///
    /// # Errors
    ///
    /// A store fault.
    pub fn suggestions_for(
        &self,
        album_ids: Vec<String>,
    ) -> Result<HashMap<String, (ProviderMatch, u32)>, OperationError> {
        self.db.read_blocking("provider_match.suggestions", |conn| {
            Ok::<_, OperationError>(suggestions(conn, &album_ids)?)
        })
    }

    /// A person accepts (`accept`) or turns down album `album_id`'s
    /// suggestion. Accepting links the album and the files its track list
    /// accounts for to Deezer. Blocking.
    ///
    /// # Errors
    ///
    /// `NotFound` when the album has no suggestion to decide.
    pub fn decide(
        &self,
        album_id: &str,
        accept: bool,
        user_id: &str,
    ) -> Result<ProviderMatch, OperationError> {
        let now = self.clock.now_ms();
        self.db
            .write_blocking(Lane::Foreground, "provider_match.decide", |tx| {
                let Some(found) = match_of(tx, album_id)? else {
                    return Err(no_suggestion());
                };
                if found.state == ProviderMatchState::NotFound {
                    return Err(no_suggestion());
                }
                let state = if accept {
                    ProviderMatchState::Confirmed
                } else {
                    ProviderMatchState::Declined
                };
                tx.prepare_cached(sql::DECIDE.text())?
                    .execute(params![album_id, state, user_id, now])?;
                if accept {
                    link(tx, &found, user_id, now)?;
                }
                Ok(ProviderMatch { state, ..found })
            })
    }
}

fn no_suggestion() -> OperationError {
    OperationError::NotFound(crate::reasons::NO_PROVIDER_MATCH)
}

fn put_match(
    conn: &Connection,
    album_id: &str,
    found: &DeezerAlbum,
    score: f64,
    now: i64,
) -> rusqlite::Result<()> {
    let tracks = serde_json::to_string(&tracks_of(found)).unwrap_or_else(|_| "[]".into());
    let genres = serde_json::to_string(&found.genres).unwrap_or_else(|_| "[]".into());
    conn.prepare_cached(sql::PUT_MATCH.text())?
        .execute(params![
            album_id,
            DEEZER,
            found.id.to_string(),
            found.title,
            found.artist_name,
            found.artist_id.map(|id| id.to_string()),
            found.artist_picture,
            found.release_date,
            found.label,
            found.upc,
            found.record_type,
            found.cover_url,
            found.link,
            genres,
            tracks,
            score,
            ProviderMatchState::Suggested,
            now,
        ])?;
    Ok(())
}

/// Link the album, and each file the track list accounts for, to Deezer.
fn link(conn: &Connection, found: &ProviderMatch, user_id: &str, now: i64) -> rusqlite::Result<()> {
    let page = found
        .link_url
        .clone()
        .unwrap_or_else(|| deezer_url("album", &found.provider_album_id));
    conn.prepare_cached(sql::LINK_ALBUM.text())?
        .execute(params![
            uuid::Uuid::new_v4().to_string(),
            found.album_id,
            found.provider,
            found.provider_album_id,
            page,
            user_id,
            now,
        ])?;
    let files = held_files(conn, &found.album_id)?;
    for held in held_tracks(&files, &found.tracks) {
        let Some(track_id) = held.track_id else {
            continue;
        };
        let mut links = vec![crate::matching::SourceLink {
            provider: found.provider.clone(),
            entity_type: "track".to_owned(),
            external_id: held.track.provider_track_id.clone(),
            canonical_url: deezer_url("track", &held.track.provider_track_id),
        }];
        if let Some(isrc) = &held.track.isrc {
            links.push(crate::matching::SourceLink {
                provider: "isrc".to_owned(),
                entity_type: "recording".to_owned(),
                external_id: isrc.to_ascii_uppercase(),
                canonical_url: format!(
                    "https://musicbrainz.org/isrc/{}",
                    isrc.to_ascii_uppercase()
                ),
            });
        }
        crate::identity::link_track(conn, &track_id, &links, now as f64 / 1000.0)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: &str, title: &str, seconds: f64) -> HeldFile {
        HeldFile {
            track_id: id.to_owned(),
            title: title.to_owned(),
            duration_seconds: Some(seconds),
            isrc: None,
        }
    }

    fn track(id: &str, title: &str, seconds: f64) -> MatchTrack {
        MatchTrack {
            provider_track_id: id.to_owned(),
            title: title.to_owned(),
            duration_seconds: Some(seconds),
            disc: 1,
            position: 1,
            isrc: None,
        }
    }

    #[test]
    fn the_right_ep_scores_over_the_threshold() {
        let files = vec![
            file("a", "Femme", 200.0),
            file("b", "Spread a Little Love", 220.0),
        ];
        let tracks = vec![
            track("1", "Femme", 201.0),
            track("2", "Spread a Little Love", 219.0),
            track("3", "Holy Moly", 180.0),
        ];
        let got = fit(
            "FEMME",
            "Poppy Ajudha",
            &files,
            "FEMME",
            "Poppy Ajudha",
            &tracks,
            false,
        );
        assert_eq!(got.matched_files, 2);
        assert!(got.score >= THRESHOLD, "{got:?}");
    }

    #[test]
    fn another_artist_scores_nothing() {
        let files = vec![file("a", "Femme", 200.0)];
        let tracks = vec![track("1", "Femme", 200.0)];
        let got = fit(
            "FEMME",
            "Poppy Ajudha",
            &files,
            "FEMME",
            "Tribute Band",
            &tracks,
            false,
        );
        assert_eq!(got.score, 0.0);
    }

    #[test]
    fn a_length_far_off_is_another_recording() {
        assert!(!same_track(
            &file("a", "Femme", 200.0),
            &track("1", "Femme", 320.0)
        ));
        assert!(same_track(
            &file("a", "Femme (Radio Edit)", 200.0),
            &track("1", "Femme", 204.0)
        ));
    }

    #[test]
    fn each_file_is_held_once() {
        let files = vec![file("a", "Intro", 60.0)];
        let held = held_tracks(
            &files,
            &[track("1", "Intro", 60.0), track("2", "Intro", 60.0)],
        );
        assert_eq!(held[0].track_id.as_deref(), Some("a"));
        assert_eq!(held[1].track_id, None);
    }
}
