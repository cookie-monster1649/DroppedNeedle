//! What identifies a want outside MusicBrainz (fork, catalogue extension 1).
//!
//! A want ([`super::ledger::want_key`]) is asked for by name; when the ask
//! also knows the track's ISRC or Spotify id (a Spotify playlist row does),
//! they are kept in `acquire_want_identities` beside it. When the want's
//! files are placed they become the placed track's provider links
//! (`local_entity_source_links`), and the ISRC is written to its tags.
//!
//! The provider set grows by one variant and one CHECK value in a later
//! catalogue extension file; nothing else here assumes two.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Who issued a want identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WantIdentityProvider {
    /// An ISRC: the recording's international code, issuer-independent.
    Isrc,
    /// A Spotify track id.
    Spotify,
}

droppedneedle_kernel::spelled!(WantIdentityProvider {
    Isrc => "isrc",
    Spotify => "spotify",
});

impl WantIdentityProvider {
    /// What kind of thing the id names, as `local_entity_source_links`
    /// spells it.
    pub fn entity_type(self) -> &'static str {
        match self {
            Self::Isrc => "recording",
            Self::Spotify => "track",
        }
    }

    /// A page for the id, as `local_entity_source_links` requires one.
    pub fn canonical_url(self, id: &str) -> String {
        match self {
            Self::Isrc => format!("https://musicbrainz.org/isrc/{id}"),
            Self::Spotify => format!("https://open.spotify.com/track/{id}"),
        }
    }

    /// The id in its stored form, or `None` when `raw` is not one.
    pub fn normalize(self, raw: &str) -> Option<String> {
        match self {
            Self::Isrc => normalize_isrc(raw),
            Self::Spotify => normalize_spotify_track(raw),
        }
    }
}

/// One identity of a want.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WantIdentity {
    /// Who issued it.
    pub provider: WantIdentityProvider,
    /// The id, normalized.
    pub external_id: String,
}

/// An ISRC in its stored form: twelve characters, upper case, no hyphens or
/// spaces (`CC-XXX-YY-NNNNN` → `CCXXXYYNNNNN`): a two-letter country code,
/// a three-character registrant, two year digits and five more digits.
pub fn normalize_isrc(raw: &str) -> Option<String> {
    let code: String = raw
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .collect::<String>()
        .to_ascii_uppercase();
    let bytes = code.as_bytes();
    let shaped = bytes.len() == 12
        && bytes[..2].iter().all(u8::is_ascii_alphabetic)
        && bytes[2..5].iter().all(u8::is_ascii_alphanumeric)
        && bytes[5..].iter().all(u8::is_ascii_digit);
    shaped.then_some(code)
}

/// A Spotify track id from the id itself, a `spotify:track:` URI or an
/// `open.spotify.com/track/` link: 22 base-62 characters.
pub fn normalize_spotify_track(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let id = if let Some(rest) = raw.strip_prefix("spotify:track:") {
        rest
    } else if let Some(at) = raw.find("open.spotify.com/") {
        let path = &raw[at + "open.spotify.com/".len()..];
        let path = path.split(['?', '#']).next().unwrap_or_default();
        // Localised links put a locale segment first (`intl-de/track/…`).
        let mut parts = path.split('/').filter(|part| !part.is_empty());
        loop {
            match parts.next() {
                Some("track") => break parts.next().unwrap_or_default(),
                Some(_) => continue,
                None => return None,
            }
        }
    } else {
        raw
    };
    (id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isrcs_are_stored_upper_case_without_separators() {
        assert_eq!(
            normalize_isrc("us-sm1-08-12345").as_deref(),
            Some("USSM10812345")
        );
        assert_eq!(
            normalize_isrc("GBAYE0601498").as_deref(),
            Some("GBAYE0601498")
        );
        assert_eq!(normalize_isrc("GBAYE060149"), None, "too short");
        assert_eq!(
            normalize_isrc("1BAYE0601498"),
            None,
            "country code is letters"
        );
        assert_eq!(normalize_isrc("GBAYE06014A8"), None, "the tail is digits");
    }

    #[test]
    fn spotify_ids_come_from_ids_uris_and_links() {
        let id = "4uLU6hMCjMI75M1A2tKUQC";
        for raw in [
            id.to_owned(),
            format!("spotify:track:{id}"),
            format!("https://open.spotify.com/track/{id}?si=abc"),
            format!("https://open.spotify.com/intl-de/track/{id}"),
        ] {
            assert_eq!(normalize_spotify_track(&raw).as_deref(), Some(id), "{raw}");
        }
        assert_eq!(
            normalize_spotify_track("https://open.spotify.com/album/x"),
            None
        );
        assert_eq!(normalize_spotify_track("short"), None);
    }

    #[test]
    fn providers_spell_what_the_schema_checks() {
        assert_eq!(WantIdentityProvider::ALL.len(), 2);
        assert_eq!(WantIdentityProvider::Isrc.entity_type(), "recording");
        assert_eq!(
            WantIdentityProvider::Spotify.canonical_url("x"),
            "https://open.spotify.com/track/x"
        );
    }
}
