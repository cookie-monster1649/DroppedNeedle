//! Fork: the release a song with no MusicBrainz release is filed on (a
//! want, or a Spotify song picked with Match).
//!
//! It is shaped so every song of one album lands in one folder, as one
//! library album, however many separate asks brought them in:
//! - the album is the provider's (Spotify's) album name, else the file's
//!   album tag, else the song itself (a single);
//! - where the file's album tag names the same album, its disc and track
//!   numbers, year and album artist are kept (so the album sorts in order
//!   and a compilation stays under its album artist);
//! - an album artist that is the asked artist in other letter case takes
//!   the provider's spelling, so one artist never gets two folders.

use droppedneedle_library::matching::strings::fold;
use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseTrack, SourceLink};
use droppedneedle_library::tags::read::AudioTag;

/// What was asked for, as the provider named it.
#[derive(Debug, Clone, Default)]
pub struct Ask {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub length_ms: Option<u64>,
}

fn named(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// The release `tags` (one per file, in file order) are placed on.
#[must_use]
pub fn release(ask: &Ask, tags: &[&AudioTag], links: &[SourceLink]) -> Release {
    let artist = named(Some(&ask.artist))
        .unwrap_or("Unknown Artist")
        .to_owned();
    let title = named(Some(&ask.title))
        .unwrap_or("Unknown Track")
        .to_owned();
    let asked_album = named(ask.album.as_deref()).filter(|album| *album != "Single track");
    let tagged_album = tags.iter().find_map(|tag| named(Some(&tag.album)));
    let album = asked_album.or(tagged_album).map(str::to_owned);
    let same_album = |tag: &AudioTag| {
        album
            .as_deref()
            .is_some_and(|album| !tag.album.trim().is_empty() && fold(&tag.album) == fold(album))
    };

    // The primary artist of a joined credit ("basecamp, Dvwn"), only to
    // recognise the album artist; the full credit stays on the track.
    let primary = artist.split(", ").next().unwrap_or(&artist).to_owned();
    let album_artist = tags
        .iter()
        .filter(|tag| same_album(tag))
        .find_map(|tag| named(tag.album_artist.as_deref()))
        .map(|tagged| {
            if fold(tagged) == fold(&artist) {
                artist.clone()
            } else if fold(tagged) == fold(&primary) {
                primary.clone()
            } else {
                tagged.to_owned()
            }
        })
        .unwrap_or_else(|| primary.clone());
    let year = tags
        .iter()
        .filter(|tag| same_album(tag))
        .find_map(|tag| tag.year);

    let isrcs: Vec<String> = links
        .iter()
        .filter(|link| link.provider == "isrc")
        .map(|link| link.external_id.clone())
        .collect();
    let credit = vec![CreditedArtist {
        name: artist.clone(),
        ..CreditedArtist::default()
    }];
    let tracks = tags
        .iter()
        .enumerate()
        .map(|(index, tag)| {
            let fallback = u32::try_from(index + 1).unwrap_or(u32::MAX);
            let (disc, position) = if same_album(tag) && tag.track_number > 0 {
                (tag.disc_number.max(1), tag.track_number)
            } else {
                (1, fallback)
            };
            ReleaseTrack {
                title: title.clone(),
                artists: credit.clone(),
                disc,
                position,
                absolute_position: position,
                length_ms: ask.length_ms,
                isrcs: isrcs.clone(),
                links: links.to_vec(),
                ..ReleaseTrack::default()
            }
        })
        .collect();
    let single = album.is_none();
    Release {
        title: album.unwrap_or_else(|| title.clone()),
        artists: vec![CreditedArtist {
            name: album_artist,
            ..CreditedArtist::default()
        }],
        date: year.map(|year| year.to_string()),
        primary_type: Some(if single { "Single" } else { "Album" }.to_owned()),
        tracks,
        ..Release::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(album: &str, album_artist: Option<&str>, disc: u32, track: u32) -> AudioTag {
        AudioTag {
            album: album.to_owned(),
            album_artist: album_artist.map(str::to_owned),
            disc_number: disc,
            track_number: track,
            year: Some(2022),
            ..AudioTag::default()
        }
    }

    fn ask(artist: &str, album: Option<&str>) -> Ask {
        Ask {
            title: "HOW TO SAY".to_owned(),
            artist: artist.to_owned(),
            album: album.map(str::to_owned),
            length_ms: None,
        }
    }

    #[test]
    fn songs_of_one_album_share_its_name_artist_and_numbers() {
        let file = tag("underground", Some("BASECAMP"), 1, 6);
        let shaped = release(&ask("basecamp, Dvwn", Some("underground")), &[&file], &[]);
        assert_eq!(shaped.title, "underground");
        assert_eq!(shaped.artists[0].name, "basecamp");
        assert_eq!(shaped.tracks[0].artists[0].name, "basecamp, Dvwn");
        assert_eq!((shaped.tracks[0].disc, shaped.tracks[0].position), (1, 6));
        assert_eq!(shaped.date.as_deref(), Some("2022"));
        assert_eq!(shaped.primary_type.as_deref(), Some("Album"));
    }

    #[test]
    fn a_file_tagged_with_another_album_keeps_the_asked_album_only() {
        let file = tag("Coffee Talk OST", Some("Someone Else"), 2, 9);
        let shaped = release(&ask("2xxx", Some("Life+unconsciousness")), &[&file], &[]);
        assert_eq!(shaped.title, "Life+unconsciousness");
        assert_eq!(shaped.artists[0].name, "2xxx");
        assert_eq!((shaped.tracks[0].disc, shaped.tracks[0].position), (1, 1));
        assert_eq!(shaped.date, None);
    }

    #[test]
    fn a_compilation_stays_under_its_album_artist() {
        let file = tag("~ fleeting moments ~", Some("Various Artists"), 1, 5);
        let shaped = release(
            &ask("leon chang", Some("~ fleeting moments ~")),
            &[&file],
            &[],
        );
        assert_eq!(shaped.artists[0].name, "Various Artists");
        assert_eq!(shaped.tracks[0].position, 5);
    }

    #[test]
    fn with_no_album_anywhere_the_song_is_its_own_single() {
        let file = tag("", None, 0, 0);
        let shaped = release(&ask("Colde", None), &[&file], &[]);
        assert_eq!(shaped.title, "HOW TO SAY");
        assert_eq!(shaped.primary_type.as_deref(), Some("Single"));
        assert_eq!(shaped.artists[0].name, "Colde");
    }
}
