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
    if tag.title.trim().is_empty() || string_dist(&tag.title, title) > NAME_GATE {
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
