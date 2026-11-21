//! Fork: landing a want (a track asked for by name, `want-<hash>`).
//!
//! MusicBrainz has nothing to check a want's files against, so the ask is
//! the reference: its artist, title and length (from the provider that
//! named it, Spotify for a playlist row). A file that agrees with all
//! three is held paired to the ask, and the held list's loop files it
//! through the held Import (`held::service::want_release`) a few minutes
//! later, with the provider's ids as source links. A file that does not
//! agree is held unpaired for a person, as before.

use droppedneedle_library::matching::score::NAME_GATE;
use droppedneedle_library::matching::strings::{artist_dist, artist_subset, string_dist};
use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseTrack};

use super::matching::FoundRelease;
use super::probe::LandedFile;
use super::specs::Target;
use crate::requests::ledger::is_want_key;

/// How far a file's length may be from the asked length, in seconds: a
/// different master or a few seconds of silence, not a different version.
pub const LENGTH_TOLERANCE_SECS: f64 = 7.0;

/// The want key of a target asked for by name.
pub fn want_key(target: &Target) -> Option<&str> {
    target
        .recording_mbid
        .as_deref()
        .filter(|key| is_want_key(key))
}

/// Whether `file` is the asked track: its title tag agrees with the asked
/// title, its artist tag with the asked artist (or names one of the asked
/// artists, for a credit such as "Bonobo, Bajka"), and its length is within
/// [`LENGTH_TOLERANCE_SECS`] of the asked length. With no asked length, or
/// a file with no title or length, nothing is shown.
pub fn agrees(target: &Target, file: &LandedFile) -> bool {
    let (Some(title), Some(asked)) = (target.track_title.as_deref(), target.want_seconds) else {
        return false;
    };
    let Some(seconds) = file.header.duration_seconds else {
        return false;
    };
    let tag = &file.tag;
    // Fork: or once both titles set aside what versions, credits and a
    // " | …" or " - …" tail add ("Al Ayloughs | Նարինե Դովլաթյան - …");
    // the length below still tells a remix from the song.
    let core = core_title(&tag.title);
    if tag.title.trim().is_empty()
        || (string_dist(&tag.title, title) > NAME_GATE
            && (core.is_empty() || string_dist(&core, &core_title(title)) > NAME_GATE))
    {
        return false;
    }
    let artist = if tag.artist.trim().is_empty() {
        tag.album_artist.as_deref().unwrap_or_default()
    } else {
        tag.artist.as_str()
    };
    let artist_ok = !artist.trim().is_empty()
        && (artist_dist(artist, &target.artist_name) <= NAME_GATE
            || artist_subset(artist, &target.artist_name));
    artist_ok && (seconds - asked).abs() <= LENGTH_TOLERANCE_SECS
}

/// The ask as a one-track release a held file is paired to: no MusicBrainz
/// ids (the want key stands where the recording would), the asked title
/// and length.
pub fn ask(target: &Target) -> Option<FoundRelease> {
    let key = want_key(target)?;
    let title = target.track_title.clone()?;
    let credit = vec![CreditedArtist {
        name: target.artist_name.clone(),
        ..CreditedArtist::default()
    }];
    let release = Release {
        title: title.clone(),
        artists: credit.clone(),
        tracks: vec![ReleaseTrack {
            recording_id: key.to_owned(),
            title,
            artists: credit,
            disc: 1,
            position: 1,
            absolute_position: 1,
            length_ms: target
                .want_seconds
                .map(|seconds| (seconds * 1000.0).round() as u64),
            ..ReleaseTrack::default()
        }],
        ..Release::default()
    };
    // Scored against nothing: the file was judged by [`agrees`]; the
    // score only fills the shape a pairing carries.
    let scored = droppedneedle_library::matching::match_release(
        &droppedneedle_library::matching::LocalAlbum::default(),
        &release,
        &std::collections::HashMap::new(),
    );
    Some(FoundRelease { release, scored })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn target(seconds: Option<f64>) -> Target {
        Target {
            artist_name: "Bonobo, Bajka".to_owned(),
            recording_mbid: Some(format!("want-{}", "0".repeat(32))),
            track_title: Some("Days To Come".to_owned()),
            is_track: true,
            want_seconds: seconds,
            ..Target::default()
        }
    }

    fn file(title: &str, artist: &str, seconds: f64) -> LandedFile {
        LandedFile {
            path: PathBuf::from("/d/x.flac"),
            format: droppedneedle_library::tags::AudioFormat::Flac,
            tag: droppedneedle_library::tags::AudioTag {
                title: title.to_owned(),
                artist: artist.to_owned(),
                ..Default::default()
            },
            header: droppedneedle_library::tags::read::HeaderInfo {
                duration_seconds: Some(seconds),
                ..Default::default()
            },
            size_bytes: 1,
        }
    }

    #[test]
    fn a_file_agrees_on_title_artist_and_length() {
        let asked = target(Some(230.0));
        assert!(agrees(&asked, &file("Days To Come", "Bonobo", 233.0)));
        assert!(agrees(
            &asked,
            &file("Days to Come", "Bonobo, Bajka", 226.0)
        ));
        assert!(
            !agrees(&asked, &file("Days To Come", "Bonobo", 260.0)),
            "length"
        );
        assert!(
            !agrees(&asked, &file("We Got to Have Peace", "Bonobo", 230.0)),
            "title"
        );
        assert!(
            !agrees(&asked, &file("Days To Come", "Curtis Mayfield", 230.0)),
            "artist"
        );
        assert!(!agrees(&asked, &file("", "Bonobo", 230.0)), "no title");
        assert!(
            agrees(
                &asked,
                &file("Days To Come | Live Session", "Bonobo", 231.0)
            ),
            "a tail after the title"
        );
        assert!(
            agrees(&asked, &file("Days To Come (2006)", "Bonobo", 231.0)),
            "a bracketed part"
        );
        assert!(
            !agrees(
                &asked,
                &file("Days To Come (Extended Mix)", "Bonobo", 380.0)
            ),
            "a longer mix"
        );
        assert!(
            !agrees(&target(None), &file("Days To Come", "Bonobo", 230.0)),
            "no length"
        );
    }

    #[test]
    fn the_ask_is_a_one_track_release_keyed_by_the_want() {
        let found = ask(&target(Some(230.4))).expect("a want");
        let track = &found.release.tracks[0];
        assert!(found.release.id.is_empty());
        assert!(is_want_key(&track.recording_id));
        assert_eq!(track.length_ms, Some(230_400));
        assert!(ask(&Target::default()).is_none());
    }
}

/// Fork: a title without what versions and credits add: bracketed parts
/// ("(feat. …)", "[Prod. …]", "(EP Version)"), a " - …" suffix and a bare
/// "feat. …" tail.
pub fn core_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut depth = 0usize;
    for ch in title.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    let lower = out.to_lowercase();
    let mut cut = lower.len();
    for marker in [" - ", " | ", " feat.", " feat ", " ft.", " featuring "] {
        if let Some(at) = lower.find(marker) {
            cut = cut.min(at);
        }
    }
    lower[..cut].trim().to_owned()
}

/// Fork: why `file` is plainly not the asked song, if it is: its title
/// (once versions and credits are set aside) neither agrees with the asked
/// title nor holds it nor is held by it, or its tags name an artist that
/// stands for none of the asked ones. A file whose tags say nothing is
/// not judged.
pub fn plainly_other(target: &Target, file: &LandedFile) -> Option<String> {
    let asked = target.track_title.as_deref()?;
    let tag = &file.tag;
    let title = tag.title.trim();
    if !title.is_empty() {
        let (have, want) = (core_title(title), core_title(asked));
        let fold = |text: &str| droppedneedle_library::matching::strings::fold(text);
        let agrees = string_dist(title, asked) <= NAME_GATE
            || (!have.is_empty()
                && !want.is_empty()
                && (string_dist(&have, &want) <= NAME_GATE
                    || fold(&have).contains(&fold(&want))
                    || fold(&want).contains(&fold(&have))));
        if !agrees {
            return Some(format!(
                "the file is '{title}', not the requested '{asked}'"
            ));
        }
    }
    let named: Vec<&str> = [Some(tag.artist.as_str()), tag.album_artist.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    if named.is_empty() {
        return None;
    }
    let asked_artists: Vec<&str> = target
        .artist_name
        .split([',', '&'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    let stands_for_one = named.iter().any(|heard| {
        names_agree(heard, &target.artist_name)
            || asked_artists.iter().any(|asked| names_agree(heard, asked))
            || heard
                .split([',', '&', '/', ';'])
                .flat_map(|part| part.split(" x ").flat_map(|p| p.split(" X ")))
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .any(|part| asked_artists.iter().any(|asked| names_agree(part, asked)))
    });
    (!stands_for_one).then(|| {
        format!(
            "the file is by '{}', not the requested '{}'",
            named[0], target.artist_name
        )
    })
}

/// Fork: whether one credited name stands for one of the asked artists:
/// close, or one holds the other ("Robert Glasper Experiment" for "Robert
/// Glasper", "KING" for "We Are KING").
fn names_agree(heard: &str, asked: &str) -> bool {
    let (heard, asked) = (heard.trim().to_lowercase(), asked.trim().to_lowercase());
    if heard.is_empty() || asked.is_empty() {
        return false;
    }
    artist_dist(&heard, &asked) <= NAME_GATE
        || artist_subset(&heard, &asked)
        || (heard.len() >= 3 && asked.contains(&heard))
        || (asked.len() >= 3 && heard.contains(&asked))
}

/// Fork: whether what AcoustID heard in a file (a recording's `title` and
/// credited `artists`) is the asked song: the titles agree once versions
/// and credits are set aside, any heard artist stands for any asked one,
/// and the file's length is within [`LENGTH_TOLERANCE_SECS`] of the asked
/// length when both are known. A remix or extended mix differs in length
/// or title and is not taken.
pub fn heard_agrees(
    target: &Target,
    title: &str,
    artists: &[String],
    seconds: Option<f64>,
) -> bool {
    let Some(asked_title) = target.track_title.as_deref() else {
        return false;
    };
    let (heard, asked) = (core_title(title), core_title(asked_title));
    let titles = !heard.is_empty()
        && (string_dist(&heard, &asked) <= NAME_GATE
            || string_dist(title, asked_title) <= NAME_GATE);
    if !titles {
        return false;
    }
    let asked_artists: Vec<&str> = target
        .artist_name
        .split([',', '&'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    let artists_ok = artists.iter().any(|heard| {
        names_agree(heard, &target.artist_name)
            || asked_artists.iter().any(|asked| names_agree(heard, asked))
    });
    if !artists_ok {
        return false;
    }
    match (target.want_seconds, seconds) {
        (Some(asked), Some(seconds)) => (seconds - asked).abs() <= LENGTH_TOLERANCE_SECS,
        _ => true,
    }
}

#[cfg(test)]
mod fork_tests {
    use super::{core_title, heard_agrees};
    use crate::landing::specs::Target;

    fn ask(artist: &str, title: &str, seconds: f64) -> Target {
        Target {
            artist_name: artist.to_owned(),
            track_title: Some(title.to_owned()),
            want_seconds: Some(seconds),
            ..Default::default()
        }
    }

    fn landed(title: &str, artist: &str) -> crate::landing::probe::LandedFile {
        crate::landing::probe::LandedFile {
            path: std::path::PathBuf::from("/d/x.mp3"),
            format: droppedneedle_library::tags::AudioFormat::Mp3,
            tag: droppedneedle_library::tags::AudioTag {
                title: title.to_owned(),
                artist: artist.to_owned(),
                ..Default::default()
            },
            header: Default::default(),
            size_bytes: 1,
        }
    }

    /// Fork: another title or another artist is plainly another song; a
    /// tail, a credit or one of several asked artists is not.
    #[test]
    fn a_plainly_other_song_is_told_apart() {
        use super::plainly_other;
        let high = ask("Shkolnik", "High", 217.0);
        assert!(plainly_other(&high, &landed("Skiptracing", "Mild High Club")).is_some());
        assert!(
            plainly_other(
                &high,
                &landed("High in the Air", "Paul K and The Weathermen")
            )
            .is_some()
        );
        let dill = ask("S I M", "Dill Pickles", 138.0);
        assert!(
            plainly_other(
                &dill,
                &landed("Charles L. Johnson: Dill Pickles", "Max Morath")
            )
            .is_some()
        );
        let al = ask("Narine Dovlatyan", "Al Ayloughs", 212.0);
        assert!(
            plainly_other(
                &al,
                &landed(
                    "Al Ayloughs | Նարինե Դովլաթյան - Ալ Այլուղս",
                    "Narine Dovlatyan"
                )
            )
            .is_none()
        );
        let beggin = ask("Evil Genius, Isaac Waddington", "Beggin' Man", 202.0);
        assert!(
            plainly_other(
                &beggin,
                &landed("Beggin' Man", "Evil Genius X Isaac Waddington")
            )
            .is_none()
        );
        let reich = ask(
            "Steve Reich, David Robertson, Orchestre National De Lyon",
            "Triple quarte (for string orchestra) III.",
            219.0,
        );
        assert!(
            plainly_other(
                &reich,
                &landed(
                    "Triple quarte (for string orchestra) (Ii.)",
                    "David Robertson"
                )
            )
            .is_none(),
            "a movement number is left to the length"
        );
        let amaal = ask("Amaal Nuux", "Later", 226.0);
        assert!(plainly_other(&amaal, &landed("Later", "Amaal")).is_none());
        assert!(
            plainly_other(&amaal, &landed("", "")).is_none(),
            "nothing to judge"
        );
    }

    #[test]
    fn versions_and_credits_are_set_aside() {
        assert_eq!(core_title("Spice - EP Version"), "spice");
        assert_eq!(core_title("Spice (EP Version)"), "spice");
        assert_eq!(
            core_title("Sango [Prod. By Nathaniel Geographica]"),
            "sango"
        );
        assert_eq!(
            core_title("Diddy Bop (feat. Raury & Cam O'bi)"),
            "diddy bop"
        );
    }

    #[test]
    fn what_acoustid_heard_matches_the_ask_by_name() {
        let names = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let afro = ask("Robert Glasper, Erykah Badu", "Afro Blue", 308.0);
        assert!(heard_agrees(
            &afro,
            "Afro Blue",
            &names(&["Robert Glasper Experiment"]),
            Some(308.4)
        ));
        let king = ask("We Are KING", "Supernatural", 240.0);
        assert!(!heard_agrees(
            &king,
            "Supernatural (Extended Mix)",
            &names(&["KING"]),
            Some(406.0)
        ));
        assert!(heard_agrees(
            &king,
            "Supernatural",
            &names(&["KING"]),
            Some(241.0)
        ));
        let other = ask("thando", "Something", 188.0);
        assert!(!heard_agrees(
            &other,
            "Wozzeck, Act I Scene 2",
            &names(&["Paul Daniel"]),
            Some(188.0)
        ));
    }
}
