//! Fork: the evidence ladder for artists still without a MusicBrainz link.
//!
//! After the album evidence, the Deezer corroboration and the joined-credit
//! splits ([`super::credits`]), an artist left unlinked is checked against
//! everything else the library holds, cheapest and strongest first:
//!
//! 1. **Deezer artist → MusicBrainz.** The Deezer artists its tracks and
//!    albums link to (and its accepted Deezer matches name), under its name,
//!    whose MusicBrainz URL relation names one artist.
//! 2. **ISRC → recording credit.** Its tracks' ISRCs, looked up at
//!    MusicBrainz: the credited artist whose name is the artist's.
//! 3. **Co-artist and label.** For the MusicBrainz artists of its name: the
//!    one with a recording or release together with an artist it shares
//!    credits with here, or with a release on its albums' label.
//! 4. **Deezer only.** When MusicBrainz has no artist of the name at all:
//!    the one Deezer artist of the name whose catalogue holds its albums or
//!    tracks, linked through its stand-in id ([`crate::sources`]), so the
//!    artist opens a normal page. Once MusicBrainz knows the artist, a
//!    later check moves the link over ([`hand_over_stand_in`]).
//!
//! An artist is linked only when exactly one MusicBrainz artist fits and
//! nothing disagrees (two rungs naming different artists, two ISRCs naming
//! different artists, two candidates both with co-credits). Names are
//! compared in several spellings ([`name_keys`]: case, accents, a leading
//! "The", "&"/"and"/"+", punctuation and spacing), which only widens the
//! search. A link is stored as the round-one links are (`manual`, no
//! person, its own audit reason); an artist a person turned down or linked
//! is never touched. What is still open keeps a one-line reason for the
//! Artists tab.
//!
//! Every provider answer is kept in `library_artist_evidence_cache`, so a
//! recheck asks nothing twice; an artist is checked again after
//! [`LADDER_RECHECK_MS`], or sooner when its evidence changes (a new ISRC
//! or Deezer link, a co-credited artist linked, a new accepted match).
//! Spotify is never asked: the library holds no Spotify artist ids, and the
//! app's Spotify quota is kept for playlists.

use std::collections::BTreeSet;
use std::future::Future;

use rusqlite::{OptionalExtension as _, params};
use serde::{Serialize, de::DeserializeOwned};

use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::error::ProviderError;
use droppedneedle_providers::musicbrainz::{Criticality, escape_lucene_phrase, is_valid_mbid};
use droppedneedle_providers::virtual_ids::is_virtual;

use super::credits::{self, name_key};
use super::sql::{self, LadderDeezerRow, LadderPendingRow};
use super::{ArtistSuggestions, deezer_artist_url};
use crate::identify::artist_links::{
    ArtistLinkError, LINK_DEEZER_ONLY, LINK_FROM_COARTIST, LINK_FROM_DEEZER_TRACKS, LINK_FROM_ISRC,
    LINK_FROM_LABEL, ProviderLinkOutcome, hand_over_stand_in, placeholder, set_from_provider_link,
};

/// How long a checked artist waits before it is checked again, when its
/// evidence did not change.
pub const LADDER_RECHECK_MS: i64 = 7 * 24 * 3600 * 1000;
/// How long a kept provider answer stands.
const CACHE_MS: i64 = 30 * 24 * 3600 * 1000;
/// ISRCs looked up per artist at most.
const MAX_ISRCS: usize = 6;
/// Deezer artists asked of MusicBrainz per artist at most.
const MAX_DEEZER: usize = 3;
/// Same-name candidates checked for co-credits at most.
const MAX_CANDIDATES: usize = 10;
/// Co-credited artists asked about per candidate at most.
const MAX_COARTISTS: usize = 6;
/// Labels asked about per candidate at most.
const MAX_LABELS: usize = 2;
/// MusicBrainz artists read per name search.
const SEARCH_HITS: u32 = 25;

/// What one ladder turn did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LadderTurn {
    /// Artists checked.
    pub checked: usize,
    /// Linked to a MusicBrainz artist (or moved off their stand-in).
    pub linked: usize,
    /// Left as a merge with the record already holding the artist.
    pub merges: usize,
    /// Linked to a Deezer artist's stand-in.
    pub deezer_only: usize,
}

/// The ways of writing a name compared as one: its folded letters and
/// digits, with and without a leading "The", with "&" and "+" as "and" and
/// the other way about. Empty for a name with no letters or digits.
pub fn name_keys(name: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let lowered = name.to_lowercase();
    for spelled in [
        lowered.clone(),
        lowered.replace(['&', '+'], " and "),
        lowered.replace(" and ", " & "),
    ] {
        let key = name_key(&spelled);
        if key.is_empty() {
            continue;
        }
        if let Some(rest) = key.strip_prefix("the")
            && rest.len() >= 3
        {
            keys.insert(rest.to_owned());
        }
        keys.insert(format!("the{key}"));
        keys.insert(key);
    }
    keys
}

/// Whether `name` is one of `keys`'s spellings.
fn named(keys: &BTreeSet<String>, name: &str) -> bool {
    let key = name_key(name);
    !key.is_empty() && keys.contains(&key)
}

/// The texts a name is searched by: as written, then without a leading
/// "The", with "&" as "and" (and back), and without trailing punctuation,
/// each once.
pub fn search_texts(name: &str) -> Vec<String> {
    let name = name.trim();
    let mut texts = vec![name.to_owned()];
    let mut push = |text: String| {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() && !texts.iter().any(|known| known.eq_ignore_ascii_case(&text)) {
            texts.push(text);
        }
    };
    if name.len() > 4 && name[..4].eq_ignore_ascii_case("the ") {
        push(name[4..].to_owned());
    }
    if name.contains('&') {
        push(name.replace('&', " and "));
    } else if name.to_lowercase().contains(" and ") {
        push(name.replace(" and ", " & ").replace(" And ", " & "));
    }
    push(
        name.trim_end_matches(|c: char| !c.is_alphanumeric())
            .to_owned(),
    );
    texts
}

/// One MusicBrainz artist of the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Candidate {
    pub mbid: String,
    pub name: String,
    pub aliases: Vec<String>,
}

/// The one Deezer artist of a name MusicBrainz does not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct DeezerArtist {
    pub id: String,
    pub name: String,
    pub picture: Option<String>,
}

/// What the ladder settled for one artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Link it to this MusicBrainz artist, for this reason.
    Link { mbid: String, reason: &'static str },
    /// Link it to this Deezer artist's stand-in.
    DeezerOnly(DeezerArtist),
    /// Nothing settles it, for this reason (one line, for the Artists tab).
    Open(String),
    /// This rung decides nothing: go on to the next.
    Next,
}

/// The first two rungs: per Deezer artist asked, the MusicBrainz artists
/// its URL relations name; per ISRC, the credited artists under the name.
pub fn decide_strong(
    name: &str,
    deezer: &[BTreeSet<String>],
    isrcs: &[BTreeSet<String>],
) -> Verdict {
    let by_deezer: BTreeSet<&String> = deezer.iter().flatten().collect();
    let by_isrc: BTreeSet<&String> = isrcs.iter().flatten().collect();
    if by_deezer.len() > 1 {
        return Verdict::Open(format!(
            "Deezer's {name} is linked to {} MusicBrainz artists",
            by_deezer.len()
        ));
    }
    if by_isrc.len() > 1 {
        return Verdict::Open(format!(
            "Its ISRCs credit {} different MusicBrainz artists named {name}",
            by_isrc.len()
        ));
    }
    match (by_deezer.first(), by_isrc.first()) {
        (Some(a), Some(b)) if a != b => Verdict::Open(format!(
            "Deezer and the ISRCs point to different MusicBrainz artists named {name}"
        )),
        (Some(mbid), _) => Verdict::Link {
            mbid: (*mbid).clone(),
            reason: LINK_FROM_DEEZER_TRACKS,
        },
        (None, Some(mbid)) => Verdict::Link {
            mbid: (*mbid).clone(),
            reason: LINK_FROM_ISRC,
        },
        (None, None) => Verdict::Next,
    }
}

/// The third rung: of `candidates`, those with a co-credit with an artist
/// here, and those with a release on the albums' label.
pub fn decide_candidates(
    name: &str,
    candidates: &[Candidate],
    with_coartist: &BTreeSet<String>,
    on_label: &BTreeSet<String>,
) -> Verdict {
    if candidates.is_empty() {
        return Verdict::Next;
    }
    let positive: BTreeSet<&String> = with_coartist.iter().chain(on_label).collect();
    match positive.len() {
        1 => {
            let mbid = positive.first().map(|id| (*id).clone()).unwrap_or_default();
            Verdict::Link {
                reason: if with_coartist.contains(&mbid) {
                    LINK_FROM_COARTIST
                } else {
                    LINK_FROM_LABEL
                },
                mbid,
            }
        }
        0 if candidates.len() == 1 => Verdict::Open(format!(
            "One MusicBrainz artist named {name}, nothing here confirms it"
        )),
        0 => Verdict::Open(format!(
            "{} MusicBrainz artists named {name}, no album or collaborator decides it",
            candidates.len()
        )),
        several => Verdict::Open(format!(
            "{several} MusicBrainz artists named {name} have worked with artists here"
        )),
    }
}

/// The last rung, for a name MusicBrainz has no artist of: the Deezer
/// artists of the name, and whether the one's catalogue holds the files.
pub fn decide_deezer_only(name: &str, hits: &[DeezerArtist], overlaps: bool) -> Verdict {
    match hits {
        [] => Verdict::Open(format!("No MusicBrainz or Deezer artist named {name}")),
        [one] if overlaps => Verdict::DeezerOnly(one.clone()),
        [_] => Verdict::Open(format!(
            "No MusicBrainz artist named {name}; Deezer's shares no album with the files"
        )),
        several => Verdict::Open(format!(
            "No MusicBrainz artist named {name}; {} Deezer artists share the name",
            several.len()
        )),
    }
}

/// A title as overlap compares it: bracketed parts ("(Remix)", "[2019]")
/// left out, folded to letters and digits.
fn title_key(title: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    name_key(&out)
}

/// Whether a Deezer catalogue (album ids and titles) holds the files: an
/// album their links name, or a title of theirs.
pub fn catalogue_overlaps(
    listed: &[(String, String)],
    linked_albums: &BTreeSet<String>,
    titles: &[String],
) -> bool {
    let local: BTreeSet<String> = titles
        .iter()
        .map(|title| title_key(title))
        .filter(|key| key.len() >= 2)
        .collect();
    listed
        .iter()
        .any(|(id, title)| linked_albums.contains(id) || local.contains(&title_key(title)))
}

/// What the library holds about one artist.
#[derive(Default)]
struct Local {
    isrcs: Vec<String>,
    deezer: Vec<LadderDeezerRow>,
    deezer_albums: BTreeSet<String>,
    titles: Vec<String>,
    coartists: Vec<String>,
    labels: Vec<String>,
}

impl ArtistSuggestions {
    /// Fork: one turn of the evidence ladder over up to `batch` artists
    /// ([`self`](super::ladder)). Resumable: each artist's check is written
    /// as it finishes.
    ///
    /// # Errors
    ///
    /// A store fault, or a provider failing in a way worth backing off for.
    pub async fn ladder_some(&self, batch: u32) -> Result<LadderTurn, String> {
        let before = self.clock.now_ms() - LADDER_RECHECK_MS;
        let pending = self
            .db
            .read("artist_match.ladder_pending", move |conn| {
                Ok::<_, DbError>(
                    conn.prepare_cached(sql::LADDER_PENDING.text())?
                        .query_map(params![i64::from(batch), before], LadderPendingRow::read)?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                )
            })
            .await
            .map_err(|error| error.to_string())?;
        let mut turn = LadderTurn::default();
        for artist in pending {
            let id = artist.artist_id.clone();
            let local = self
                .db
                .read("artist_match.ladder_local", move |conn| {
                    Ok::<_, DbError>(read_local(conn, &id)?)
                })
                .await
                .map_err(|error| error.to_string())?;
            let handing_over = artist
                .stand_in
                .as_deref()
                .is_some_and(|id| is_virtual(&id.to_ascii_lowercase()));
            let verdict = match self.climb(&artist.name, &local, handing_over).await {
                Ok(verdict) => verdict,
                Err(error) if error.is_retriable() => return Err(error.to_string()),
                Err(error) => {
                    tracing::debug!(%error, artist = artist.artist_id, "artist ladder skipped");
                    Verdict::Open(format!("{} could not be looked up", artist.name))
                }
            };
            let now = self.clock.now_ms();
            let outcome = self
                .db
                .write(Lane::Background, "artist_match.ladder_put", move |tx| {
                    write_verdict(tx, &artist, verdict, now)
                })
                .await
                .map_err(|error| error.to_string())?;
            turn.checked += 1;
            match outcome {
                Written::Linked => turn.linked += 1,
                Written::Merge => turn.merges += 1,
                Written::DeezerOnly => turn.deezer_only += 1,
                Written::Open => {}
            }
        }
        if turn.linked + turn.merges + turn.deezer_only > 0 {
            tracing::info!(
                checked = turn.checked,
                linked = turn.linked,
                merges = turn.merges,
                deezer_only = turn.deezer_only,
                "artists settled by the evidence ladder"
            );
        }
        Ok(turn)
    }

    /// Climb the ladder for an artist named `name`. `handing_over`: it sits
    /// on a Deezer stand-in, so only a MusicBrainz artist settles it.
    async fn climb(
        &self,
        name: &str,
        local: &Local,
        handing_over: bool,
    ) -> Result<Verdict, ProviderError> {
        let keys = name_keys(name);
        if keys.is_empty() {
            return Ok(Verdict::Open(format!("{name} has no letters to search by")));
        }
        // Rung 1: the Deezer artists of its tracks, under its name.
        let mut by_deezer = Vec::new();
        for row in local.deezer.iter().take(MAX_DEEZER * 3) {
            if by_deezer.len() >= MAX_DEEZER {
                break;
            }
            let deezer_name = if row.name.trim().is_empty() {
                self.deezer_name(&row.deezer_artist_id).await?
            } else {
                row.name.clone()
            };
            if !named(&keys, &deezer_name) {
                continue;
            }
            by_deezer.push(self.deezer_relations(&row.deezer_artist_id).await?);
        }
        // Rung 2: its ISRCs' recording credits, under its name.
        let mut by_isrc = Vec::new();
        for isrc in local.isrcs.iter().take(MAX_ISRCS) {
            let credits = self.isrc_credits(isrc).await?;
            let found: BTreeSet<String> = credits
                .into_iter()
                .filter(|(_, artist, credited)| named(&keys, artist) || named(&keys, credited))
                .map(|(mbid, _, _)| mbid)
                .collect();
            if !found.is_empty() {
                by_isrc.push(found);
            }
        }
        let strong = decide_strong(name, &by_deezer, &by_isrc);
        if !matches!(strong, Verdict::Next) {
            return Ok(strong);
        }
        // Rung 3: the MusicBrainz artists of the name, told apart by
        // co-credits and labels.
        let candidates = self.candidates(name, &keys).await?;
        if !candidates.is_empty() {
            let mut with_coartist = BTreeSet::new();
            let mut on_label = BTreeSet::new();
            let coartists: Vec<&String> = local
                .coartists
                .iter()
                .filter(|id| !candidates.iter().any(|c| &c.mbid == *id))
                .take(MAX_COARTISTS)
                .collect();
            for candidate in candidates.iter().take(MAX_CANDIDATES) {
                if !coartists.is_empty() && self.credited_with(&candidate.mbid, &coartists).await? {
                    with_coartist.insert(candidate.mbid.clone());
                }
                for label in local.labels.iter().take(MAX_LABELS) {
                    if self.on_label(&candidate.mbid, label).await? {
                        on_label.insert(candidate.mbid.clone());
                    }
                }
            }
            return Ok(decide_candidates(
                name,
                &candidates,
                &with_coartist,
                &on_label,
            ));
        }
        if handing_over {
            return Ok(Verdict::Open(format!(
                "No MusicBrainz artist named {name} yet; it shows Deezer's"
            )));
        }
        // Rung 4: no MusicBrainz artist of the name at all.
        let hits = self.deezer_artists(name, &keys).await?;
        let overlaps = match hits.as_slice() {
            [one] => {
                local
                    .deezer
                    .iter()
                    .any(|row| row.deezer_artist_id == one.id)
                    || {
                        let listed = self.deezer_albums(&one.id).await?;
                        catalogue_overlaps(&listed, &local.deezer_albums, &local.titles)
                    }
            }
            _ => false,
        };
        Ok(decide_deezer_only(name, &hits, overlaps))
    }

    /// The answer kept for `lookup`, or `fetch`'s, kept.
    async fn cached<T, F>(&self, lookup: String, fetch: F) -> Result<T, ProviderError>
    where
        T: Serialize + DeserializeOwned + Send + 'static,
        F: Future<Output = Result<T, ProviderError>>,
    {
        let since = self.clock.now_ms() - CACHE_MS;
        let key = lookup.clone();
        let kept: Option<String> = self
            .db
            .read("artist_match.evidence_cached", move |conn| {
                Ok::<_, DbError>(
                    conn.prepare_cached(sql::EVIDENCE_CACHED.text())?
                        .query_row(params![key, since], |row| row.get(0))
                        .optional()?,
                )
            })
            .await
            .unwrap_or(None);
        if let Some(answer) = kept.and_then(|json| serde_json::from_str(&json).ok()) {
            return Ok(answer);
        }
        let answer = fetch.await?;
        let json = serde_json::to_string(&answer).unwrap_or_default();
        let now = self.clock.now_ms();
        let _ = self
            .db
            .write(Lane::Background, "artist_match.evidence_put", move |tx| {
                Ok::<_, DbError>(
                    tx.prepare_cached(sql::PUT_EVIDENCE.text())?
                        .execute(params![lookup, json, now])?,
                )
            })
            .await;
        Ok(answer)
    }

    /// The MusicBrainz artists Deezer artist `id`'s URL relations name.
    async fn deezer_relations(&self, id: &str) -> Result<BTreeSet<String>, ProviderError> {
        let musicbrainz = self.musicbrainz.clone();
        let url = deezer_artist_url(id);
        self.cached(format!("dzurl:{id}"), async move {
            let resolution = musicbrainz
                .resolve_url(&url, &["artist-rels"], Criticality::IdentityCritical)
                .await?;
            Ok(resolution
                .relations
                .iter()
                .filter_map(|relation| relation.artist.as_ref())
                .map(|artist| artist.id.to_ascii_lowercase())
                .filter(|id| is_valid_mbid(id) && !placeholder(id))
                .collect())
        })
        .await
    }

    /// Deezer artist `id`'s name.
    async fn deezer_name(&self, id: &str) -> Result<String, ProviderError> {
        let Ok(number) = id.parse::<i64>() else {
            return Ok(String::new());
        };
        let deezer = self.deezer.clone();
        self.cached(format!("dzname:{id}"), async move {
            Ok(deezer
                .deezer_artist(number, RequestPriority::BackgroundSync)
                .await?
                .map(|artist| artist.name)
                .unwrap_or_default())
        })
        .await
    }

    /// The artists credited on the recordings of ISRC `isrc`: id, name,
    /// name as credited.
    async fn isrc_credits(
        &self,
        isrc: &str,
    ) -> Result<Vec<(String, String, String)>, ProviderError> {
        let isrc = isrc.trim().to_ascii_uppercase();
        if isrc.len() != 12 || !isrc.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Ok(Vec::new());
        }
        let musicbrainz = self.musicbrainz.clone();
        let query = format!("isrc:{isrc}");
        self.cached(format!("isrc:{isrc}"), async move {
            let page = musicbrainz
                .search_recordings_query(&query, 10, Criticality::IdentityCritical)
                .await?;
            Ok(page
                .items
                .into_iter()
                .flat_map(|hit| hit.artist_credit)
                .map(|credit| {
                    (
                        credit.artist.id.to_ascii_lowercase(),
                        credit.artist.name,
                        credit.name,
                    )
                })
                .filter(|(id, _, _)| is_valid_mbid(id) && !placeholder(id))
                .collect())
        })
        .await
    }

    /// The MusicBrainz artists whose name or an alias is one of `keys`,
    /// searched by each of [`search_texts`] until one finds any.
    async fn candidates(
        &self,
        name: &str,
        keys: &BTreeSet<String>,
    ) -> Result<Vec<Candidate>, ProviderError> {
        for text in search_texts(name) {
            let musicbrainz = self.musicbrainz.clone();
            let query = text.clone();
            let hits: Vec<Candidate> = self
                .cached(format!("mbsearch:{}", text.to_lowercase()), async move {
                    let page = musicbrainz
                        .search_artists(&query, SEARCH_HITS, Criticality::IdentityCritical)
                        .await?;
                    Ok(page
                        .items
                        .into_iter()
                        .filter(|hit| is_valid_mbid(&hit.id))
                        .map(|hit| Candidate {
                            mbid: hit.id.to_ascii_lowercase(),
                            name: hit.name.unwrap_or_default(),
                            aliases: hit
                                .aliases
                                .into_iter()
                                .flatten()
                                .map(|alias| alias.name)
                                .collect(),
                        })
                        .collect())
                })
                .await?;
            let mut found: Vec<Candidate> = hits
                .into_iter()
                .filter(|hit| !placeholder(&hit.mbid))
                .filter(|hit| {
                    named(keys, &hit.name) || hit.aliases.iter().any(|alias| named(keys, alias))
                })
                .collect();
            found.dedup_by(|a, b| a.mbid == b.mbid);
            if !found.is_empty() {
                return Ok(found);
            }
        }
        Ok(Vec::new())
    }

    /// Whether MusicBrainz artist `candidate` has a recording or release
    /// credited together with one of `others`.
    async fn credited_with(
        &self,
        candidate: &str,
        others: &[&String],
    ) -> Result<bool, ProviderError> {
        let joined = others
            .iter()
            .map(|id| format!("arid:{id}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let query = format!("arid:{candidate} AND ({joined})");
        let lookup = format!(
            "with:{candidate}:{}",
            others
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        let musicbrainz = self.musicbrainz.clone();
        self.cached(lookup, async move {
            let recordings = musicbrainz
                .search_recordings_query(&query, 1, Criticality::IdentityCritical)
                .await?;
            if recordings.count > 0 {
                return Ok(true);
            }
            let releases = musicbrainz
                .search_releases_query(&query, 1, Criticality::IdentityCritical)
                .await?;
            Ok(releases.count > 0)
        })
        .await
    }

    /// Whether MusicBrainz artist `candidate` has a release on `label`.
    async fn on_label(&self, candidate: &str, label: &str) -> Result<bool, ProviderError> {
        let label = label.trim();
        if name_key(label).is_empty() {
            return Ok(false);
        }
        let query = format!(
            "arid:{candidate} AND label:\"{}\"",
            escape_lucene_phrase(label)
        );
        let musicbrainz = self.musicbrainz.clone();
        self.cached(
            format!("label:{candidate}:{}", label.to_lowercase()),
            async move {
                let releases = musicbrainz
                    .search_releases_query(&query, 1, Criticality::IdentityCritical)
                    .await?;
                Ok(releases.count > 0)
            },
        )
        .await
    }

    /// The Deezer artists whose name is one of `keys`.
    async fn deezer_artists(
        &self,
        name: &str,
        keys: &BTreeSet<String>,
    ) -> Result<Vec<DeezerArtist>, ProviderError> {
        let deezer = self.deezer.clone();
        let query = name.to_owned();
        let hits: Vec<DeezerArtist> = self
            .cached(format!("dzsearch:{}", name.to_lowercase()), async move {
                Ok(deezer
                    .deezer_search_artists(&query, 10, RequestPriority::BackgroundSync)
                    .await?
                    .into_iter()
                    .map(|hit| DeezerArtist {
                        id: hit.id.to_string(),
                        name: hit.name,
                        picture: hit.picture,
                    })
                    .collect())
            })
            .await?;
        let mut found: Vec<DeezerArtist> = hits
            .into_iter()
            .filter(|hit| named(keys, &hit.name))
            .collect();
        found.dedup_by(|a, b| a.id == b.id);
        Ok(found)
    }

    /// Deezer artist `id`'s albums: id and title.
    async fn deezer_albums(&self, id: &str) -> Result<Vec<(String, String)>, ProviderError> {
        let Ok(number) = id.parse::<i64>() else {
            return Ok(Vec::new());
        };
        let deezer = self.deezer.clone();
        self.cached(format!("dzalbums:{id}"), async move {
            Ok(deezer
                .deezer_artist_albums(number, RequestPriority::BackgroundSync)
                .await?
                .into_iter()
                .map(|album| (album.id.to_string(), album.title))
                .collect())
        })
        .await
    }
}

/// Read what the library holds about artist `artist`.
fn read_local(conn: &rusqlite::Connection, artist: &str) -> rusqlite::Result<Local> {
    let strings = |statement: &str| -> rusqlite::Result<Vec<String>> {
        conn.prepare_cached(statement)?
            .query_map([artist], |row| row.get::<_, String>(0))?
            .collect()
    };
    Ok(Local {
        isrcs: strings(sql::LADDER_ISRCS.text())?,
        deezer: conn
            .prepare_cached(sql::LADDER_DEEZER_ARTISTS.text())?
            .query_map([artist], LadderDeezerRow::read)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        deezer_albums: strings(sql::LADDER_DEEZER_ALBUMS.text())?
            .into_iter()
            .collect(),
        titles: strings(sql::LADDER_TITLES.text())?,
        coartists: strings(sql::LADDER_COARTISTS.text())?,
        labels: strings(sql::LADDER_LABELS.text())?,
    })
}

/// What writing a verdict did.
enum Written {
    Linked,
    Merge,
    DeezerOnly,
    Open,
}

/// Write `verdict` for `artist` at `now`, and record the check.
fn write_verdict(
    tx: &rusqlite::Transaction<'_>,
    artist: &LadderPendingRow,
    verdict: Verdict,
    now: i64,
) -> Result<Written, ArtistLinkError> {
    // A person may have turned the artist down while it was looked up.
    let declined: bool = tx
        .prepare_cached(sql::DECLINED_MUSICBRAINZ.text())?
        .exists([&artist.artist_id])?;
    let (written, reason) = if declined {
        (Written::Open, None)
    } else {
        match verdict {
            Verdict::Link { mbid, reason } => {
                match hand_over_stand_in(tx, &artist.artist_id, &mbid, reason, now)? {
                    ProviderLinkOutcome::Linked => (Written::Linked, None),
                    ProviderLinkOutcome::Shared(others) => {
                        credits::offer_shared(tx, &artist.artist_id, &others, &mbid, now)?;
                        (Written::Merge, None)
                    }
                    ProviderLinkOutcome::Skipped => (Written::Open, None),
                }
            }
            Verdict::DeezerOnly(found) => {
                let stand_in = crate::sources::catalogue::keep_artist(
                    tx,
                    &found.id,
                    &found.name,
                    found.picture.as_deref(),
                    now,
                )?;
                match set_from_provider_link(
                    tx,
                    &artist.artist_id,
                    &stand_in,
                    LINK_DEEZER_ONLY,
                    now,
                )? {
                    ProviderLinkOutcome::Linked => (Written::DeezerOnly, None),
                    _ => (
                        Written::Open,
                        Some(format!(
                            "Only Deezer knows {}, and another artist here already shows it",
                            artist.name
                        )),
                    ),
                }
            }
            Verdict::Open(reason) => (Written::Open, Some(reason)),
            Verdict::Next => (Written::Open, None),
        }
    };
    tx.prepare_cached(sql::MARK_LADDER.text())?
        .execute(params![artist.artist_id, now, artist.evidence, reason])?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const B: &str = "aaaaaaaa-0000-4000-8000-000000000002";

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    fn candidate(mbid: &str) -> Candidate {
        Candidate {
            mbid: mbid.to_owned(),
            name: "Willo".to_owned(),
            aliases: Vec::new(),
        }
    }

    #[test]
    fn spellings_of_one_name_compare_as_one() {
        let keys = name_keys("The Japanese Popstars");
        assert!(named(&keys, "Japanese Popstars"));
        assert!(named(&keys, "the japanese popstars"));
        let keys = name_keys("ABSOLUTE.");
        assert!(named(&keys, "Absolute"));
        let keys = name_keys("Simon & Garfunkel");
        assert!(named(&keys, "Simon and Garfunkel"));
        assert!(named(&keys, "Simon + Garfunkel"));
        let keys = name_keys("Beyoncé");
        assert!(named(&keys, "BEYONCE"));
        assert!(named(&name_keys("Mind Against"), "MindAgainst"));
        assert!(!named(&name_keys("Mind Against"), "Mind Against Twins"));
        assert!(!named(&name_keys("Low"), "Lowe"));
        assert!(name_keys("...").is_empty());
    }

    #[test]
    fn a_name_is_searched_in_its_other_spellings() {
        assert_eq!(
            search_texts("The Streets"),
            vec!["The Streets".to_owned(), "Streets".to_owned()]
        );
        assert_eq!(
            search_texts("Nina Lares & Esteban Adame"),
            vec![
                "Nina Lares & Esteban Adame".to_owned(),
                "Nina Lares and Esteban Adame".to_owned()
            ]
        );
        assert_eq!(
            search_texts("ABSOLUTE."),
            vec!["ABSOLUTE.".to_owned(), "ABSOLUTE".to_owned()]
        );
    }

    #[test]
    fn isrcs_agreeing_link() {
        let verdict = decide_strong("Willo", &[], &[set(&[A]), set(&[A])]);
        assert_eq!(
            verdict,
            Verdict::Link {
                mbid: A.to_owned(),
                reason: LINK_FROM_ISRC
            }
        );
    }

    #[test]
    fn isrcs_disagreeing_block() {
        let verdict = decide_strong("Willo", &[], &[set(&[A]), set(&[B])]);
        assert!(
            matches!(verdict, Verdict::Open(ref why) if why.contains("2 different")),
            "{verdict:?}"
        );
        let verdict = decide_strong("Willo", &[], &[set(&[A, B])]);
        assert!(matches!(verdict, Verdict::Open(_)), "{verdict:?}");
    }

    #[test]
    fn deezer_link_settles_and_must_agree_with_isrcs() {
        assert_eq!(
            decide_strong("Willo", &[set(&[A])], &[]),
            Verdict::Link {
                mbid: A.to_owned(),
                reason: LINK_FROM_DEEZER_TRACKS
            }
        );
        assert_eq!(
            decide_strong("Willo", &[set(&[A])], &[set(&[A])]),
            Verdict::Link {
                mbid: A.to_owned(),
                reason: LINK_FROM_DEEZER_TRACKS
            }
        );
        assert!(matches!(
            decide_strong("Willo", &[set(&[A])], &[set(&[B])]),
            Verdict::Open(_)
        ));
        assert!(matches!(
            decide_strong("Willo", &[set(&[A]), set(&[B])], &[]),
            Verdict::Open(_)
        ));
        assert_eq!(decide_strong("Willo", &[set(&[])], &[]), Verdict::Next);
    }

    #[test]
    fn one_candidate_with_a_collaborator_links() {
        let candidates = [candidate(A), candidate(B)];
        assert_eq!(
            decide_candidates("Willo", &candidates, &set(&[B]), &set(&[])),
            Verdict::Link {
                mbid: B.to_owned(),
                reason: LINK_FROM_COARTIST
            }
        );
        assert_eq!(
            decide_candidates("Willo", &candidates, &set(&[]), &set(&[A])),
            Verdict::Link {
                mbid: A.to_owned(),
                reason: LINK_FROM_LABEL
            }
        );
        assert_eq!(
            decide_candidates("Willo", &candidates, &set(&[A]), &set(&[A])),
            Verdict::Link {
                mbid: A.to_owned(),
                reason: LINK_FROM_COARTIST
            }
        );
    }

    #[test]
    fn same_name_candidates_nothing_decides_stay_open() {
        let candidates = [candidate(A), candidate(B), candidate("c")];
        assert_eq!(
            decide_candidates("Willo", &candidates, &set(&[]), &set(&[])),
            Verdict::Open(
                "3 MusicBrainz artists named Willo, no album or collaborator decides it".to_owned()
            )
        );
        // A co-credit and a label naming different candidates conflict.
        assert!(matches!(
            decide_candidates("Willo", &candidates, &set(&[A]), &set(&[B])),
            Verdict::Open(_)
        ));
        assert!(matches!(
            decide_candidates("Willo", &candidates[..1], &set(&[]), &set(&[])),
            Verdict::Open(ref why) if why.starts_with("One MusicBrainz artist")
        ));
        assert_eq!(
            decide_candidates("Willo", &[], &set(&[]), &set(&[])),
            Verdict::Next
        );
    }

    fn deezer(id: &str) -> DeezerArtist {
        DeezerArtist {
            id: id.to_owned(),
            name: "Teste".to_owned(),
            picture: None,
        }
    }

    #[test]
    fn deezer_only_takes_the_one_artist_whose_catalogue_holds_the_files() {
        assert_eq!(
            decide_deezer_only("Teste", &[deezer("1")], true),
            Verdict::DeezerOnly(deezer("1"))
        );
        assert!(matches!(
            decide_deezer_only("Teste", &[deezer("1")], false),
            Verdict::Open(_)
        ));
        assert!(matches!(
            decide_deezer_only("Teste", &[deezer("1"), deezer("2")], true),
            Verdict::Open(ref why) if why.contains("2 Deezer artists")
        ));
        assert!(matches!(
            decide_deezer_only("Teste", &[], true),
            Verdict::Open(_)
        ));
    }

    #[test]
    fn a_catalogue_overlaps_by_album_link_or_title() {
        let listed = vec![
            ("11".to_owned(), "Night Drive (Remixes)".to_owned()),
            ("12".to_owned(), "Other".to_owned()),
        ];
        assert!(catalogue_overlaps(&listed, &set(&["12"]), &[]));
        assert!(catalogue_overlaps(
            &listed,
            &set(&[]),
            &["Night Drive [2019]".to_owned()]
        ));
        assert!(!catalogue_overlaps(
            &listed,
            &set(&["99"]),
            &["Day Drive".to_owned()]
        ));
    }
}
