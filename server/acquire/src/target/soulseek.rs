//! Search Soulseek for a target and pick the folder to fetch from.
//!
//! Albums search with the album query ladder and rank folders with
//! [`rank_folders`]. Fork: a single track goes straight to the track query
//! ladder (each rung costs a search window, and slskd runs one search at a
//! time); its results still turn up album folders, which rank first, and a
//! lone-track share is taken only when nothing better exists; the caller
//! records the reason.

use super::SearchTarget;
use super::reasons::TrackReason;
use crate::client_error::ClientError;
use crate::reason::Reason;
use crate::slskd::folders::{FolderPick, RankRequest, rank_folders, rank_listing};
use crate::slskd::{SearchResult, SlskdHttp, SlskdRepository};

/// The folder chosen, and why a lone-track share was used when it was.
#[derive(Debug, Clone)]
pub struct SoulseekChoice {
    /// The folder and the files to enqueue from it.
    pub pick: FolderPick,
    /// Set when the pick is a lone-track share.
    pub lone_reason: Option<TrackReason>,
}

/// Why nothing was picked.
#[derive(Debug)]
pub enum SoulseekMiss {
    /// slskd failed.
    Search(ClientError),
    /// Nothing usable was found: why, and the text the log keeps.
    Nothing { reason: Reason, detail: String },
}

const NO_FOLDER: &str = "slskd has no untried folder for this album";

/// Why no folder could be picked: folders were found but the settings refuse
/// each of them, or else `reason`.
fn miss<H: SlskdHttp>(
    hits: &[SearchResult],
    request: RankRequest<'_>,
    repo: &SlskdRepository<H>,
    reason: Reason,
    detail: &str,
) -> SoulseekMiss {
    if rank_listing(hits, request, repo.policy()).is_empty() {
        return SoulseekMiss::Nothing {
            reason,
            detail: detail.to_owned(),
        };
    }
    SoulseekMiss::Nothing {
        reason: Reason::NothingAllowed,
        detail: "slskd has folders for this, but none in your quality settings".to_owned(),
    }
}

/// Search and pick. `keep` drops hits that may not be tried again
/// (quarantined files, peers this task already tried).
pub async fn choose<H: SlskdHttp>(
    repo: &SlskdRepository<H>,
    target: &SearchTarget,
    keep: impl Fn(&SearchResult) -> bool,
) -> Result<SoulseekChoice, SoulseekMiss> {
    let request = RankRequest {
        tracklist: &target.tracklist,
        wanted: &target.wanted,
        single_track: target.track.is_some(),
    };
    let policy = repo.policy();
    let mut hits: Vec<SearchResult> = Vec::new();
    if target.has_album() && target.track.is_none() {
        hits = repo
            .search_album(&target.artist, &target.album_title, target.year)
            .await
            .map_err(SoulseekMiss::Search)?
            .into_iter()
            .filter(|hit| keep(hit))
            .collect();
        let best = rank_folders(&hits, request, policy).into_iter().next();
        match (&target.track, best) {
            (None, Some(pick)) => {
                return Ok(SoulseekChoice {
                    pick,
                    lone_reason: None,
                });
            }
            (None, None) => {
                return Err(miss(&hits, request, repo, Reason::NoSource, NO_FOLDER));
            }
            (Some(_), _) => {}
        }
    }
    let Some(track) = target.track.as_ref() else {
        return Err(miss(&hits, request, repo, Reason::NoSource, NO_FOLDER));
    };
    // No album folder holds the track: search for the track itself. Its
    // results can still turn up an album folder, which wins as usual.
    let album = target.has_album().then_some(target.album_title.as_str());
    let more = repo
        .search_track(&track.artist, &track.title, album)
        .await
        .map_err(SoulseekMiss::Search)?;
    for hit in more {
        let seen = hits
            .iter()
            .any(|known| known.username == hit.username && known.filename == hit.filename);
        if !seen && keep(&hit) && !credits_swapped(&hit.filename, &track.artist, &track.title) {
            hits.push(hit);
        }
    }
    let Some(pick) = rank_folders(&hits, request, policy).into_iter().next() else {
        return Err(miss(
            &hits,
            request,
            repo,
            TrackReason::TrackNotFound.into(),
            &TrackReason::TrackNotFound.to_string(),
        ));
    };
    let lone_reason = if pick.lone_track || !target.has_album() {
        Some(track.no_album.unwrap_or(TrackReason::NoAlbumFolder))
    } else {
        None
    };
    Ok(SoulseekChoice { pick, lone_reason })
}

/// Fork: whether a song search hit names the asked artist and title the
/// wrong way round: its file name reads "X - Y" (track numbers aside) with
/// the title part agreeing with the asked artist and not the asked title,
/// as `Arizona - Common Souls` does for "Arizona" by Common Souls. Soulseek
/// matches words anywhere in a path, so such a file matches every word of
/// the search while being another artist's song.
pub fn credits_swapped(filename: &str, artist: &str, title: &str) -> bool {
    use crate::landing::want::core_title;
    use droppedneedle_library::matching::score::NAME_GATE;
    use droppedneedle_library::matching::strings::string_dist;
    let name = filename.rsplit(['\\', '/']).next().unwrap_or(filename);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let Some((_, tail)) = stem.rsplit_once(" - ") else {
        return false;
    };
    let tail =
        core_title(tail.trim_start_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '.'));
    if tail.is_empty() {
        return false;
    }
    let close = |a: &str, b: &str| string_dist(a, &core_title(b)) <= NAME_GATE;
    let primary = artist.split([',', '&']).next().unwrap_or(artist).trim();
    !close(&tail, title) && (close(&tail, artist) || close(&tail, primary))
}

#[cfg(test)]
mod fork_tests {
    use super::credits_swapped;

    #[test]
    fn a_file_naming_the_artist_as_its_title_is_another_song() {
        assert!(credits_swapped(
            r"songs\Arizona - Common Souls.mp3",
            "Common Souls",
            "Arizona"
        ));
        assert!(!credits_swapped(
            r"Common Souls\Pyramid Lake\03 - Arizona.flac",
            "Common Souls",
            "Arizona"
        ));
        assert!(!credits_swapped(
            r"songs\Common Souls - Arizona.mp3",
            "Common Souls",
            "Arizona"
        ));
        assert!(!credits_swapped(
            r"x\01 Arizona.flac",
            "Common Souls",
            "Arizona"
        ));
        // A self-titled song is not a swap.
        assert!(!credits_swapped(
            r"x\Bad Company - Bad Company.flac",
            "Bad Company",
            "Bad Company"
        ));
    }
}
