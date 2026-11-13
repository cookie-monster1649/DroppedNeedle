//! Fork: files that are plainly not part of the album asked for.
//!
//! A download can be a mixed folder: a compilation of another artist's
//! songs with one or two of the right artist's thrown in. Such a file says
//! it is by someone else (no word of its artist tag agrees with the release
//! artist, the requested artist or any track's credit, fold-insensitive),
//! and its title names no track of the release. It is never offered as a
//! candidate for the album: it is left where it landed, never held under
//! the album, and never given a track position. A download that is mostly
//! such files is the wrong release (`ForeignArtists` in [`super::specs`]).
//!
//! A file's own track number is shown only when its title is the title of
//! the release's track at that number: a number read from a song that is
//! not on the album would read as that album's track.

use std::collections::BTreeMap;

use droppedneedle_library::matching::Release;
use droppedneedle_library::matching::model::credit_text;
use droppedneedle_library::matching::score::NAME_GATE;
use droppedneedle_library::matching::strings::{
    artist_subset, fold, string_dist, strip_edition_words,
};

use super::matching::{file_name_parts, is_various};
use super::probe::{LandedFile, Landing};
use super::specs::Target;

/// The title a file goes by: its title tag, else the title in its name.
fn file_title(file: &LandedFile) -> String {
    let tagged = file.tag.title.trim();
    if tagged.is_empty() {
        file_name_parts(file).2
    } else {
        tagged.to_owned()
    }
}

/// Whether two titles name the same song, editions aside.
fn same_title(left: &str, right: &str) -> bool {
    let (left, right) = (strip_edition_words(left), strip_edition_words(right));
    !left.trim().is_empty()
        && !right.trim().is_empty()
        && (string_dist(&left, &right) <= NAME_GATE || fold(&left) == fold(&right))
}

/// Whether one artist name agrees with another: close as text, or one's
/// words hold the other's ("Jack Johnson" and "Jack Johnson feat. Ben
/// Harper").
fn names_agree(tagged: &str, name: &str) -> bool {
    string_dist(tagged, name) <= NAME_GATE || artist_subset(tagged, name) || {
        let name = fold(name);
        !name.trim().is_empty() && fold(tagged).contains(name.trim())
    }
}

/// Whether `tagged` agrees with the requested artist, the release's
/// artist or the credit of any of its tracks. With no known name to
/// compare (a various-artists release with uncredited tracks), it agrees.
pub fn artist_agrees(target: &Target, release: &Release, tagged: &str) -> bool {
    let mut known: Vec<String> = vec![target.artist_name.clone(), release.artist_text()];
    known.extend(
        release
            .tracks
            .iter()
            .map(|track| credit_text(&track.artists)),
    );
    let known: Vec<&String> = known
        .iter()
        .filter(|name| !name.trim().is_empty() && !is_various(name))
        .collect();
    known.is_empty() || known.iter().any(|name| names_agree(tagged, name))
}

/// Whether the file's title names any track of the release.
pub fn title_on_release(release: &Release, title: &str) -> bool {
    release
        .tracks
        .iter()
        .any(|track| same_title(title, &track.title))
}

/// Whether the file is plainly not part of the release: tagged with an
/// artist that agrees with no credit of it, titled as none of its tracks,
/// and not tagged with the recording id of one of them.
pub fn is_foreign(target: &Target, release: &Release, file: &LandedFile) -> bool {
    let tagged = file.tag.artist.trim();
    if tagged.is_empty() || release.tracks.is_empty() {
        return false;
    }
    if let Some(recording) = file.tag.musicbrainz_recording_id.as_deref()
        && release
            .tracks
            .iter()
            .any(|track| track.recording_id.eq_ignore_ascii_case(recording.trim()))
    {
        return false;
    }
    !artist_agrees(target, release, tagged) && !title_on_release(release, &file_title(file))
}

/// The landed files, by index, that are plainly not part of the release.
pub fn foreign_files(target: &Target, landing: &Landing, release: &Release) -> Vec<usize> {
    landing
        .audio
        .iter()
        .enumerate()
        .filter(|(_, file)| is_foreign(target, release, file))
        .map(|(index, _)| index)
        .collect()
}

/// The artists the foreign files name, most files first, with how many.
pub fn foreign_artists(landing: &Landing, files: &[usize]) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for file in files.iter().filter_map(|index| landing.audio.get(*index)) {
        let name = file.tag.artist.trim();
        let entry = counts
            .entry(fold(name))
            .or_insert_with(|| (name.to_owned(), 0));
        entry.1 += 1;
    }
    let mut named: Vec<(String, usize)> = counts.into_values().collect();
    named.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    named
}

/// The track number to show for a file held with no pairing: its own
/// number, only when the release is unknown or its track at that number
/// carries the file's title.
pub fn trusted_number(release: Option<&Release>, file: &LandedFile) -> Option<i64> {
    let number = file.tag.track_number;
    if number == 0 {
        return None;
    }
    let Some(release) = release else {
        return Some(i64::from(number));
    };
    let disc = file.tag.disc_number.max(1);
    let title = file_title(file);
    release
        .tracks
        .iter()
        .any(|track| {
            track.position == number
                && track.disc.max(1) == disc
                && same_title(&title, &track.title)
        })
        .then_some(i64::from(number))
}

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_library::matching::ReleaseTrack;
    use droppedneedle_library::matching::model::CreditedArtist;
    use droppedneedle_library::tags::AudioFormat;
    use droppedneedle_library::tags::read::AudioTag;

    fn credit(name: &str) -> Vec<CreditedArtist> {
        vec![CreditedArtist {
            name: name.to_owned(),
            ..CreditedArtist::default()
        }]
    }

    fn on_and_on() -> Release {
        let titles = [
            "Times Like These",
            "The Horizon Has Been Defeated",
            "Traffic in the Sky",
            "Taylor",
            "Gone",
            "Cupid",
            "Wasting Time",
            "Holes to Heaven",
        ];
        Release {
            id: "on-and-on".to_owned(),
            title: "On and On".to_owned(),
            artists: credit("Jack Johnson"),
            tracks: titles
                .iter()
                .enumerate()
                .map(|(index, title)| ReleaseTrack {
                    title: (*title).to_owned(),
                    recording_id: format!("rec-{index}"),
                    disc: 1,
                    position: index as u32 + 1,
                    ..ReleaseTrack::default()
                })
                .collect(),
            ..Release::default()
        }
    }

    fn target() -> Target {
        Target {
            artist_name: "Jack Johnson".to_owned(),
            album_title: "On and On".to_owned(),
            ..Target::default()
        }
    }

    fn file(artist: &str, title: &str, number: u32) -> LandedFile {
        LandedFile {
            path: format!("mix/{artist} - {title}.mp3").into(),
            format: AudioFormat::Mp3,
            tag: AudioTag {
                artist: artist.to_owned(),
                title: title.to_owned(),
                track_number: number,
                disc_number: 1,
                ..AudioTag::default()
            },
            header: Default::default(),
            size_bytes: 1,
        }
    }

    /// The mixed compilation that was held under "On and On": John Mayer
    /// and Black Eyed Peas songs are foreign; Jack Johnson songs from other
    /// albums are the right artist and stay; a song of the album by a
    /// featured credit stays by its title.
    #[test]
    fn other_artists_off_the_album_are_foreign() {
        let release = on_and_on();
        let target = target();
        let foreign =
            |artist: &str, title: &str| is_foreign(&target, &release, &file(artist, title, 4));
        assert!(foreign("John Mayer", "Your Body Is a Wonderland"));
        assert!(foreign("John Mayer", "Daughters"));
        assert!(foreign("The Black Eyed Peas", "Gone Going"));
        assert!(!foreign("Jack Johnson", "Banana Pancakes"), "right artist");
        assert!(!foreign("jack johnson", "Upside Down"), "case folded");
        assert!(
            !foreign("Jack Johnson feat. Ben Harper", "Gone"),
            "featured"
        );
        assert!(!foreign("Someone Else", "Taylor"), "title on the album");
        assert!(!foreign("", "Daughters"), "untagged never counts");
    }

    /// A various-artists release with uncredited tracks has nobody to
    /// disagree with; a credited track artist agrees.
    #[test]
    fn various_artists_never_foreign_without_credits() {
        let mut release = on_and_on();
        release.artists = credit("Various Artists");
        let target = Target {
            artist_name: "Various Artists".to_owned(),
            ..target()
        };
        assert!(!is_foreign(
            &target,
            &release,
            &file("John Mayer", "Daughters", 1)
        ));
        release.tracks[0].artists = credit("John Mayer");
        assert!(!is_foreign(
            &target,
            &release,
            &file("John Mayer", "Daughters", 1)
        ));
    }

    /// A held file's own number is shown only when the release's track at
    /// that number is the file's song.
    #[test]
    fn numbers_only_from_matching_titles() {
        let release = on_and_on();
        assert_eq!(
            trusted_number(
                Some(&release),
                &file("John Mayer", "Your Body Is a Wonderland", 4)
            ),
            None
        );
        assert_eq!(
            trusted_number(Some(&release), &file("Jack Johnson", "Taylor", 4)),
            Some(4)
        );
        assert_eq!(
            trusted_number(Some(&release), &file("Jack Johnson", "Taylor", 5)),
            None,
            "right song, wrong number"
        );
        assert_eq!(
            trusted_number(None, &file("x", "y", 3)),
            Some(3),
            "no release"
        );
        assert_eq!(trusted_number(None, &file("x", "y", 0)), None);
    }

    #[test]
    fn foreign_artists_are_counted() {
        let landing = Landing {
            audio: vec![
                file("John Mayer", "Daughters", 1),
                file("john mayer", "Gravity", 2),
                file("Black Eyed Peas", "Shut Up", 3),
            ],
            ..Landing::default()
        };
        assert_eq!(
            foreign_artists(&landing, &[0, 1, 2]),
            vec![
                ("John Mayer".to_owned(), 2),
                ("Black Eyed Peas".to_owned(), 1)
            ]
        );
    }
}
