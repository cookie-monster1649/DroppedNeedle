//! A track's identities outside MusicBrainz, as source links.
//!
//! `local_entity_source_links` holds what identifies a library entity with
//! another provider (an ISRC, a Spotify track). An import adopts the links
//! its release gives each track ([`crate::matching::SourceLink`]), in the
//! commit that places the file. A link the track already has stays as it
//! is; nothing here removes one.

use rusqlite::{Connection, params};

use super::sql;
use crate::matching::SourceLink;

/// Adopt `links` as track `track_id`'s source links, automatically decided
/// and verified now. Links it already has are left alone.
pub fn link_track(
    conn: &Connection,
    track_id: &str,
    links: &[SourceLink],
    now: f64,
) -> rusqlite::Result<()> {
    if links.is_empty() {
        return Ok(());
    }
    let now_ms = (now * 1000.0) as i64;
    let mut insert = conn.prepare_cached(sql::ADOPT_TRACK_SOURCE_LINK.text())?;
    for link in links {
        if link.provider.is_empty() || link.external_id.is_empty() || link.canonical_url.is_empty()
        {
            continue;
        }
        insert.execute(params![
            uuid::Uuid::new_v4().to_string(),
            track_id,
            link.provider,
            link.entity_type,
            link.external_id,
            link.canonical_url,
            now_ms
        ])?;
    }
    Ok(())
}
