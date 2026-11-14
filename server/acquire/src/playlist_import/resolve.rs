//! Which album a tagged file is, over the same lookups the Spotify
//! playlist sync resolves a song with ([`crate::imports::spotify_resolve`]),
//! so a song imported here and the same song in a synced playlist agree on
//! their album and recording.
//!
//! The order: the ISRC at MusicBrainz; the ISRC at Deezer (the Deezer
//! album kept as a stand-in, then checked at MusicBrainz by its barcode
//! or a confident match); the file's barcode at MusicBrainz; title,
//! artist and length at MusicBrainz; the Deezer album the file names
//! (again checked at MusicBrainz).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::imports::spotify_resolve::{AlbumFacts, Lookups, ResolveError, TrackFacts};
use crate::stand_ins::StandInCheck;
use droppedneedle_providers::ProviderError;
use droppedneedle_providers::virtual_ids::is_virtual;

/// What resolved a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    IsrcMusicbrainz,
    IsrcDeezer,
    BarcodeMusicbrainz,
    NameMusicbrainz,
    NameDeezer,
    /// Fork: matched to an entry of the folder's public Spotify playlist,
    /// then resolved by that entry's ISRC or album.
    SpotifyPlaylist,
}

/// A resolved file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolved {
    pub via: Via,
    /// The album to import into: a MusicBrainz group, or a Deezer
    /// stand-in MusicBrainz does not know.
    pub album: String,
    /// The Deezer stand-in the album was found as, when it was.
    pub stand_in: Option<String>,
    /// The recording (MusicBrainz, or Deezer's stand-in); empty when only
    /// the album was proved (by barcode).
    pub recording: String,
}

/// Fork: why one song's lookup failed, sorted by what the run does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupFailure {
    /// This one request was refused (a 403, another 4xx, an unreadable
    /// answer): the song is set aside and the run goes on.
    Refused(String),
    /// A rate limit or quota: wait, then ask again.
    Limited {
        message: String,
        wait: Option<Duration>,
    },
    /// A service is down or the catalogue failed: the run stops.
    Down(String),
}

impl std::fmt::Display for LookupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(message) | Self::Limited { message, .. } | Self::Down(message) => {
                f.write_str(message)
            }
        }
    }
}

impl From<ResolveError> for LookupFailure {
    fn from(error: ResolveError) -> Self {
        let message = error.to_string();
        match error {
            ResolveError::Provider(provider) => match provider {
                ProviderError::RateLimited { retry_after, .. } => Self::Limited {
                    message,
                    wait: retry_after,
                },
                ProviderError::Forbidden { .. }
                | ProviderError::NotFound { .. }
                | ProviderError::BadRequest { .. }
                | ProviderError::Redirect { .. }
                | ProviderError::Payload { .. }
                | ProviderError::Invalid { .. } => Self::Refused(message),
                _ => Self::Down(message),
            },
            ResolveError::Store(_) => Self::Down(message),
        }
    }
}

/// A failure only known by its wording (the stand-in check): the run stops.
impl From<String> for LookupFailure {
    fn from(message: String) -> Self {
        Self::Down(message)
    }
}

/// The facts a file's tags give, as the resolver reads a Spotify song.
pub fn facts(
    title: &str,
    artist: &str,
    album: &str,
    album_artist: Option<&str>,
    isrc: Option<&str>,
    barcode: Option<&str>,
    seconds: Option<f64>,
) -> TrackFacts {
    let clean = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let artist = artist.trim().to_owned();
    let album_artist = clean(album_artist).unwrap_or_else(|| artist.clone());
    TrackFacts {
        title: title.trim().to_owned(),
        artists: if artist.is_empty() {
            Vec::new()
        } else {
            vec![artist]
        },
        album: AlbumFacts {
            spotify_id: String::new(),
            title: album.trim().to_owned(),
            artists: if album_artist.is_empty() {
                Vec::new()
            } else {
                vec![album_artist]
            },
            upc: clean(barcode).filter(|code| code.chars().all(|c| c.is_ascii_digit())),
            total_tracks: None,
        },
        isrc: clean(isrc).map(|code| code.replace('-', "").to_ascii_uppercase()),
        seconds: seconds
            .filter(|s| s.is_finite() && *s > 0.0)
            .map(|s| s.round() as u64),
    }
}

/// A Deezer answer, checked at MusicBrainz.
pub(super) async fn from_deezer(
    via: Via,
    stand_in: String,
    recording: String,
    check: &dyn StandInCheck,
) -> Result<Resolved, String> {
    let album = if is_virtual(&stand_in) {
        match check.musicbrainz_of(&stand_in).await {
            Ok(Some((group, _))) => group,
            Ok(None) => stand_in.clone(),
            Err(error) => return Err(error),
        }
    } else {
        stand_in.clone()
    };
    Ok(Resolved {
        via,
        stand_in: is_virtual(&stand_in).then(|| stand_in.clone()),
        album,
        recording,
    })
}

/// The album and recording a file is, in the order the module names.
/// `Ok(None)` when nothing is sure.
///
/// # Errors
///
/// A lookup failed; [`LookupFailure`] says whether the run goes on.
pub async fn resolve(
    track: &TrackFacts,
    lookups: &dyn Lookups,
    check: &dyn StandInCheck,
) -> Result<Option<Resolved>, LookupFailure> {
    resolve_in_order(track, lookups, check).await
}

async fn resolve_in_order(
    track: &TrackFacts,
    lookups: &dyn Lookups,
    check: &dyn StandInCheck,
) -> Result<Option<Resolved>, LookupFailure> {
    if track.isrc.is_some() {
        if let Some((album, recording)) = lookups.mb_track_by_isrc(track).await? {
            return Ok(Some(Resolved {
                via: Via::IsrcMusicbrainz,
                album,
                stand_in: None,
                recording,
            }));
        }
        if let Some((stand_in, recording)) = lookups.deezer_track_by_isrc(track).await? {
            return Ok(Some(
                from_deezer(Via::IsrcDeezer, stand_in, recording, check).await?,
            ));
        }
    }
    if track.album.upc.is_some()
        && let Some(album) = lookups.mb_album_by_barcode(&track.album).await?
    {
        return Ok(Some(Resolved {
            via: Via::BarcodeMusicbrainz,
            album,
            stand_in: None,
            recording: String::new(),
        }));
    }
    if track.title.is_empty() || track.artists.is_empty() || track.seconds.is_none() {
        return Ok(None);
    }
    if let Some((album, recording)) = lookups.mb_track_by_name(track).await? {
        return Ok(Some(Resolved {
            via: Via::NameMusicbrainz,
            album,
            stand_in: None,
            recording,
        }));
    }
    match lookups.deezer_track_by_name(track).await? {
        Some((stand_in, recording)) => Ok(Some(
            from_deezer(Via::NameDeezer, stand_in, recording, check).await?,
        )),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imports::spotify_resolve::{ArtistFacts, Placed};
    use droppedneedle_kernel::jobs::BoxFuture;
    use droppedneedle_library::sources::links::LinkVia;
    use droppedneedle_providers::virtual_ids::{Kind, mint};
    use std::sync::Mutex;

    const MB_ALBUM: &str = "ea8e626f-520f-4f54-9fab-8004e702afef";
    const MB_REC: &str = "1dbbabcb-6902-40ec-8a7f-bcb1aa15d6bd";

    /// Lookups answering only the ones named, and recording what was asked.
    #[derive(Default)]
    struct Fake {
        answers: Vec<&'static str>,
        asked: Mutex<Vec<&'static str>>,
    }

    impl Fake {
        fn with(answers: &[&'static str]) -> Self {
            Self {
                answers: answers.to_vec(),
                ..Self::default()
            }
        }
        fn pair(&self, name: &'static str, deezer: bool) -> Result<Option<Placed>, ResolveError> {
            self.asked.lock().unwrap().push(name);
            Ok(self.answers.contains(&name).then(|| {
                if deezer {
                    (mint("deezer", Kind::ReleaseGroup, "9"), "d-rec".to_owned())
                } else {
                    (MB_ALBUM.to_owned(), MB_REC.to_owned())
                }
            }))
        }
        fn id(&self, name: &'static str) -> Result<Option<String>, ResolveError> {
            self.asked.lock().unwrap().push(name);
            Ok(self.answers.contains(&name).then(|| MB_ALBUM.to_owned()))
        }
        fn asked(&self) -> Vec<&'static str> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Lookups for Fake {
        fn mb_track_by_isrc<'a>(
            &'a self,
            _: &'a TrackFacts,
        ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>> {
            Box::pin(async { self.pair("mb_isrc", false) })
        }
        fn deezer_track_by_isrc<'a>(
            &'a self,
            _: &'a TrackFacts,
        ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>> {
            Box::pin(async { self.pair("dz_isrc", true) })
        }
        fn mb_track_by_name<'a>(
            &'a self,
            _: &'a TrackFacts,
        ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>> {
            Box::pin(async { self.pair("mb_name", false) })
        }
        fn deezer_track_by_name<'a>(
            &'a self,
            _: &'a TrackFacts,
        ) -> BoxFuture<'a, Result<Option<Placed>, ResolveError>> {
            Box::pin(async { self.pair("dz_name", true) })
        }
        fn mb_album_by_barcode<'a>(
            &'a self,
            _: &'a AlbumFacts,
        ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
            Box::pin(async { self.id("mb_barcode") })
        }
        fn mb_album_by_link<'a>(
            &'a self,
            _: &'a AlbumFacts,
        ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
            Box::pin(async { self.id("mb_link") })
        }
        fn deezer_album_by_barcode<'a>(
            &'a self,
            _: &'a AlbumFacts,
        ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
            Box::pin(async { self.id("dz_barcode") })
        }
        fn mb_album_by_name<'a>(
            &'a self,
            _: &'a AlbumFacts,
        ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
            Box::pin(async { self.id("mb_album_name") })
        }
        fn deezer_album_by_name<'a>(
            &'a self,
            _: &'a AlbumFacts,
        ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
            Box::pin(async { self.id("dz_album_name") })
        }
        fn mb_artist_by_link<'a>(
            &'a self,
            _: &'a ArtistFacts,
        ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
            Box::pin(async { Ok(None) })
        }
        fn mb_artist_by_album<'a>(
            &'a self,
            _: &'a ArtistFacts,
        ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
            Box::pin(async { Ok(None) })
        }
        fn mb_artist_by_name<'a>(
            &'a self,
            _: &'a ArtistFacts,
        ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
            Box::pin(async { Ok(None) })
        }
        fn deezer_artist_by_name<'a>(
            &'a self,
            _: &'a ArtistFacts,
        ) -> BoxFuture<'a, Result<Option<String>, ResolveError>> {
            Box::pin(async { Ok(None) })
        }
    }

    /// A MusicBrainz check that knows every stand-in, or none.
    struct Check(bool);

    impl StandInCheck for Check {
        fn musicbrainz_of<'a>(
            &'a self,
            _: &'a str,
        ) -> BoxFuture<'a, Result<Option<(String, LinkVia)>, String>> {
            let known = self.0;
            Box::pin(async move { Ok(known.then(|| (MB_ALBUM.to_owned(), LinkVia::Barcode))) })
        }
    }

    fn tagged(isrc: Option<&str>, barcode: Option<&str>) -> TrackFacts {
        facts("Song", "Artist", "Album", None, isrc, barcode, Some(201.4))
    }

    #[test]
    fn tags_become_the_resolvers_facts() {
        let facts = facts(
            " Song ",
            "Artist",
            "EP",
            Some(""),
            Some("gb-abc-12-34567"),
            Some("x1"),
            Some(200.6),
        );
        assert_eq!(facts.isrc.as_deref(), Some("GBABC1234567"));
        assert_eq!(facts.album.upc, None);
        assert_eq!(facts.album.artists, vec!["Artist".to_owned()]);
        assert_eq!(facts.seconds, Some(201));
        assert_eq!(facts.title, "Song");
    }

    #[tokio::test]
    async fn an_isrc_resolves_at_musicbrainz_first() {
        let fake = Fake::with(&["mb_isrc", "dz_isrc"]);
        let hit = resolve(&tagged(Some("GBABC1234567"), None), &fake, &Check(false))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hit.via, Via::IsrcMusicbrainz);
        assert_eq!(hit.album, MB_ALBUM);
        assert_eq!(fake.asked(), vec!["mb_isrc"]);
    }

    #[tokio::test]
    async fn a_deezer_isrc_hit_is_checked_at_musicbrainz() {
        let fake = Fake::with(&["dz_isrc"]);
        let hit = resolve(&tagged(Some("GBABC1234567"), None), &fake, &Check(true))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hit.via, Via::IsrcDeezer);
        assert_eq!(hit.album, MB_ALBUM);
        assert_eq!(hit.stand_in, Some(mint("deezer", Kind::ReleaseGroup, "9")));
        let alone = resolve(&tagged(Some("GBABC1234567"), None), &fake, &Check(false))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(alone.album, mint("deezer", Kind::ReleaseGroup, "9"));
    }

    #[tokio::test]
    async fn without_an_isrc_the_barcode_then_names_are_tried() {
        let fake = Fake::with(&["mb_barcode"]);
        let hit = resolve(&tagged(None, Some("0123456789012")), &fake, &Check(false))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hit.via, Via::BarcodeMusicbrainz);
        let fake = Fake::with(&["dz_name"]);
        let hit = resolve(&tagged(None, None), &fake, &Check(false))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hit.via, Via::NameDeezer);
        assert_eq!(fake.asked(), vec!["mb_name", "dz_name"]);
        let fake = Fake::with(&[]);
        assert!(
            resolve(&tagged(None, None), &fake, &Check(false))
                .await
                .unwrap()
                .is_none()
        );
    }
}
