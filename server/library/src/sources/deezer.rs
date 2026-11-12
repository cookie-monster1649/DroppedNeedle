//! Fork: Deezer as a metadata source.
//!
//! An album's Deezer match (`library_album_provider_matches`) holds what
//! the album page, the edition picker and management need: title, artist,
//! date, label, UPC, record type, genres and the track list with ISRCs.
//! This turns it into the MusicBrainz documents its stand-in ids name.
//! Each recording carries its Deezer track, album and artist pages as URL
//! relationships, the way MusicBrainz itself links Deezer, so the release
//! the library reads keeps the Deezer ids for the `DEEZER_*` tags.

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Value, json};

use droppedneedle_providers::virtual_ids::{self, Kind, is_virtual};

use super::{MetadataSource, SourceIdentity, sql};
use crate::provider_match::{DEEZER, MatchTrack, ProviderMatch, deezer_url, match_of};

/// MusicBrainz's "free streaming" URL relationship type.
const STREAMING_TYPE: &str = "free streaming";
const STREAMING_TYPE_ID: &str = "08445ccf-7b99-4438-9f9a-fb9ac18099ee";

/// Deezer.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeezerSource;

/// The stand-in ids of one Deezer album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumIds {
    pub release_group: String,
    pub release: String,
    /// The album artist: their MusicBrainz id when the library has one,
    /// else a stand-in.
    pub artist: Option<String>,
}

impl AlbumIds {
    /// The ids for `found` (no store read: the artist is a stand-in).
    pub fn of(found: &ProviderMatch) -> Self {
        Self {
            release_group: virtual_ids::mint(DEEZER, Kind::ReleaseGroup, &found.provider_album_id),
            release: virtual_ids::mint(DEEZER, Kind::Release, &found.provider_album_id),
            artist: found
                .provider_artist_id
                .as_deref()
                .map(|id| virtual_ids::mint(DEEZER, Kind::Artist, id)),
        }
    }

    /// The ids for `found`, with the album artist's MusicBrainz id when
    /// the library holds one.
    ///
    /// # Errors
    ///
    /// A store fault.
    pub fn read(conn: &Connection, found: &ProviderMatch) -> rusqlite::Result<Self> {
        let mut ids = Self::of(found);
        let real: Option<String> = conn
            .prepare_cached(sql::ALBUM_ARTIST_MBID.text())?
            .query_row([&found.album_id], |row| row.get(0))
            .optional()?;
        if let Some(real) = real.filter(|id| !id.trim().is_empty()) {
            ids.artist = Some(real.to_ascii_lowercase());
        }
        Ok(ids)
    }
}

/// The stand-in release track of Deezer track `track_id`.
pub fn release_track_id(track_id: &str) -> String {
    virtual_ids::mint(DEEZER, Kind::ReleaseTrack, track_id)
}

/// The stand-in recording of Deezer track `track_id`.
pub fn recording_id(track_id: &str) -> String {
    virtual_ids::mint(DEEZER, Kind::Recording, track_id)
}

impl MetadataSource for DeezerSource {
    fn provider(&self) -> &'static str {
        DEEZER
    }

    fn document(
        &self,
        conn: &Connection,
        entity: &str,
        identity: &SourceIdentity,
    ) -> rusqlite::Result<Option<Value>> {
        let Some(album) = identity.provider_album_id.as_deref() else {
            return Ok(None);
        };
        let Some(found) = stored_match(conn, album)? else {
            return Ok(None);
        };
        let ids = AlbumIds::read(conn, &found)?;
        Ok(match (entity, identity.kind) {
            ("release-group", Kind::ReleaseGroup) => Some(release_group(&found, &ids)),
            ("release", Kind::Release) => Some(release(&found, &ids)),
            ("recording", Kind::Recording) => found
                .tracks
                .iter()
                .find(|track| track.provider_track_id == identity.provider_id)
                .map(|track| recording(&found, &ids, track)),
            ("artist", Kind::Artist) => Some(artist(&found, &identity.uuid)),
            _ => None,
        })
    }
}

/// The stored match of Deezer album `album`.
fn stored_match(conn: &Connection, album: &str) -> rusqlite::Result<Option<ProviderMatch>> {
    let local: Option<String> = conn
        .prepare_cached(sql::ALBUM_OF_MATCH.text())?
        .query_row(params![DEEZER, album], |row| row.get(0))
        .optional()?;
    match local {
        Some(local) => match_of(conn, &local),
        None => Ok(None),
    }
}

/// Deezer's date, unless it is blank or all zeros.
fn date(found: &ProviderMatch) -> Option<String> {
    found
        .release_date
        .clone()
        .filter(|date| !date.is_empty() && !date.starts_with("0000"))
}

/// The MusicBrainz primary and secondary types of Deezer's record type.
fn types(found: &ProviderMatch) -> (&'static str, Vec<&'static str>) {
    match found
        .record_type
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("ep") => ("EP", Vec::new()),
        Some("single") => ("Single", Vec::new()),
        Some("compile") => ("Album", vec!["Compilation"]),
        _ => ("Album", Vec::new()),
    }
}

fn credit(found: &ProviderMatch, ids: &AlbumIds) -> Value {
    match &ids.artist {
        Some(id) => json!([{
            "name": found.artist_name,
            "joinphrase": "",
            "artist": {"id": id, "name": found.artist_name, "sort-name": found.artist_name},
        }]),
        None => json!([]),
    }
}

fn group_ref(found: &ProviderMatch, ids: &AlbumIds) -> Value {
    let (primary, secondary) = types(found);
    json!({
        "id": ids.release_group,
        "title": found.title,
        "first-release-date": date(found),
        "primary-type": primary,
        "secondary-types": secondary,
        "artist-credit": credit(found, ids),
    })
}

fn genres(found: &ProviderMatch) -> Value {
    Value::Array(
        found
            .genres
            .iter()
            .map(|name| json!({"name": name.to_lowercase(), "count": 1}))
            .collect(),
    )
}

/// The release group document.
pub fn release_group(found: &ProviderMatch, ids: &AlbumIds) -> Value {
    let mut group = group_ref(found, ids);
    group["releases"] = json!([{
        "id": ids.release,
        "title": found.title,
        "status": "Official",
        "date": date(found),
        "barcode": found.upc,
        "artist-credit": credit(found, ids),
        "media": media_summary(&found.tracks),
    }]);
    group
}

/// Discs in order, each with its tracks in order.
fn discs(tracks: &[MatchTrack]) -> Vec<(i64, Vec<&MatchTrack>)> {
    let mut discs: Vec<(i64, Vec<&MatchTrack>)> = Vec::new();
    for track in tracks {
        let disc = track.disc.max(1);
        match discs.iter_mut().find(|(at, _)| *at == disc) {
            Some((_, list)) => list.push(track),
            None => discs.push((disc, vec![track])),
        }
    }
    discs.sort_by_key(|(disc, _)| *disc);
    for (_, list) in &mut discs {
        list.sort_by_key(|track| track.position);
    }
    discs
}

fn media_summary(tracks: &[MatchTrack]) -> Value {
    Value::Array(
        discs(tracks)
            .into_iter()
            .map(|(disc, list)| json!({"position": disc, "format": "Digital Media", "track-count": list.len()}))
            .collect(),
    )
}

fn length_ms(track: &MatchTrack) -> Option<u64> {
    track
        .duration_seconds
        .filter(|seconds| *seconds > 0.0)
        .map(|seconds| (seconds * 1000.0).round() as u64)
}

/// Deezer's pages for the track, its album and its artist, as MusicBrainz
/// URL relationships.
fn links(found: &ProviderMatch, track: &MatchTrack) -> Value {
    let mut urls = vec![
        deezer_url("track", &track.provider_track_id),
        deezer_url("album", &found.provider_album_id),
    ];
    if let Some(artist) = &found.provider_artist_id {
        urls.push(deezer_url("artist", artist));
    }
    Value::Array(
        urls.into_iter()
            .map(|resource| {
                json!({
                    "type": STREAMING_TYPE,
                    "type-id": STREAMING_TYPE_ID,
                    "target-type": "url",
                    "url": {"resource": resource},
                })
            })
            .collect(),
    )
}

fn recording_ref(found: &ProviderMatch, ids: &AlbumIds, track: &MatchTrack) -> Value {
    json!({
        "id": recording_id(&track.provider_track_id),
        "title": track.title,
        "length": length_ms(track),
        "isrcs": track.isrc.iter().map(|isrc| isrc.to_ascii_uppercase()).collect::<Vec<_>>(),
        "artist-credit": credit(found, ids),
        "relations": links(found, track),
    })
}

/// The release document.
pub fn release(found: &ProviderMatch, ids: &AlbumIds) -> Value {
    let media: Vec<Value> = discs(&found.tracks)
        .into_iter()
        .map(|(disc, list)| {
            let tracks: Vec<Value> = list
                .iter()
                .enumerate()
                .map(|(index, track)| {
                    let position = if track.position > 0 {
                        track.position as usize
                    } else {
                        index + 1
                    };
                    json!({
                        "id": release_track_id(&track.provider_track_id),
                        "position": position,
                        "number": position.to_string(),
                        "title": track.title,
                        "length": length_ms(track),
                        "recording": recording_ref(found, ids, track),
                    })
                })
                .collect();
            json!({
                "position": disc,
                "format": "Digital Media",
                "track-count": tracks.len(),
                "tracks": tracks,
            })
        })
        .collect();
    let labels: Vec<Value> = found
        .label
        .iter()
        .filter(|label| !label.trim().is_empty())
        .map(|label| {
            let id = virtual_ids::mint(DEEZER, Kind::Label, label.trim());
            json!({"catalog-number": null, "label": {"id": id, "name": label.trim()}})
        })
        .collect();
    json!({
        "id": ids.release,
        "title": found.title,
        "status": "Official",
        "date": date(found),
        "barcode": found.upc,
        "artist-credit": credit(found, ids),
        "label-info": labels,
        "media": media,
        "release-group": group_ref(found, ids),
        "genres": genres(found),
    })
}

/// A recording document.
pub fn recording(found: &ProviderMatch, ids: &AlbumIds, track: &MatchTrack) -> Value {
    let mut doc = recording_ref(found, ids, track);
    doc["releases"] = json!([{
        "id": ids.release,
        "status": "Official",
        "date": date(found),
        "release-group": group_ref(found, ids),
    }]);
    doc
}

/// An artist document: the name Deezer credits.
pub fn artist(found: &ProviderMatch, uuid: &str) -> Value {
    json!({
        "id": uuid,
        "name": found.artist_name,
        "sort-name": found.artist_name,
    })
}

/// The Deezer id in one of Deezer's `kind` pages (`https://www.deezer.com/<kind>/<id>`).
pub fn id_in_url<'a>(url: &'a str, kind: &str) -> Option<&'a str> {
    let rest = url.split("deezer.com/").nth(1)?;
    let rest = rest
        .split_once('/')
        .filter(|(first, _)| first.len() == 2 && *first != kind)
        .map_or(rest, |(_, after)| after);
    let id = rest.strip_prefix(kind)?.strip_prefix('/')?;
    let id = id.split(['?', '#', '/']).next()?;
    (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())).then_some(id)
}

/// The Deezer track, album and artist of stand-in release track
/// `release_track` (from its recording's URL relationships), as source
/// links. Empty for a MusicBrainz track, so its document is unchanged.
pub fn track_links(
    release_track: &str,
    recording: &droppedneedle_providers::musicbrainz::models::RecordingRef,
) -> Vec<crate::matching::SourceLink> {
    if !is_virtual(release_track) {
        return Vec::new();
    }
    let mut links = Vec::new();
    for relation in recording.relations.iter().flatten() {
        let Some(url) = relation.url.as_ref().map(|url| url.resource.as_str()) else {
            continue;
        };
        for kind in ["track", "album", "artist"] {
            if let Some(id) = id_in_url(url, kind) {
                links.push(crate::matching::SourceLink {
                    provider: DEEZER.to_owned(),
                    entity_type: kind.to_owned(),
                    external_id: id.to_owned(),
                    canonical_url: deezer_url(kind, id),
                });
            }
        }
    }
    links
}

/// True when `id` names nothing at MusicBrainz.
pub fn stand_in(id: &str) -> bool {
    is_virtual(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_match::ProviderMatchState;
    use droppedneedle_providers::musicbrainz::models::{MbRecording, MbRelease, MbReleaseGroup};

    fn found() -> ProviderMatch {
        ProviderMatch {
            album_id: "local".into(),
            provider: DEEZER.into(),
            provider_album_id: "359557537".into(),
            title: "FEMME".into(),
            artist_name: "Poppy Ajudha".into(),
            provider_artist_id: Some("4050205".into()),
            artist_picture_url: None,
            release_date: Some("2022-09-30".into()),
            label: Some("Virgin".into()),
            upc: Some("00602445".into()),
            record_type: Some("ep".into()),
            cover_url: None,
            link_url: None,
            genres: vec!["Pop".into()],
            tracks: vec![
                MatchTrack {
                    provider_track_id: "2".into(),
                    title: "Spread a Little Love".into(),
                    duration_seconds: Some(219.0),
                    disc: 1,
                    position: 2,
                    isrc: Some("gbum72205502".into()),
                },
                MatchTrack {
                    provider_track_id: "1".into(),
                    title: "Femme".into(),
                    duration_seconds: Some(201.0),
                    disc: 1,
                    position: 1,
                    isrc: None,
                },
            ],
            score: 0.9,
            state: ProviderMatchState::Confirmed,
        }
    }

    #[test]
    fn the_documents_read_as_musicbrainz_entities() {
        let found = found();
        let ids = AlbumIds::of(&found);
        let release: MbRelease = serde_json::from_value(release(&found, &ids)).unwrap();
        assert_eq!(release.id, ids.release);
        assert!(stand_in(&release.id));
        let group = release.release_group.as_ref().unwrap();
        assert_eq!(group.id, ids.release_group);
        assert_eq!(group.primary_type.as_deref(), Some("EP"));
        let tracks = &release.media[0].tracks;
        assert_eq!(tracks[0].title.as_deref(), Some("Femme"));
        assert_eq!(tracks[1].length, Some(219_000));
        let recording = tracks[1].recording.as_ref().unwrap();
        assert_eq!(recording.isrcs, vec!["GBUM72205502".to_owned()]);
        assert!(stand_in(&recording.id));
        let group: MbReleaseGroup = serde_json::from_value(release_group(&found, &ids)).unwrap();
        assert_eq!(group.releases[0].id, ids.release);
        let recording: MbRecording =
            serde_json::from_value(super::recording(&found, &ids, &found.tracks[1])).unwrap();
        assert_eq!(recording.releases[0].id, ids.release);
    }

    #[test]
    fn deezer_ids_come_out_of_their_pages() {
        assert_eq!(
            id_in_url("https://www.deezer.com/track/123", "track"),
            Some("123")
        );
        assert_eq!(
            id_in_url("https://www.deezer.com/en/album/45?x=1", "album"),
            Some("45")
        );
        assert_eq!(id_in_url("https://www.deezer.com/artist/7", "album"), None);
    }
}
