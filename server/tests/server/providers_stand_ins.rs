//! Fork: stand-in ids (providers `virtual_ids`) never leave the process.
//! MusicBrainz lookups of one are answered by the attached source or read
//! as missing; browses find nothing; the Cover Art Archive, ListenBrainz
//! and Last.fm send nothing that carries one. Every service here sits at
//! a scripted address that answers anything, so a request that got out
//! would show in what the wire saw.

use std::sync::Arc;

use droppedneedle_kernel::clock::SystemClock;
use droppedneedle_providers::executor::Method;
use droppedneedle_providers::musicbrainz::{self, Criticality, MbSource};
use droppedneedle_providers::test_support::{Scripted, ScriptedWire};
use droppedneedle_providers::virtual_ids::{self, BoxFuture, Kind, VirtualSource};
use droppedneedle_providers::{
    ProviderEndpoints, ProviderError, Providers, RequestPriority, coverart, lastfm, listenbrainz,
};

const LB_BASE: &str = "https://api.listenbrainz.org";

fn ok() -> Scripted {
    Scripted::Answer {
        status: 200,
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: b"{}".to_vec(),
    }
}

/// Providers whose every service answers 200 to anything asked.
fn open_providers() -> (Arc<ScriptedWire>, Providers) {
    let wire = Arc::new(ScriptedWire::new());
    for base in [
        musicbrainz::MB_API_BASE,
        coverart::COVER_ART_ARCHIVE_BASE,
        LB_BASE,
        lastfm::DEFAULT_BASE_URL,
    ] {
        wire.on(Method::Get, base, ok());
        wire.on(Method::Post, base, ok());
    }
    let providers = Providers::over_wire(
        wire.clone(),
        Arc::new(SystemClock),
        ProviderEndpoints {
            musicbrainz: musicbrainz::MB_API_BASE.to_owned(),
            coverart: coverart::COVER_ART_ARCHIVE_BASE.to_owned(),
            listenbrainz: LB_BASE.to_owned(),
            lastfm: lastfm::DEFAULT_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    );
    (wire, providers)
}

fn view(providers: &Providers) -> musicbrainz::MusicBrainzView {
    providers.musicbrainz.fixed(
        MbSource::Official {
            base_url: musicbrainz::MB_API_BASE.to_owned(),
        },
        RequestPriority::UserInitiated,
    )
}

/// Answers every release lookup with a one-track release.
struct OneRelease;

impl VirtualSource for OneRelease {
    fn lookup<'a>(
        &'a self,
        entity: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Option<serde_json::Value>> {
        Box::pin(async move {
            (entity == "release").then(|| {
                serde_json::json!({
                    "id": id,
                    "title": "FEMME",
                    "release-group": {"id": virtual_ids::mint("deezer", Kind::ReleaseGroup, "1")},
                    "media": [{"position": 1, "tracks": [{
                        "id": virtual_ids::mint("deezer", Kind::ReleaseTrack, "2"),
                        "position": 1,
                        "recording": {"id": virtual_ids::mint("deezer", Kind::Recording, "2")},
                    }]}],
                })
            })
        })
    }
}

#[tokio::test]
async fn musicbrainz_never_hears_a_stand_in_id() {
    let (wire, providers) = open_providers();
    let client = view(&providers);
    let release = virtual_ids::mint("deezer", Kind::Release, "1");
    let group = virtual_ids::mint("deezer", Kind::ReleaseGroup, "1");
    let artist = virtual_ids::mint("deezer", Kind::Artist, "7");

    // No source attached: missing, and nothing sent.
    let found = client
        .lookup_release(&release, &["recordings"], Criticality::IdentityCritical)
        .await
        .expect("no error");
    assert!(found.is_none());
    let found = client
        .lookup_release_group(&group, &[], Criticality::BestEffort)
        .await
        .expect("no error");
    assert!(found.is_none());
    let page = client
        .browse_artist_release_groups(&artist, 25, 0, Criticality::BestEffort)
        .await
        .expect("no error");
    assert!(page.items.is_empty());

    // Attached: the source answers, still nothing sent.
    providers
        .musicbrainz
        .attach_virtual_source(Arc::new(OneRelease));
    let found = client
        .lookup_release(&release, &["recordings"], Criticality::IdentityCritical)
        .await
        .expect("no error")
        .expect("the source answers");
    assert_eq!(found.entity.id, release);
    assert_eq!(found.entity.title.as_deref(), Some("FEMME"));
    assert_eq!(found.entity.media[0].tracks.len(), 1);
    let exact = client
        .lookup_exact_release(&release.to_ascii_uppercase(), Criticality::IdentityCritical)
        .await
        .expect("no error")
        .expect("the source answers");
    assert_eq!(exact.entity.release_group.unwrap().id, group);
    // A kind the source has nothing for is missing.
    assert!(
        client
            .lookup_artist(&artist, &[], Criticality::BestEffort)
            .await
            .expect("no error")
            .is_none()
    );

    assert!(wire.sent().is_empty(), "sent: {:?}", wire.sent());
}

#[tokio::test]
async fn no_other_service_hears_a_stand_in_id() {
    let (wire, providers) = open_providers();
    let group = virtual_ids::mint("deezer", Kind::ReleaseGroup, "1");
    let recording = virtual_ids::mint("deezer", Kind::Recording, "2");

    let cover = providers
        .coverart
        .fetch_front(
            coverart::EntityKind::ReleaseGroup,
            &group,
            coverart::DownloadSize::Size500,
            1 << 20,
            RequestPriority::UserInitiated,
        )
        .await;
    assert!(
        matches!(cover, Err(ProviderError::NotFound { .. })),
        "{cover:?}"
    );

    let listens = serde_json::json!({
        "listen_type": "single",
        "payload": [{"track_metadata": {"artist_name": "A", "track_name": "T",
            "additional_info": {"recording_mbid": recording}}}],
    });
    let submitted = providers
        .listenbrainz
        .submit_listens(
            "token",
            listens.to_string().into_bytes(),
            RequestPriority::BackgroundSync,
        )
        .await;
    assert!(
        matches!(submitted, Err(ProviderError::NotFound { .. })),
        "{submitted:?}"
    );
    let groups = providers
        .listenbrainz
        .recording_release_groups(
            std::slice::from_ref(&recording),
            &listenbrainz::ListenBrainzCredentials::default(),
            RequestPriority::BackgroundSync,
        )
        .await;
    assert!(groups.is_err() || groups.is_ok_and(|found| found.is_empty()));

    let creds = lastfm::LastFmCredentials {
        api_key: "key".to_owned(),
        shared_secret: "secret".to_owned(),
        username: None,
        session_key: None,
    };
    let info = providers
        .lastfm
        .album_info(
            &creds,
            "A",
            "B",
            Some(&group),
            RequestPriority::BackgroundSync,
        )
        .await;
    assert!(info.is_err() || info.is_ok_and(|found| found.is_none()));

    let carried: Vec<String> = wire
        .sent()
        .iter()
        .map(|sent| sent.url.to_string())
        .filter(|url| virtual_ids::mentions_virtual(url))
        .collect();
    assert!(carried.is_empty(), "sent a stand-in id: {carried:?}");
    assert!(wire.sent().is_empty(), "sent: {:?}", wire.sent());
}
