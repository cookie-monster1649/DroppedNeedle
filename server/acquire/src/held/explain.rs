//! Fork: why a file waits in To Decide, said concretely.
//!
//! [`Reason::message`] is one fixed sentence per reason. Here the sentence
//! is worked out from the file itself: the artist and title it claims
//! against the album asked for, its format and quality, its length against
//! the track's. Nothing is stored; an old row reads as well as a new one,
//! and a reason with nothing more to say keeps its fixed sentence.

use std::collections::BTreeMap;

use droppedneedle_library::matching::strings::fold;
use serde::Serialize;
use utoipa::ToSchema;

use super::store::HeldFile;
use crate::reason::{Reason, ReasonKind};

/// The album the entry was held for.
#[derive(Debug, Clone, Copy, Default)]
pub struct Asked<'a> {
    pub artist: Option<&'a str>,
    pub album: Option<&'a str>,
}

impl Asked<'_> {
    /// "On and On by Jack Johnson", "On and On", "the album".
    fn album_by(&self) -> String {
        match (self.album_name(), self.artist_name()) {
            (Some(album), Some(artist)) => format!("{album} by {artist}"),
            (Some(album), None) => album.to_owned(),
            _ => "the album".to_owned(),
        }
    }

    fn album_name(&self) -> Option<&str> {
        self.album.map(str::trim).filter(|name| !name.is_empty())
    }

    fn artist_name(&self) -> Option<&str> {
        self.artist.map(str::trim).filter(|name| !name.is_empty())
    }

    fn album_or(&self) -> String {
        self.album_name().unwrap_or("the album").to_owned()
    }
}

fn some(value: Option<&String>) -> Option<&str> {
    value
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
}

/// Minutes and seconds.
fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as i64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// Whether two artist names are the same artist, as far as words go.
fn same_artist(left: &str, right: &str) -> bool {
    let (left, right) = (fold(left), fold(right));
    !left.is_empty() && !right.is_empty() && (left.contains(&right) || right.contains(&left))
}

/// "“Daughters” by John Mayer", "“Daughters”", "by John Mayer".
fn claim(file: &HeldFile) -> Option<String> {
    match (
        some(file.evidence_title.as_ref()),
        some(file.evidence_artist.as_ref()),
    ) {
        (Some(title), Some(artist)) => Some(format!("“{title}” by {artist}")),
        (Some(title), None) => Some(format!("“{title}”")),
        (None, Some(artist)) => Some(format!("a song by {artist}")),
        (None, None) => None,
    }
}

/// The track the file was paired to: "track 4, “Taylor”".
fn paired_track(file: &HeldFile) -> Option<String> {
    let title = some(file.pairing.track_title.as_ref())?;
    file.pairing.release_track_mbid.as_ref()?;
    Some(match file.pairing.track_number {
        Some(number) => format!("track {number}, “{title}”"),
        None => format!("“{title}”"),
    })
}

/// The file's format, upper-case.
fn format(file: &HeldFile) -> Option<String> {
    some(file.file_format.as_ref()).map(str::to_uppercase)
}

/// The length sentence, when both lengths are known and they differ.
fn lengths(file: &HeldFile) -> Option<String> {
    let (have, want) = (
        file.duration_seconds?,
        file.pairing.expected_duration_seconds?,
    );
    ((have - want).abs() >= 5.0).then(|| {
        format!(
            "This file is {} long; the track should be {}.",
            clock(have),
            clock(want)
        )
    })
}

/// Why this file waits, in plain words, from what is known of it. A
/// reason with nothing more to say keeps [`Reason::message`].
pub fn file_text(file: &HeldFile, asked: Asked<'_>) -> String {
    let detail = some(file.reason_detail.as_ref());
    let album = asked.album_by();
    let claim = claim(file);
    let artist_differs = match (some(file.evidence_artist.as_ref()), asked.artist_name()) {
        (Some(theirs), Some(ours)) => !same_artist(theirs, ours),
        _ => false,
    };
    let text = match file.reason {
        Reason::WeakMatch => Some(match (&claim, artist_differs) {
            (Some(claim), true) => format!(
                "This file says it is {claim}, a different artist than {album}. It doesn't line up with any track of that album."
            ),
            (Some(claim), false) => format!(
                "This file says it is {claim}, but the download as a whole doesn't line up with {album} closely enough to import on its own. It may be from another album or edition."
            ),
            (None, _) => format!(
                "This file has no tags, and the download doesn't line up with {album} closely enough by names, track numbers and lengths to import on its own."
            ),
        }),
        Reason::NoArtistEvidence => Some(format!(
            "This file has no artist tag, and neither the folder name nor AcoustID shows it is by {}.",
            asked.artist_name().unwrap_or("the artist asked for")
        )),
        Reason::MostlyUnmatched => Some(match detail {
            Some(detail) if detail.chars().next().is_some_and(|c| c.is_ascii_digit()) => format!(
                "Most of this download isn't {album}: {detail}. It looks like a different release or a mixed folder."
            ),
            _ => format!(
                "Most of this download isn't {album}. It looks like a different release or a mixed folder."
            ),
        }),
        Reason::TagMismatch => Some(match (&claim, paired_track(file)) {
            (Some(claim), Some(track)) => format!(
                "This file's tags say {claim}, but by length and position it would be {track} of {}.",
                asked.album_or()
            ),
            (Some(claim), None) => {
                format!("This file's tags say {claim}, which isn't a track of {album}.")
            }
            _ => detail.map_or_else(
                || Reason::TagMismatch.message().to_owned(),
                |detail| format!("Its tags don't agree with the track: {detail}."),
            ),
        }),
        Reason::FingerprintMismatch => {
            let heard = claim
                .clone()
                .unwrap_or_else(|| "a different recording".to_owned());
            let score = file
                .evidence_score
                .map(|score| format!(" ({}% sure)", (score * 100.0).round() as i64))
                .unwrap_or_default();
            Some(match paired_track(file) {
                Some(track) => {
                    format!("AcoustID heard {heard}{score}, not {track} it is named after.")
                }
                None => format!("AcoustID heard {heard}{score}, not the track it is named after."),
            })
        }
        Reason::FingerprintUnconfirmed => Some(match paired_track(file) {
            Some(track) => format!(
                "AcoustID couldn't confirm this file is {track}, so your current copy stays."
            ),
            None => Reason::FingerprintUnconfirmed.message().to_owned(),
        }),
        Reason::Samples => Some(match lengths(file) {
            Some(lengths) => {
                format!("This looks like a sample or preview, not the full track. {lengths}")
            }
            None => Reason::Samples.message().to_owned(),
        }),
        Reason::VersionLowerQuality | Reason::NoBetterThanYours => {
            let base = file.reason.message();
            Some(match format(file) {
                Some(format) => format!("{base} The held copy is {format}."),
                None => base.to_owned(),
            })
        }
        Reason::NoRelease => Some(format!(
            "MusicBrainz has no release of {album} to check these files against."
        )),
        Reason::NoTracks => Some(format!(
            "No file of this download could be imported into {album}."
        )),
        Reason::TagsDisagree => Some(match &claim {
            Some(claim) => format!(
                "This file is tagged as {claim}, but it doesn't line up with that album closely enough to import on its own."
            ),
            None => Reason::TagsDisagree.message().to_owned(),
        }),
        Reason::NotChosenEdition => Some(format!(
            "These files are a different edition of {} than the one your library uses.",
            asked.album_or()
        )),
        Reason::FormatNotListed => Some(match detail {
            // Written whole by the landing (`landing::quality_words`).
            Some(detail) if detail.starts_with("The files are") => detail.to_owned(),
            _ => match format(file) {
                Some(format) => format!(
                    "The files are {format}, a format or quality your download settings don't allow. Add it in {} to import files like these automatically.",
                    crate::landing::quality_words::QUALITY_SETTINGS
                ),
                None => Reason::FormatNotListed.message().to_owned(),
            },
        }),
        Reason::WrongTrack => Some(match (&claim, paired_track(file)) {
            (Some(claim), Some(track)) => {
                format!("This file is {claim}, not {track} you asked for.")
            }
            _ => Reason::WrongTrack.message().to_owned(),
        }),
        Reason::AlbumNotFound
        | Reason::NoAlbumMatch
        | Reason::OwnedElsewhere
        | Reason::OwnedOnAlbum
        | Reason::AlbumNeedsCheck
        | Reason::AlbumVersionOnly
        | Reason::TooManyFiles
        | Reason::Interrupted
        | Reason::HeldByEarlierVersion => None,
        reason if reason.kind() == ReasonKind::Library => {
            detail.map(|detail| format!("{} ({detail})", reason.message().trim_end_matches('.')))
        }
        Reason::ImportFailed => {
            detail.map(|detail| format!("Something went wrong importing this folder: {detail}."))
        }
        _ => None,
    };
    text.unwrap_or_else(|| file.reason.message().to_owned())
}

/// One reason an entry holds files for, said once for all of them.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HeldReasonSummary {
    pub reason: Reason,
    /// How many of the entry's files it holds.
    pub count: usize,
    /// What is wrong, for all of them.
    pub text: String,
    /// What the person can do about it.
    pub action: String,
}

/// The entry's reasons, each once, in the order its files first name them.
pub fn summaries(files: &[HeldFile], asked: Asked<'_>) -> Vec<HeldReasonSummary> {
    let mut order: Vec<Reason> = Vec::new();
    let mut by_reason: BTreeMap<&'static str, Vec<&HeldFile>> = BTreeMap::new();
    for file in files {
        let group = by_reason.entry(file.reason.code()).or_default();
        if group.is_empty() {
            order.push(file.reason);
        }
        group.push(file);
    }
    order
        .into_iter()
        .map(|reason| {
            let group = &by_reason[reason.code()];
            let texts: Vec<String> = group.iter().map(|file| file_text(file, asked)).collect();
            let text = if texts.iter().all(|text| *text == texts[0]) {
                texts[0].clone()
            } else {
                summary_text(reason, group, asked)
            };
            HeldReasonSummary {
                reason,
                count: group.len(),
                text,
                action: action_text(reason, asked),
            }
        })
        .collect()
}

/// What the files of one reason have in common, when each says something
/// of its own: the artists they claim, against the album asked for.
fn summary_text(reason: Reason, files: &[&HeldFile], asked: Asked<'_>) -> String {
    let mut artists: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for file in files {
        if let Some(artist) = some(file.evidence_artist.as_ref()) {
            artists
                .entry(fold(artist))
                .or_insert_with(|| (artist.to_owned(), 0))
                .1 += 1;
        }
    }
    let mut named: Vec<(String, usize)> = artists.into_values().collect();
    named.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let claimed = named
        .iter()
        .take(4)
        .map(|(name, count)| format!("{name} ({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    let album = asked.album_by();
    match reason {
        Reason::WeakMatch | Reason::TagMismatch | Reason::TagsDisagree if !claimed.is_empty() => {
            format!(
                "These files don't line up with {album} closely enough to import on their own. Their tags name: {claimed}."
            )
        }
        Reason::FormatNotListed => files
            .iter()
            .find_map(|file| {
                some(file.reason_detail.as_ref())
                    .filter(|detail| detail.starts_with("The files are"))
            })
            .map_or_else(|| file_text(files[0], asked), str::to_owned),
        _ => reason.message().to_owned(),
    }
}

/// What the person can do, naming the album where it helps.
pub fn action_text(reason: Reason, asked: Asked<'_>) -> String {
    let album = asked.album_or();
    match reason {
        Reason::WeakMatch | Reason::MostlyUnmatched => format!(
            "Listen, then Import the files that really are {album} and Discard the rest. If they are a different album, use Match… to pick it."
        ),
        Reason::FormatNotListed => {
            "Import them now if you want them; DroppedNeedle also keeps looking for a copy your settings allow.".to_owned()
        }
        _ => reason.action().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::held::store::{HeldRoot, Pairing};

    fn file(reason: Reason) -> HeldFile {
        HeldFile {
            id: 1,
            entry_id: 480,
            root: HeldRoot::Held,
            path: String::new(),
            original_name: "x.mp3".to_owned(),
            pairing: Pairing {
                release_mbid: None,
                release_track_mbid: None,
                recording_mbid: None,
                disc_number: None,
                track_number: None,
                track_title: None,
                expected_duration_seconds: None,
            },
            file_format: Some("mp3".to_owned()),
            duration_seconds: None,
            evidence_title: None,
            evidence_artist: None,
            evidence_score: None,
            source: "usenet".to_owned(),
            source_task_id: None,
            reason,
            reason_detail: None,
            file_size: None,
            file_modified_at: None,
            created_at: 0,
        }
    }

    const ASKED: Asked<'static> = Asked {
        artist: Some("Jack Johnson"),
        album: Some("On and On"),
    };

    fn claimed(artist: &str, title: &str) -> HeldFile {
        HeldFile {
            evidence_artist: Some(artist.to_owned()),
            evidence_title: Some(title.to_owned()),
            ..file(Reason::WeakMatch)
        }
    }

    #[test]
    fn weak_match_names_the_claimed_artist_against_the_album() {
        let text = file_text(&claimed("John Mayer", "Daughters"), ASKED);
        assert_eq!(
            text,
            "This file says it is “Daughters” by John Mayer, a different artist than On and On by Jack Johnson. It doesn't line up with any track of that album."
        );
        let same = file_text(&claimed("Jack Johnson", "Banana Pancakes"), ASKED);
        assert!(same.contains("may be from another album"), "{same}");
    }

    #[test]
    fn summaries_tally_the_claimed_artists() {
        let files = vec![
            claimed("John Mayer", "Daughters"),
            claimed("John Mayer", "Gravity"),
            claimed("Black Eyed Peas", "Shut Up"),
        ];
        let summary = summaries(&files, ASKED);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].count, 3);
        assert_eq!(
            summary[0].text,
            "These files don't line up with On and On by Jack Johnson closely enough to import on their own. Their tags name: John Mayer (2), Black Eyed Peas (1)."
        );
    }

    #[test]
    fn format_not_listed_uses_the_landing_sentence_else_the_format() {
        let mut held = file(Reason::FormatNotListed);
        held.file_format = Some("flac".to_owned());
        assert!(file_text(&held, ASKED).starts_with("The files are FLAC, a format or quality"));
        held.reason_detail = Some("The files are a higher quality format (FLAC 24-bit/192 kHz) than your download settings allow.".to_owned());
        assert_eq!(file_text(&held, ASKED), held.reason_detail.clone().unwrap());
    }

    #[test]
    fn tag_mismatch_and_samples_use_the_file() {
        let mut held = claimed("Jack Johnson", "Banana Pancakes");
        held.reason = Reason::TagMismatch;
        held.pairing.release_track_mbid = Some("t".to_owned());
        held.pairing.track_number = Some(4);
        held.pairing.track_title = Some("Taylor".to_owned());
        assert_eq!(
            file_text(&held, ASKED),
            "This file's tags say “Banana Pancakes” by Jack Johnson, but by length and position it would be track 4, “Taylor” of On and On."
        );
        let mut sample = file(Reason::Samples);
        sample.duration_seconds = Some(30.0);
        sample.pairing.expected_duration_seconds = Some(252.0);
        assert!(
            file_text(&sample, ASKED)
                .ends_with("This file is 0:30 long; the track should be 4:12.")
        );
    }

    /// Every reason says something, and never an empty sentence.
    #[test]
    fn every_reason_reads() {
        for reason in Reason::ALL {
            let text = file_text(&file(reason), ASKED);
            assert!(!text.trim().is_empty(), "{reason:?}");
            assert!(!action_text(reason, ASKED).trim().is_empty(), "{reason:?}");
        }
    }
}
