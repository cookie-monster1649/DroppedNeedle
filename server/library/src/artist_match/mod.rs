//! Fork: suggestions for an artist with no MusicBrainz link.
//!
//! A background loop looks at a few such artists at a time and keeps, for
//! each, what Check albums' Artists list offers:
//!
//! - another live artist that is plainly the same (the same name once
//!   folded, or nearly), to merge into. One with a MusicBrainz link is
//!   preferred, so the merge brings the link along;
//! - otherwise the MusicBrainz artist of the same name, to link. When
//!   another artist in the library already has that link, the suggestion
//!   is a merge into that artist instead (linking would be refused);
//! - Deezer's artist, whose picture stands in for the artist's own: the
//!   artist of an album match a person accepted, or else the one Deezer
//!   artist of exactly that name.
//!
//! Before those, the loop asks MusicBrainz which artist links to the
//! Deezer artist of an artist's accepted album matches (its URL relation).
//! Exactly one links the artist outright; when another record already
//! holds that artist, the pair is offered as a merge and shown here. None
//! or several change nothing ([`ArtistSuggestions::link_some`]).
//!
//! Taking a suggestion goes through the existing routes (the MusicBrainz
//! link and the artist merge); "Not it" keeps the loop from offering it
//! again. The picture and genres stay either way.

pub mod sql;

use std::sync::Arc;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::error::ProviderError;
use droppedneedle_providers::itunes::similarity;
use droppedneedle_providers::musicbrainz::{Criticality, MusicBrainzView, is_valid_mbid};
use droppedneedle_providers::preview::{PreviewClient, norm};

use crate::jobs::models::OperationError;

use self::sql::{
    ArtistDeezerRow, DeezerLinkPendingRow, LiveArtistRow, MatchedDeezerRow, OpenSuggestionRow,
    PendingArtistRow,
};
use crate::identify::artist_links::{
    ArtistLinkError, LINK_FROM_DEEZER, ProviderLinkOutcome, set_from_provider_link,
};

/// How long an artist with nothing to offer waits before it is looked at
/// again.
const RECHECK_MS: i64 = 30 * 24 * 3600 * 1000;
/// MusicBrainz hits read per artist.
const SEARCH_HITS: u32 = 5;
/// The lowest MusicBrainz search score taken.
const MB_MIN_SCORE: i64 = 90;
/// Two folded names this alike (0 to 100) are one artist, when both are
/// long enough to tell.
const NEAR_NAME: f64 = 92.0;
/// Folded names shorter than this must match exactly.
const NEAR_MIN_LEN: usize = 6;
/// The most Deezer artists of one artist asked of MusicBrainz a turn.
const DEEZER_IDS_ASKED: usize = 3;

/// The Deezer artist page MusicBrainz keeps URL relations under.
fn deezer_artist_url(id: &str) -> String {
    format!("https://www.deezer.com/artist/{id}")
}

/// The one MusicBrainz artist the URL relations name, lowercased: none
/// when they name no artist or more than one.
fn sole_artist<'a>(
    relations: impl Iterator<Item = &'a droppedneedle_providers::musicbrainz::models::Relation>,
) -> Option<String> {
    let mut ids: Vec<String> = relations
        .filter_map(|relation| relation.artist.as_ref())
        .map(|artist| artist.id.to_ascii_lowercase())
        .filter(|id| is_valid_mbid(id))
        .collect();
    ids.sort();
    ids.dedup();
    match ids.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// What asking MusicBrainz about an artist's Deezer link did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeezerLinkTurn {
    /// Artists asked about.
    pub asked: usize,
    /// Linked to the one MusicBrainz artist found.
    pub linked: usize,
    /// Left with a merge into the artist already linked to it.
    pub merges: usize,
}

/// Where an artist's suggestions stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionState {
    /// Nobody decided yet.
    Open,
    /// A person said it is not them.
    Declined,
}

droppedneedle_kernel::spelled!(SuggestionState {
    Open => "open",
    Declined => "declined",
});

/// Whether two folded names are one artist's.
pub fn same_name(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || (a.len().min(b.len()) >= NEAR_MIN_LEN && similarity(a, b) >= NEAR_NAME)
}

/// A live artist, folded.
#[derive(Debug, Clone)]
struct LiveArtist {
    id: String,
    folded: String,
    mbid: Option<String>,
}

/// The artist to merge `id` (folded `folded`) into: one with the same
/// name, a MusicBrainz-linked one first, then an exact name, then the
/// lowest id.
fn merge_target<'a>(id: &str, folded: &str, live: &'a [LiveArtist]) -> Option<&'a LiveArtist> {
    live.iter()
        .filter(|other| other.id != id && same_name(folded, &other.folded))
        .min_by(|a, b| {
            b.mbid
                .is_some()
                .cmp(&a.mbid.is_some())
                .then((b.folded == folded).cmp(&(a.folded == folded)))
                .then(a.id.cmp(&b.id))
        })
}

/// What the loop found for one artist.
#[derive(Debug, Clone, Default, PartialEq)]
struct Found {
    merge_artist_id: Option<String>,
    musicbrainz: Option<(String, String, Option<String>)>,
    deezer: Option<DeezerFound>,
}

#[derive(Debug, Clone, PartialEq)]
struct DeezerFound {
    id: String,
    picture: Option<String>,
    link: Option<String>,
    fans: Option<i64>,
}

/// One artist's open suggestion, for Check albums.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistSuggestion {
    pub artist_id: String,
    pub name: String,
    pub artist_revision: i64,
    pub album_count: i64,
    /// The MusicBrainz artist to link: id, name and its short note.
    pub musicbrainz: Option<(String, String, Option<String>)>,
    /// The artist to merge into.
    pub merge: Option<MergeTarget>,
    pub deezer_artist_id: Option<String>,
    pub deezer_url: Option<String>,
    pub fans: Option<i64>,
}

/// The live artist a suggestion merges into.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeTarget {
    pub artist_id: String,
    pub name: String,
    pub artist_revision: i64,
    pub musicbrainz_artist_id: Option<String>,
}

impl From<OpenSuggestionRow> for ArtistSuggestion {
    fn from(row: OpenSuggestionRow) -> Self {
        let merge = match (row.merge_artist_id, row.merge_name, row.merge_revision) {
            (Some(artist_id), Some(name), Some(artist_revision)) => Some(MergeTarget {
                artist_id,
                name,
                artist_revision,
                musicbrainz_artist_id: row.merge_mbid,
            }),
            _ => None,
        };
        Self {
            musicbrainz: if merge.is_some() {
                None
            } else {
                row.musicbrainz_artist_id.map(|id| {
                    (
                        id,
                        row.musicbrainz_name.unwrap_or_default(),
                        row.musicbrainz_disambiguation,
                    )
                })
            },
            merge,
            artist_id: row.artist_id,
            name: row.name,
            artist_revision: row.artist_revision,
            album_count: row.album_count,
            deezer_artist_id: row.deezer_artist_id,
            deezer_url: row.link_url,
            fans: row.fans,
        }
    }
}

/// Genres shown on an artist's page at most.
const MAX_GENRES: usize = 8;

/// What an artist with no MusicBrainz link shows from other catalogues:
/// Deezer's artist and the genres of its accepted album matches.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistProviderPage {
    pub deezer_artist_id: Option<String>,
    pub deezer_url: Option<String>,
    pub fans: Option<i64>,
    /// Most albums first.
    pub genres: Vec<String>,
}

/// Artist `artist_id`'s page from other catalogues; none when there is
/// nothing to show.
pub fn artist_page(
    conn: &rusqlite::Connection,
    artist_id: &str,
) -> rusqlite::Result<Option<ArtistProviderPage>> {
    use rusqlite::OptionalExtension as _;
    let deezer = conn
        .prepare_cached(sql::ARTIST_DEEZER.text())?
        .query_row([artist_id], ArtistDeezerRow::read)
        .optional()?;
    let lists = conn
        .prepare_cached(sql::ARTIST_MATCH_GENRES.text())?
        .query_map([artist_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let genres = top_genres(
        lists
            .iter()
            .map(|json| serde_json::from_str::<Vec<String>>(json).unwrap_or_default()),
    );
    if deezer.is_none() && genres.is_empty() {
        return Ok(None);
    }
    Ok(Some(ArtistProviderPage {
        deezer_url: deezer.as_ref().map(|row| {
            row.link_url.clone().unwrap_or_else(|| {
                format!("https://www.deezer.com/artist/{}", row.deezer_artist_id)
            })
        }),
        fans: deezer.as_ref().and_then(|row| row.fans),
        deezer_artist_id: deezer.map(|row| row.deezer_artist_id),
        genres,
    }))
}

/// The genres named most across albums, each once (case aside), at most
/// [`MAX_GENRES`]; a tie keeps the first seen.
fn top_genres(lists: impl Iterator<Item = Vec<String>>) -> Vec<String> {
    let mut counts: Vec<(String, String, usize)> = Vec::new();
    for list in lists {
        let mut seen = std::collections::HashSet::new();
        for genre in list {
            let key = genre.trim().to_lowercase();
            if key.is_empty() || !seen.insert(key.clone()) {
                continue;
            }
            match counts.iter_mut().find(|(known, _, _)| *known == key) {
                Some((_, _, count)) => *count += 1,
                None => counts.push((key, genre.trim().to_owned(), 1)),
            }
        }
    }
    counts.sort_by(|a, b| b.2.cmp(&a.2));
    counts
        .into_iter()
        .take(MAX_GENRES)
        .map(|(_, genre, _)| genre)
        .collect()
}

/// The artist suggestions: the loop that finds them and a person's "Not
/// it".
#[derive(Clone)]
pub struct ArtistSuggestions {
    db: Db,
    deezer: PreviewClient,
    musicbrainz: MusicBrainzView,
    clock: Arc<dyn Clock>,
}

impl ArtistSuggestions {
    pub fn new(
        db: Db,
        deezer: PreviewClient,
        musicbrainz: MusicBrainzView,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            db,
            deezer,
            musicbrainz,
            clock,
        }
    }

    /// Look at up to `batch` artists. Answers how many were looked at.
    ///
    /// # Errors
    ///
    /// A store fault, or a provider failing in a way worth backing off for.
    pub async fn suggest_some(&self, batch: u32) -> Result<usize, String> {
        let before = self.clock.now_ms() - RECHECK_MS;
        let (pending, live) = self
            .db
            .read("artist_match.pending", move |conn| {
                let pending = conn
                    .prepare_cached(sql::PENDING_ARTISTS.text())?
                    .query_map(params![i64::from(batch), before], PendingArtistRow::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let live = if pending.is_empty() {
                    Vec::new()
                } else {
                    conn.prepare_cached(sql::LIVE_ARTISTS.text())?
                        .query_map([], LiveArtistRow::read)?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                let mut matched = Vec::with_capacity(pending.len());
                for artist in &pending {
                    matched.push(
                        conn.prepare_cached(sql::MATCHED_DEEZER_ARTISTS.text())?
                            .query_map([&artist.artist_id], MatchedDeezerRow::read)?
                            .collect::<rusqlite::Result<Vec<_>>>()?,
                    );
                }
                Ok::<_, droppedneedle_kernel::db::DbError>((
                    pending.into_iter().zip(matched).collect::<Vec<_>>(),
                    live,
                ))
            })
            .await
            .map_err(|error| error.to_string())?;
        let live: Vec<LiveArtist> = live
            .into_iter()
            .map(|row| LiveArtist {
                folded: norm(&row.name),
                id: row.artist_id,
                mbid: row.mbid,
            })
            .collect();
        let mut looked = 0;
        for (artist, matched) in pending {
            let found = match self.find(&artist, &matched, &live).await {
                Ok(found) => found,
                Err(error) if error.is_retriable() => return Err(error.to_string()),
                Err(error) => {
                    tracing::debug!(%error, artist = artist.artist_id, "artist suggestion skipped");
                    Found::default()
                }
            };
            let now = self.clock.now_ms();
            self.db
                .write(Lane::Background, "artist_match.put", move |tx| {
                    let (mbid, mb_name, mb_note) = match found.musicbrainz {
                        Some((id, name, note)) => (Some(id), Some(name), note),
                        None => (None, None, None),
                    };
                    let deezer = found.deezer;
                    tx.prepare_cached(sql::PUT_SUGGESTION.text())?
                        .execute(params![
                            artist.artist_id,
                            found.merge_artist_id,
                            mbid,
                            mb_name,
                            mb_note,
                            deezer.as_ref().map(|found| found.id.clone()),
                            deezer.as_ref().and_then(|found| found.picture.clone()),
                            deezer.as_ref().and_then(|found| found.link.clone()),
                            deezer.as_ref().and_then(|found| found.fans),
                            now,
                        ])?;
                    Ok::<_, droppedneedle_kernel::db::DbError>(())
                })
                .await
                .map_err(|error| error.to_string())?;
            looked += 1;
        }
        Ok(looked)
    }

    /// Fork: ask MusicBrainz which artist links to the Deezer artist of
    /// the accepted album matches of up to `batch` unlinked artists. One
    /// artist found links the row; when another record holds that artist,
    /// the pair is offered as a merge (and shown as the row's suggestion)
    /// instead. None or several change nothing. Each row records what it
    /// asked, so it is asked again only after [`RECHECK_MS`] or when a new
    /// match names another Deezer artist.
    ///
    /// # Errors
    ///
    /// A store fault, or MusicBrainz failing in a way worth backing off
    /// for.
    pub async fn link_some(&self, batch: u32) -> Result<DeezerLinkTurn, String> {
        let before = self.clock.now_ms() - RECHECK_MS;
        let pending = self
            .db
            .read("artist_match.deezer_pending", move |conn| {
                let rows = conn
                    .prepare_cached(sql::DEEZER_LINK_PENDING.text())?
                    .query_map(
                        params![i64::from(batch), before],
                        DeezerLinkPendingRow::read,
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut pending = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut ids: Vec<String> = conn
                        .prepare_cached(sql::MATCHED_DEEZER_ARTISTS.text())?
                        .query_map([&row.artist_id], MatchedDeezerRow::read)?
                        .map(|found| found.map(|found| found.deezer_artist_id))
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    ids.retain(|id| !id.trim().is_empty());
                    ids.sort();
                    ids.dedup();
                    pending.push((row.artist_id, ids));
                }
                Ok::<_, droppedneedle_kernel::db::DbError>(pending)
            })
            .await
            .map_err(|error| error.to_string())?;
        let mut turn = DeezerLinkTurn::default();
        for (artist, ids) in pending {
            let mut found: Vec<String> = Vec::new();
            for id in ids.iter().take(DEEZER_IDS_ASKED) {
                match self
                    .musicbrainz
                    .resolve_url(
                        &deezer_artist_url(id),
                        &["artist-rels"],
                        Criticality::BestEffort,
                    )
                    .await
                {
                    Ok(resolution) => found.extend(sole_artist(resolution.relations.iter())),
                    Err(error) if error.is_retriable() => return Err(error.to_string()),
                    Err(error) => {
                        tracing::debug!(%error, artist, deezer = id, "deezer link not asked");
                    }
                }
            }
            found.sort();
            found.dedup();
            let mbid = match found.as_slice() {
                [one] => Some(one.clone()),
                _ => None,
            };
            let asked = ids.join(",");
            let now = self.clock.now_ms();
            let outcome = self
                .db
                .write(Lane::Background, "artist_match.deezer_link", move |tx| {
                    tx.prepare_cached(sql::MARK_DEEZER_LINK.text())?
                        .execute(params![artist, asked, now])?;
                    let Some(mbid) = mbid else {
                        return Ok::<_, ArtistLinkError>(ProviderLinkOutcome::Skipped);
                    };
                    let declined: Option<Option<String>> = {
                        use rusqlite::OptionalExtension as _;
                        tx.prepare_cached(sql::DECLINED_MUSICBRAINZ.text())?
                            .query_row([&artist], |row| row.get(0))
                            .optional()?
                    };
                    if declined
                        .flatten()
                        .is_some_and(|turned_down| turned_down.eq_ignore_ascii_case(&mbid))
                    {
                        return Ok(ProviderLinkOutcome::Skipped);
                    }
                    let outcome =
                        set_from_provider_link(tx, &artist, &mbid, LINK_FROM_DEEZER, now)?;
                    if let ProviderLinkOutcome::Shared(others) = &outcome {
                        for (other, _) in others {
                            let (left, right) = if artist.as_str() < other.as_str() {
                                (artist.as_str(), other.as_str())
                            } else {
                                (other.as_str(), artist.as_str())
                            };
                            tx.prepare_cached(sql::OFFER_MERGE.text())?
                                .execute(params![
                                    format!("provider:{left}:{right}"),
                                    left,
                                    right,
                                    now,
                                ])?;
                        }
                        if let Some((target, _)) = others.first() {
                            tx.prepare_cached(sql::SUGGEST_DEEZER_MERGE.text())?
                                .execute(params![artist, target, mbid, now])?;
                        }
                    }
                    Ok::<_, ArtistLinkError>(outcome)
                })
                .await
                .map_err(|error| error.to_string())?;
            turn.asked += 1;
            match outcome {
                ProviderLinkOutcome::Linked => turn.linked += 1,
                ProviderLinkOutcome::Shared(_) => turn.merges += 1,
                ProviderLinkOutcome::Skipped => {}
            }
        }
        if turn.linked + turn.merges > 0 {
            tracing::info!(
                asked = turn.asked,
                linked = turn.linked,
                merges = turn.merges,
                "artists linked to musicbrainz by their deezer artist"
            );
        }
        Ok(turn)
    }

    async fn find(
        &self,
        artist: &PendingArtistRow,
        matched: &[MatchedDeezerRow],
        live: &[LiveArtist],
    ) -> Result<Found, ProviderError> {
        let folded = norm(&artist.name);
        let mut found = Found::default();
        if folded.is_empty() {
            return Ok(found);
        }
        if let Some(target) = merge_target(&artist.artist_id, &folded, live) {
            found.merge_artist_id = Some(target.id.clone());
        } else if let Some(hit) = self.musicbrainz_artist(&artist.name, &folded).await? {
            let owner = live.iter().find(|other| {
                other.id != artist.artist_id
                    && other
                        .mbid
                        .as_deref()
                        .is_some_and(|mbid| mbid.eq_ignore_ascii_case(&hit.0))
            });
            match owner {
                Some(owner) => found.merge_artist_id = Some(owner.id.clone()),
                None => found.musicbrainz = Some(hit),
            }
        }
        found.deezer = self.deezer_artist(&folded, &artist.name, matched).await?;
        Ok(found)
    }

    /// The MusicBrainz artist named `name`: the best-scored hit whose name
    /// folds to the same.
    async fn musicbrainz_artist(
        &self,
        name: &str,
        folded: &str,
    ) -> Result<Option<(String, String, Option<String>)>, ProviderError> {
        let page = self
            .musicbrainz
            .search_artists(name, SEARCH_HITS, Criticality::BestEffort)
            .await?;
        Ok(page
            .items
            .into_iter()
            .filter(|hit| is_valid_mbid(&hit.id))
            .filter(|hit| hit.score.or(hit.ext_score).unwrap_or(0) >= MB_MIN_SCORE)
            .find(|hit| hit.name.as_deref().is_some_and(|got| norm(got) == folded))
            .map(|hit| {
                (
                    hit.id.to_ascii_lowercase(),
                    hit.name.unwrap_or_default(),
                    hit.disambiguation.filter(|note| !note.trim().is_empty()),
                )
            }))
    }

    /// Deezer's artist: the one of an accepted album match with the same
    /// name, or else the only Deezer artist of exactly that name.
    async fn deezer_artist(
        &self,
        folded: &str,
        name: &str,
        matched: &[MatchedDeezerRow],
    ) -> Result<Option<DeezerFound>, ProviderError> {
        if let Some(row) = matched.iter().find(|row| norm(&row.name) == folded) {
            return Ok(Some(DeezerFound {
                link: Some(format!(
                    "https://www.deezer.com/artist/{}",
                    row.deezer_artist_id
                )),
                id: row.deezer_artist_id.clone(),
                picture: row.picture_url.clone(),
                fans: None,
            }));
        }
        let hits = self
            .deezer
            .deezer_search_artists(name, SEARCH_HITS, RequestPriority::BackgroundSync)
            .await?;
        let mut exact = hits.into_iter().filter(|hit| norm(&hit.name) == folded);
        let (Some(hit), None) = (exact.next(), exact.next()) else {
            return Ok(None);
        };
        Ok(Some(DeezerFound {
            id: hit.id.to_string(),
            picture: hit.picture,
            link: hit.link,
            fans: hit.nb_fan,
        }))
    }

    /// The open suggestions, by artist name. Blocking.
    ///
    /// # Errors
    ///
    /// A store fault.
    pub fn open(&self) -> Result<Vec<ArtistSuggestion>, OperationError> {
        self.db.read_blocking("artist_match.open", |conn| {
            let rows = conn
                .prepare_cached(sql::OPEN_SUGGESTIONS.text())?
                .query_map([], OpenSuggestionRow::read)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, OperationError>(rows.into_iter().map(Into::into).collect())
        })
    }

    /// A person says artist `artist_id`'s suggestion is not them.
    /// Blocking.
    ///
    /// # Errors
    ///
    /// `NotFound` when the artist has no open suggestion.
    pub fn decline(&self, artist_id: &str, user_id: &str) -> Result<(), OperationError> {
        let now = self.clock.now_ms();
        self.db
            .write_blocking(Lane::Foreground, "artist_match.decline", |tx| {
                let changed = tx
                    .prepare_cached(sql::DECLINE.text())?
                    .execute(params![artist_id, user_id, now])?;
                if changed == 0 {
                    return Err(OperationError::NotFound(
                        crate::reasons::NO_ARTIST_SUGGESTION,
                    ));
                }
                Ok(())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artist(id: &str, name: &str, mbid: Option<&str>) -> LiveArtist {
        LiveArtist {
            id: id.to_owned(),
            folded: norm(name),
            mbid: mbid.map(str::to_owned),
        }
    }

    #[test]
    fn punctuation_and_case_are_one_name() {
        assert!(same_name(
            &norm("Tyler, The Creator"),
            &norm("tyler the creator")
        ));
        assert!(!same_name(&norm("Low"), &norm("Lowe")));
    }

    #[test]
    fn a_merge_goes_into_the_linked_artist() {
        let live = vec![
            artist("a", "Poppy Ajudha", None),
            artist("b", "Poppy Ajudha", None),
            artist(
                "c",
                "POPPY AJUDHA",
                Some("0f6d2d4b-6e7d-4c38-a5a1-3c5fbc2f4a10"),
            ),
        ];
        let target = merge_target("a", &norm("Poppy Ajudha"), &live).expect("a target");
        assert_eq!(target.id, "c");
    }

    #[test]
    fn genres_named_by_most_albums_lead() {
        let got = top_genres(
            vec![
                vec!["Pop".to_owned(), "R&B".to_owned()],
                vec!["r&b".to_owned()],
                vec!["Jazz".to_owned(), "Jazz".to_owned()],
            ]
            .into_iter(),
        );
        assert_eq!(got, vec!["R&B", "Pop", "Jazz"]);
    }

    #[test]
    fn a_lone_name_has_no_merge() {
        let live = vec![artist("a", "Colde", None), artist("b", "Cold", None)];
        assert!(merge_target("a", &norm("Colde"), &live).is_none());
    }
}
