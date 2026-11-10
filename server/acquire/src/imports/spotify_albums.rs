//! Which MusicBrainz release group a Spotify album is.
//!
//! An album is matched only on proof, never on its name: the release
//! search by title and artist only finds candidates, and a candidate
//! counts when one of its tracks is a recording carrying an ISRC that
//! Spotify gave for a track of that album. One proven release group is the
//! answer. No ISRC, no proven candidate, or two proven groups (an album
//! and a same-named single carrying the same recording) leave the album
//! unresolved, so an import never points a request at the wrong music.
//!
//! The client has no ISRC lookup, so the proof is read from the candidate
//! release (`inc=recordings+isrcs`), which also keeps the calls to what the
//! album needs. Lookups run at background priority, paced by the
//! executor, and each album's answer is kept in the metadata cache.

use std::collections::BTreeSet;

use droppedneedle_kernel::runtime_config::{Live, sections::MusicBrainzSettings};
use droppedneedle_providers::cache::{CacheKey, Fetched, Policy, ProviderCacheStore, Source};
use droppedneedle_providers::musicbrainz::{
    Criticality, MusicBrainzClient, MusicBrainzView, SOURCE, is_valid_mbid, normalize_mb_id,
};
use droppedneedle_providers::{ProviderError, Providers, RequestPriority};

/// Releases read from one search. Editions of a proven group cost no
/// lookup, so this bounds the lookups to the distinct candidates.
const SEARCH_LIMIT: u32 = 5;

/// The includes a candidate is proven with.
const PROOF_INCLUDES: [&str; 3] = ["isrcs", "recordings", "release-groups"];

/// One Spotify album as the import knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyAlbum {
    /// Album title.
    pub title: String,
    /// The first credited artist of a track on it.
    pub artist: String,
    /// The ISRCs of its tracks on the playlist, uppercased and well formed.
    pub isrcs: BTreeSet<String>,
    /// Fork: the Spotify album id, empty when unknown. MusicBrainz releases
    /// link their Spotify album, which proves the album outright.
    pub spotify_id: String,
}

impl SpotifyAlbum {
    /// The album with `isrc` added when it is a well-formed ISRC.
    pub fn add_isrc(&mut self, isrc: &str) {
        let isrc = isrc.trim().to_ascii_uppercase();
        if is_isrc(&isrc) {
            self.isrcs.insert(isrc);
        }
    }
}

/// A Spotify album proven to be one release group, with the recording each
/// of its proving ISRCs names on that group (fork: per-track requests).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProvenAlbum {
    /// Release-group MBID.
    pub group: String,
    /// Uppercased ISRC -> recording MBID, for the ISRCs the album carried.
    pub recordings: std::collections::BTreeMap<String, String>,
    /// Fork: the release that proved it (the exact edition), when one did.
    #[serde(default)]
    pub release: Option<String>,
}

/// Finds the release group of Spotify albums through MusicBrainz.
#[derive(Clone)]
pub struct AlbumResolver {
    musicbrainz: MusicBrainzClient,
    settings: Live<MusicBrainzSettings>,
    cache: ProviderCacheStore,
}

impl AlbumResolver {
    /// The resolver over the shared providers, reading the MusicBrainz
    /// source from `settings` at each album.
    pub fn new(providers: &Providers, settings: Live<MusicBrainzSettings>) -> Self {
        Self {
            musicbrainz: providers.musicbrainz.clone(),
            settings,
            cache: providers.cache.clone(),
        }
    }

    /// The album's release group id, `None` when nothing proves one.
    ///
    /// # Errors
    ///
    /// A MusicBrainz failure, so the caller can stop asking while the
    /// service is down. Nothing is cached for it.
    pub async fn resolve(
        &self,
        album: &SpotifyAlbum,
    ) -> Result<Option<ProvenAlbum>, ProviderError> {
        if album.isrcs.is_empty() || album.title.trim().is_empty() {
            return Ok(None);
        }
        let Some(settings) = self.settings.read() else {
            return Err(ProviderError::NotConfigured {
                provider: SOURCE,
                message: "the musicbrainz settings cannot be read".to_owned(),
            });
        };
        let namespace = format!("{}:g{}", settings.source_id, settings.generation);
        let view = self.musicbrainz.fixed(
            self.musicbrainz.source_from(&settings),
            RequestPriority::BackgroundSync,
        );
        let isrcs: Vec<&str> = album.isrcs.iter().map(String::as_str).collect();
        let joined = isrcs.join(",");
        let key = CacheKey::hashed(
            Source::MusicBrainz,
            "spotify_album_recordings",
            &[
                &namespace,
                &album.title,
                &album.artist,
                &joined,
                &album.spotify_id,
            ],
        );
        let album = album.clone();
        self.cache
            .get_or_fetch(
                &key,
                Policy::RELEASE_TO_GROUP,
                RequestPriority::BackgroundSync,
                move |_| async move {
                    Ok(match prove(&view, &album).await? {
                        Some(group) => Fetched::Found(group),
                        None => Fetched::Missing,
                    })
                },
            )
            .await
    }

    /// Fork: the album's release group found by its barcode (Spotify's
    /// UPC), for an album the link and ISRC proofs left open. A barcode
    /// names one product, so the releases carrying it must agree on one
    /// release group.
    ///
    /// # Errors
    ///
    /// A MusicBrainz failure; nothing is cached for it.
    pub async fn resolve_barcode(
        &self,
        album: &SpotifyAlbum,
        upc: &str,
    ) -> Result<Option<ProvenAlbum>, ProviderError> {
        let upc: String = upc.chars().filter(char::is_ascii_digit).collect();
        if upc.len() < 8 {
            return Ok(None);
        }
        let Some(settings) = self.settings.read() else {
            return Err(ProviderError::NotConfigured {
                provider: SOURCE,
                message: "the musicbrainz settings cannot be read".to_owned(),
            });
        };
        let namespace = format!("{}:g{}", settings.source_id, settings.generation);
        let view = self.musicbrainz.fixed(
            self.musicbrainz.source_from(&settings),
            RequestPriority::BackgroundSync,
        );
        let joined = album
            .isrcs
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(",");
        let key = CacheKey::hashed(
            Source::MusicBrainz,
            "spotify_album_barcode",
            &[&namespace, &upc, &joined],
        );
        let album = album.clone();
        self.cache
            .get_or_fetch(
                &key,
                Policy::RELEASE_TO_GROUP,
                RequestPriority::BackgroundSync,
                move |_| async move {
                    let page = view
                        .search_releases_query(
                            &format!("barcode:{upc}"),
                            SEARCH_LIMIT,
                            Criticality::IdentityCritical,
                        )
                        .await?;
                    let ids: Vec<String> = page.items.iter().map(|hit| hit.id.clone()).collect();
                    Ok(match prove_releases(&view, &album, &ids, false).await? {
                        Some(found) => Fetched::Found(found),
                        None => Fetched::Missing,
                    })
                },
            )
            .await
    }
}

impl AlbumResolver {
    /// Fork: the barcode kept for Spotify album `id`: `Some(None)` when
    /// Spotify was asked and had none, `None` when it hasn't been asked.
    pub async fn kept_barcode(&self, id: &str) -> Option<Option<String>> {
        let bytes = self.cache.get_bytes(&barcode_key(id)).await?;
        let code = String::from_utf8(bytes).ok()?;
        Some(Some(code).filter(|code| !code.is_empty()))
    }

    /// Fork: keep Spotify album `id`'s barcode (empty for none) for a month,
    /// so a playlist sync asks Spotify once per album.
    pub fn keep_barcode(&self, id: &str, code: Option<&str>) {
        self.cache.set_bytes(
            &barcode_key(id),
            code.unwrap_or_default().as_bytes().to_vec(),
            std::time::Duration::from_secs(30 * 24 * 3600),
        );
    }
}

/// Fork: where a Spotify album's barcode is kept.
fn barcode_key(id: &str) -> CacheKey {
    CacheKey::hashed(Source::MusicBrainz, "spotify_album_upc", &[id])
}

impl AlbumResolver {
    /// Fork: the recording and album one song is, looked up by itself
    /// (`spotify_tracks`). `None` when no recording agrees with it.
    ///
    /// # Errors
    ///
    /// A MusicBrainz failure; nothing is cached for it.
    pub async fn resolve_track(
        &self,
        song: &super::spotify_tracks::SpotifySong,
    ) -> Result<Option<super::spotify_tracks::ProvenTrack>, ProviderError> {
        let Some(first) = song.artists.first().filter(|name| !name.trim().is_empty()) else {
            return Ok(None);
        };
        if song.title.trim().is_empty() {
            return Ok(None);
        }
        let Some(settings) = self.settings.read() else {
            return Err(ProviderError::NotConfigured {
                provider: SOURCE,
                message: "the musicbrainz settings cannot be read".to_owned(),
            });
        };
        let namespace = format!("{}:g{}", settings.source_id, settings.generation);
        let view = self.musicbrainz.fixed(
            self.musicbrainz.source_from(&settings),
            RequestPriority::BackgroundSync,
        );
        let seconds = song.seconds.map(|s| s.to_string()).unwrap_or_default();
        let key = CacheKey::hashed(
            Source::MusicBrainz,
            "spotify_track_recording",
            &[
                &namespace,
                &song.title,
                &song.artists.join("\u{1f}"),
                &song.album,
                &seconds,
            ],
        );
        let (song, first) = (song.clone(), first.clone());
        self.cache
            .get_or_fetch(
                &key,
                Policy::RELEASE_TO_GROUP,
                RequestPriority::BackgroundSync,
                move |_| async move {
                    let mut titles = vec![song.title.clone()];
                    let core = crate::landing::want::core_title(&song.title);
                    if !core.is_empty() && !core.eq_ignore_ascii_case(&song.title) {
                        titles.push(core);
                    }
                    let mut hits = Vec::new();
                    for title in titles {
                        let query = droppedneedle_providers::musicbrainz::build_recording_query(
                            &title, &first,
                        );
                        let page = view
                            .search_recordings_query(
                                &query,
                                super::spotify_tracks::TRACK_SEARCH_LIMIT,
                                Criticality::IdentityCritical,
                            )
                            .await?;
                        hits.extend(page.items);
                        if let Some(found) = super::spotify_tracks::choose(&song, &hits) {
                            return Ok(Fetched::Found(found));
                        }
                    }
                    Ok(Fetched::Missing)
                },
            )
            .await
    }
}

/// Search each title form until one proves exactly one release group.
async fn prove(
    view: &MusicBrainzView,
    album: &SpotifyAlbum,
) -> Result<Option<ProvenAlbum>, ProviderError> {
    // Fork: a MusicBrainz release that links this Spotify album is it.
    if !album.spotify_id.is_empty() && album.spotify_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        let resolution = view
            .resolve_url(
                &format!("https://open.spotify.com/album/{}", album.spotify_id),
                &["release-rels"],
                Criticality::IdentityCritical,
            )
            .await?;
        let ids: Vec<String> = resolution
            .relations
            .iter()
            .filter_map(|relation| relation.release.as_ref().map(|release| release.id.clone()))
            .collect();
        if let Some(found) = prove_releases(view, album, &ids, false).await? {
            return Ok(Some(found));
        }
    }
    for title in title_forms(&album.title) {
        let page = view
            .search_releases(
                &title,
                &album.artist,
                SEARCH_LIMIT,
                Criticality::IdentityCritical,
            )
            .await?;
        let mut proven: BTreeSet<String> = BTreeSet::new();
        let mut recordings: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        let mut edition: Option<String> = None;
        for hit in page.items {
            let listed = hit
                .release_group
                .as_ref()
                .map(|group| normalize_mb_id(&group.id));
            if listed.as_ref().is_some_and(|group| proven.contains(group)) {
                continue;
            }
            let Some(release) = view
                .lookup_release(&hit.id, &PROOF_INCLUDES, Criticality::IdentityCritical)
                .await?
            else {
                continue;
            };
            let release = release.entity;
            let Some(group) = release
                .release_group
                .as_ref()
                .map(|group| normalize_mb_id(&group.id))
                .filter(|group| is_valid_mbid(group))
            else {
                continue;
            };
            let mut carries = false;
            for recording in release
                .media
                .iter()
                .flat_map(|medium| &medium.tracks)
                .filter_map(|track| track.recording.as_ref())
            {
                for isrc in &recording.isrcs {
                    let isrc = isrc.trim().to_ascii_uppercase();
                    if album.isrcs.contains(&isrc) {
                        carries = true;
                        recordings
                            .entry(isrc)
                            .or_insert_with(|| normalize_mb_id(&recording.id));
                    }
                }
            }
            if carries {
                if proven.is_empty() {
                    edition = Some(normalize_mb_id(&release.id));
                }
                proven.insert(group);
            }
        }
        match proven.len() {
            0 => {}
            1 => {
                return Ok(proven.pop_first().map(|group| ProvenAlbum {
                    group,
                    recordings,
                    release: edition,
                }));
            }
            _ => {
                tracing::debug!(
                    groups = proven.len(),
                    "spotify album matches more than one release group; left unresolved"
                );
                return Ok(None);
            }
        }
    }
    Ok(None)
}

/// Fork: the one release group the releases `ids` agree on, with the
/// recordings the album's ISRCs name on them and the first release as the
/// edition. The releases are already tied to the album (by its Spotify link
/// or its barcode), so no ISRC has to match unless `need_isrc`. Two groups
/// leave it open.
async fn prove_releases(
    view: &MusicBrainzView,
    album: &SpotifyAlbum,
    ids: &[String],
    need_isrc: bool,
) -> Result<Option<ProvenAlbum>, ProviderError> {
    let mut groups: BTreeSet<String> = BTreeSet::new();
    let mut recordings = std::collections::BTreeMap::new();
    let mut edition = None;
    for id in ids.iter().take(SEARCH_LIMIT as usize) {
        let Some(release) = view
            .lookup_release(id, &PROOF_INCLUDES, Criticality::IdentityCritical)
            .await?
        else {
            continue;
        };
        let release = release.entity;
        let Some(group) = release
            .release_group
            .as_ref()
            .map(|group| normalize_mb_id(&group.id))
            .filter(|group| is_valid_mbid(group))
        else {
            continue;
        };
        let mut carries = false;
        for recording in release
            .media
            .iter()
            .flat_map(|medium| &medium.tracks)
            .filter_map(|track| track.recording.as_ref())
        {
            for isrc in &recording.isrcs {
                let isrc = isrc.trim().to_ascii_uppercase();
                if album.isrcs.contains(&isrc) {
                    carries = true;
                    recordings
                        .entry(isrc)
                        .or_insert_with(|| normalize_mb_id(&recording.id));
                }
            }
        }
        if need_isrc && !carries {
            continue;
        }
        if edition.is_none() {
            edition = Some(normalize_mb_id(&release.id));
        }
        groups.insert(group);
    }
    if groups.len() != 1 {
        return Ok(None);
    }
    Ok(groups.pop_first().map(|group| ProvenAlbum {
        group,
        recordings,
        release: edition,
    }))
}

/// The title as Spotify gives it, then without a trailing edition note
/// (`Abbey Road (Remastered)`, `Glass Tides - Single`), which MusicBrainz
/// keeps out of the title.
fn title_forms(title: &str) -> Vec<String> {
    let title = title.trim();
    let mut forms = vec![title.to_owned()];
    let base = if let Some(stripped) = title
        .strip_suffix(')')
        .and_then(|rest| rest.rfind('(').map(|at| &rest[..at]))
    {
        stripped
    } else if let Some(stripped) = title
        .strip_suffix(']')
        .and_then(|rest| rest.rfind('[').map(|at| &rest[..at]))
    {
        stripped
    } else if let Some(at) = title.rfind(" - ") {
        &title[..at]
    } else {
        title
    };
    let base = base.trim();
    if !base.is_empty() && base != title {
        forms.push(base.to_owned());
    }
    forms
}

/// `CC XXX YY NNNNN`: two letters, three letters or digits, then seven
/// digits.
fn is_isrc(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 12
        && bytes[..2].iter().all(u8::is_ascii_uppercase)
        && bytes[2..5]
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        && bytes[5..].iter().all(u8::is_ascii_digit)
}
