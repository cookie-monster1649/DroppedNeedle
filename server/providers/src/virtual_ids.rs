//! Fork: stand-in ids for albums, releases, recordings and artists that
//! come from a metadata source other than MusicBrainz (Deezer today).
//!
//! A stand-in id is shaped like an MBID, so it flows through every column,
//! plan, route and cache that holds one, but it can never be a real one:
//! it is a version-8 UUID whose first 48 bits are the fixed marker
//! `d0ab1e00-50c3`. MusicBrainz only mints version 1, 3 and 4 ids, so no
//! MBID can carry it. The rest of the id is a SHA-256 of
//! `provider:kind:provider_id`, so the same source item always gets the
//! same id, on any machine and after any rebuild.
//!
//! Two guarantees live here:
//!
//! - [`VirtualSlot`] lets the library answer MusicBrainz lookups of a
//!   stand-in id from the source's own data (see `MusicBrainzView`).
//! - [`mentions_virtual`] is checked by the executor before any request
//!   leaves the process, so a stand-in id never reaches MusicBrainz,
//!   the Cover Art Archive, ListenBrainz, Last.fm or any other service.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use sha2::{Digest as _, Sha256};

/// The fixed start of every stand-in id, lowercase.
pub const MARKER: &str = "d0ab1e00-50c3-8";

const MARKER_BYTES: [u8; 6] = [0xd0, 0xab, 0x1e, 0x00, 0x50, 0xc3];

/// What a stand-in id names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A release group (an album as a work).
    ReleaseGroup,
    /// A release (one edition).
    Release,
    /// One track of a release.
    ReleaseTrack,
    /// A recording.
    Recording,
    /// An artist.
    Artist,
    /// A label, named by its name.
    Label,
}

impl Kind {
    /// Every kind, in the order the table's check lists them.
    pub const ALL: [Kind; 6] = [
        Self::ReleaseGroup,
        Self::Release,
        Self::ReleaseTrack,
        Self::Recording,
        Self::Artist,
        Self::Label,
    ];

    /// The stored name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReleaseGroup => "release_group",
            Self::Release => "release",
            Self::ReleaseTrack => "release_track",
            Self::Recording => "recording",
            Self::Artist => "artist",
            Self::Label => "label",
        }
    }

    /// The kind a stored name names.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "release_group" => Self::ReleaseGroup,
            "release" => Self::Release,
            "release_track" => Self::ReleaseTrack,
            "recording" => Self::Recording,
            "artist" => Self::Artist,
            "label" => Self::Label,
            _ => return None,
        })
    }
}

/// The stand-in id for `provider`'s `kind` item `provider_id`.
#[must_use]
pub fn mint(provider: &str, kind: Kind, provider_id: &str) -> String {
    let digest = Sha256::digest(format!("{provider}:{}:{provider_id}", kind.as_str()).as_bytes());
    let mut bytes = [0u8; 16];
    bytes[..6].copy_from_slice(&MARKER_BYTES);
    bytes[6..].copy_from_slice(&digest[..10]);
    // Version 8 (custom), RFC 4122 variant.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// True when `id` is a stand-in id. Pure: no table is read.
#[must_use]
pub fn is_virtual(id: &str) -> bool {
    let id = id.trim();
    id.len() == 36
        && id
            .get(..MARKER.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(MARKER))
        && crate::musicbrainz::is_valid_mbid(id)
}

/// True when `text` (a URL, a query or a request body) holds a stand-in id
/// anywhere.
#[must_use]
pub fn mentions_virtual(text: &str) -> bool {
    text.to_ascii_lowercase().contains(MARKER)
}

/// `id` unless it is a stand-in id: what may be sent to a service.
#[must_use]
pub fn real_only(id: Option<&str>) -> Option<&str> {
    id.filter(|id| !is_virtual(id))
}

/// A boxed future, so the trait stays object safe without a macro.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Answers MusicBrainz-shaped reads of stand-in ids from a source's data.
/// Implemented by the library over its stored source matches.
pub trait VirtualSource: Send + Sync {
    /// The MusicBrainz JSON document `entity` (`release-group`, `release`,
    /// `recording` or `artist`) would have for stand-in id `id`, or `None`
    /// when the id names nothing known.
    fn lookup<'a>(
        &'a self,
        entity: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Option<serde_json::Value>>;
}

/// Where the library's [`VirtualSource`] is attached, shared by every
/// clone of a client. Empty until attached: a stand-in id then reads as
/// missing, never as a request.
#[derive(Clone, Default)]
pub struct VirtualSlot(Arc<OnceLock<Arc<dyn VirtualSource>>>);

impl std::fmt::Debug for VirtualSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualSlot")
            .field("attached", &self.0.get().is_some())
            .finish()
    }
}

impl VirtualSlot {
    /// Attach `source`. The first attachment stands.
    pub fn attach(&self, source: Arc<dyn VirtualSource>) {
        let _ = self.0.set(source);
    }

    /// The attached source, if any.
    #[must_use]
    pub fn get(&self) -> Option<&Arc<dyn VirtualSource>> {
        self.0.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_id_is_stable_detectable_and_never_an_mbid_version() {
        let id = mint("deezer", Kind::ReleaseGroup, "359557537");
        assert_eq!(id, mint("deezer", Kind::ReleaseGroup, "359557537"));
        assert_ne!(id, mint("deezer", Kind::Release, "359557537"));
        assert!(id.starts_with(MARKER), "{id}");
        assert!(is_virtual(&id));
        assert!(is_virtual(&id.to_ascii_uppercase()));
        assert!(crate::musicbrainz::is_valid_mbid(&id));
        let variant = id.as_bytes()[19];
        assert!(matches!(variant, b'8' | b'9' | b'a' | b'b'), "{id}");
    }

    #[test]
    fn real_and_local_ids_are_not_stand_ins() {
        for id in [
            "32f2a529-448a-3868-963d-7b42b003b9cd",
            "0f8ce4c1-f224-37bc-8a2c-8c930082aebd",
            "3df4a122-3d0a-8dbe-a6c5-cc2b8ddeb657",
            "00000000-0000-4000-8000-000000000001",
            "",
            "not-an-id",
        ] {
            assert!(!is_virtual(id), "{id}");
        }
        assert_eq!(
            real_only(Some("32f2a529-448a-3868-963d-7b42b003b9cd")),
            Some("32f2a529-448a-3868-963d-7b42b003b9cd")
        );
        let stand_in = mint("deezer", Kind::Recording, "1");
        assert_eq!(real_only(Some(&stand_in)), None);
        assert!(mentions_virtual(&format!(
            "https://x/ws/2/release/{stand_in}?inc=a"
        )));
    }
}
