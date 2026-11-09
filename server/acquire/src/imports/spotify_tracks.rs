//! Fork: which MusicBrainz recording, and album, one Spotify song is.
//!
//! The album proof (`spotify_albums`) needs an ISRC that MusicBrainz also
//! holds, which many recordings lack. A song it leaves unresolved is looked
//! up by itself: a recording search by title and artist, where a hit
//! counts when its title agrees (versions and credits aside), one of its
//! credited artists stands for one of Spotify's, and its length is within
//! a few seconds of Spotify's. The song then goes to the album it comes
//! from: a release group whose title agrees with Spotify's album, else the
//! earliest official album carrying it, else the earliest release group.
//! With no hit that agrees, the song stays a want.

use droppedneedle_library::matching::score::NAME_GATE;
use droppedneedle_library::matching::strings::{artist_dist, artist_subset, string_dist};
use droppedneedle_providers::musicbrainz::models::RecordingSearchHit;

use crate::landing::want::{LENGTH_TOLERANCE_SECS, core_title};

/// Recording hits read per search.
pub const TRACK_SEARCH_LIMIT: u32 = 15;

/// One Spotify song as the lookup knows it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpotifySong {
    pub title: String,
    /// Every credited artist, in Spotify's order.
    pub artists: Vec<String>,
    pub album: String,
    pub seconds: Option<u64>,
}

/// A song placed on MusicBrainz.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProvenTrack {
    pub recording: String,
    pub group: String,
}

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

fn titles_agree(a: &str, b: &str) -> bool {
    let (x, y) = (core_title(a), core_title(b));
    (!x.is_empty() && string_dist(&x, &y) <= NAME_GATE) || string_dist(a, b) <= NAME_GATE
}

/// Whether a recording hit is the song.
pub fn hit_agrees(song: &SpotifySong, hit: &RecordingSearchHit) -> bool {
    let Some(title) = hit.title.as_deref() else {
        return false;
    };
    if !titles_agree(title, &song.title) {
        return false;
    }
    let artists = hit.artist_credit.iter().any(|credit| {
        song.artists
            .iter()
            .any(|asked| names_agree(&credit.name, asked))
    });
    if !artists {
        return false;
    }
    match (song.seconds, hit.length) {
        (Some(asked), Some(ms)) => {
            (ms as f64 / 1000.0 - asked as f64).abs() <= LENGTH_TOLERANCE_SECS
        }
        // A hit without a length counts only on the album's word.
        (Some(_), None) => hit.releases.iter().any(|release| {
            release
                .release_group
                .as_ref()
                .and_then(|group| group.title.as_deref())
                .is_some_and(|group| titles_agree(group, &song.album))
        }),
        (None, _) => true,
    }
}

/// The recording and album the hits that agree with the song name.
pub fn choose(song: &SpotifySong, hits: &[RecordingSearchHit]) -> Option<ProvenTrack> {
    // (recording, group id, group title, primary type, date, official)
    let mut on: Vec<(String, String, String, String, String, bool)> = Vec::new();
    for hit in hits.iter().filter(|hit| hit_agrees(song, hit)) {
        for release in &hit.releases {
            let Some(group) = &release.release_group else {
                continue;
            };
            on.push((
                hit.id.to_lowercase(),
                group.id.to_lowercase(),
                group.title.clone().unwrap_or_default(),
                group.primary_type.clone().unwrap_or_default(),
                group
                    .first_release_date
                    .clone()
                    .or_else(|| release.date.clone())
                    .filter(|date| !date.is_empty())
                    .unwrap_or_else(|| "9999".to_owned()),
                release
                    .status
                    .as_deref()
                    .is_none_or(|status| status.eq_ignore_ascii_case("official")),
            ));
        }
    }
    let rank = |type_: &str| match type_.to_ascii_lowercase().as_str() {
        "album" => 0,
        "ep" => 1,
        "single" => 2,
        _ => 3,
    };
    let named: Vec<_> = on
        .iter()
        .filter(|row| !song.album.trim().is_empty() && titles_agree(&row.2, &song.album))
        .collect();
    let pool: Vec<_> = if named.is_empty() {
        on.iter().filter(|row| row.5).collect()
    } else {
        named
    };
    pool.into_iter()
        .min_by(|a, b| {
            rank(&a.3)
                .cmp(&rank(&b.3))
                .then_with(|| a.4.cmp(&b.4))
                .then_with(|| a.1.cmp(&b.1))
        })
        .map(|row| ProvenTrack {
            recording: row.0.clone(),
            group: row.1.clone(),
        })
}

#[cfg(test)]
mod tests {
    use super::{SpotifySong, choose};
    use droppedneedle_providers::musicbrainz::models::RecordingSearchHit;

    fn hit(
        id: &str,
        title: &str,
        artist: &str,
        ms: u64,
        groups: &[(&str, &str, &str, &str)],
    ) -> RecordingSearchHit {
        let releases: Vec<serde_json::Value> = groups
            .iter()
            .map(|(gid, gtitle, kind, date)| {
                serde_json::json!({"id": format!("rel-{gid}"), "status": "Official", "date": date,
                    "release-group": {"id": gid, "title": gtitle, "primary-type": kind}})
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "id": id, "title": title, "length": ms,
            "artist-credit": [{"name": artist, "joinphrase": "", "artist": {"id": "a", "name": artist}}],
            "releases": releases,
        }))
        .expect("hit")
    }

    fn song(title: &str, artists: &[&str], album: &str, seconds: u64) -> SpotifySong {
        SpotifySong {
            title: title.to_owned(),
            artists: artists.iter().map(|a| (*a).to_owned()).collect(),
            album: album.to_owned(),
            seconds: Some(seconds),
        }
    }

    #[test]
    fn a_song_goes_to_the_album_spotify_names() {
        let hits = [hit(
            "r1",
            "Afro Blue",
            "Robert Glasper Experiment",
            308_000,
            &[
                ("single", "Afro Blue", "Single", "2011"),
                ("black-radio", "Black Radio", "Album", "2012"),
            ],
        )];
        let asked = song(
            "Afro Blue",
            &["Robert Glasper", "Erykah Badu"],
            "Black Radio",
            308,
        );
        assert_eq!(
            choose(&asked, &hits).map(|p| p.group),
            Some("black-radio".to_owned())
        );
    }

    #[test]
    fn without_the_album_the_earliest_album_wins_over_a_single() {
        let hits = [hit(
            "r1",
            "Sweet Time",
            "Raveena",
            251_000,
            &[
                ("single", "Sweet Time", "Single", "2017"),
                ("comp", "Best Of", "Compilation", "2016"),
                ("album", "Shanti", "Album", "2018"),
            ],
        )];
        let asked = song("Sweet Time", &["Raveena"], "Unknown Spotify Album", 252);
        assert_eq!(
            choose(&asked, &hits).map(|p| p.group),
            Some("album".to_owned())
        );
    }

    #[test]
    fn a_different_length_or_artist_is_not_the_song() {
        let long = [hit(
            "r1",
            "Supernatural",
            "KING",
            406_000,
            &[("g", "We Are KING", "Album", "2016")],
        )];
        assert!(
            choose(
                &song("Supernatural", &["We Are KING"], "We Are KING", 240),
                &long
            )
            .is_none()
        );
        let other = [hit(
            "r2",
            "Home",
            "Someone Else",
            217_000,
            &[("g", "Home", "Single", "2020")],
        )];
        assert!(choose(&song("Home", &["LaBlue", "Astrønne"], "Home", 217), &other).is_none());
    }
}
