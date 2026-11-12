//! Fork: metadata sources other than MusicBrainz.
//!
//! The library is built around MusicBrainz ids: identity rows, plans,
//! pages, covers, Subsonic and Jellyfin all carry them. An album confirmed
//! from another source (Deezer today) takes stand-in ids instead
//! (providers `virtual_ids`): MBID-shaped, never sent to any service, and
//! kept in `source_identities` with the source item each names.
//!
//! - [`MetadataSource`] is what a source gives: MusicBrainz-shaped
//!   documents for its stand-in ids, from what the library stored of it.
//!   [`deezer::DeezerSource`] is the one source.
//! - [`LibrarySources`] is attached to the MusicBrainz client, so every
//!   lookup of a stand-in id (album pages, the edition picker,
//!   identification, management) reads the source's data, unchanged
//!   upstream code and all.
//! - [`adopt`] gives a confirmed album its stand-in identity as a
//!   person's choice; [`backfill`](adopt::backfill_some) does it for
//!   albums confirmed before.
//! - [`promote`] looks each such album up at MusicBrainz now and then, and
//!   hands it back to automatic identification when MusicBrainz has a
//!   confident match.

pub mod adopt;
pub mod deezer;
pub mod editions;
pub mod promote;
pub(crate) mod sql;

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension as _, params};

use droppedneedle_kernel::db::Db;
use droppedneedle_providers::virtual_ids::{self, BoxFuture, Kind, VirtualSource};

use crate::jobs::models::OperationError;

/// What an album's last promotion check (or a failed adoption) found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    /// MusicBrainz had a confident match: handed back.
    Promoted,
    /// Nothing confident: the source's identity stays.
    Kept,
    /// No file fits the source's track list: not adopted.
    Unplaceable,
}

droppedneedle_kernel::spelled!(CheckOutcome {
    Promoted => "promoted",
    Kept => "kept",
    Unplaceable => "unplaceable",
});

/// One minted stand-in id and what it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentity {
    pub uuid: String,
    pub provider: String,
    pub kind: Kind,
    pub provider_id: String,
    /// The source album the item was minted from.
    pub provider_album_id: Option<String>,
}

/// A source of metadata outside MusicBrainz.
pub trait MetadataSource: Send + Sync {
    /// The provider key its stand-in ids are minted under.
    fn provider(&self) -> &'static str;

    /// The MusicBrainz document `entity` would have for `identity`, from
    /// what the library stored, or `None` when nothing stored names it.
    ///
    /// # Errors
    ///
    /// A store fault.
    fn document(
        &self,
        conn: &Connection,
        entity: &str,
        identity: &SourceIdentity,
    ) -> rusqlite::Result<Option<serde_json::Value>>;
}

/// Mint `provider`'s stand-in id for its `kind` item `provider_id`, and
/// keep what it names. Minting again finds the same id.
///
/// # Errors
///
/// A store fault.
pub fn mint(
    conn: &Connection,
    provider: &str,
    kind: Kind,
    provider_id: &str,
    provider_album_id: Option<&str>,
    now_ms: i64,
) -> rusqlite::Result<String> {
    let uuid = virtual_ids::mint(provider, kind, provider_id);
    conn.prepare_cached(sql::KEEP_IDENTITY.text())?
        .execute(params![
            uuid,
            provider,
            kind.as_str(),
            provider_id,
            provider_album_id,
            now_ms
        ])?;
    Ok(uuid)
}

/// What stand-in id `uuid` names, if it was minted here.
///
/// # Errors
///
/// A store fault.
pub fn identity_of(conn: &Connection, uuid: &str) -> rusqlite::Result<Option<SourceIdentity>> {
    let row = conn
        .prepare_cached(sql::IDENTITY_OF.text())?
        .query_row([uuid.trim().to_ascii_lowercase()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .optional()?;
    Ok(
        row.and_then(|(uuid, provider, kind, provider_id, provider_album_id)| {
            Some(SourceIdentity {
                uuid,
                provider,
                kind: Kind::parse(&kind)?,
                provider_id,
                provider_album_id,
            })
        }),
    )
}

/// The library's sources, answering the MusicBrainz client's lookups of
/// stand-in ids.
#[derive(Clone)]
pub struct LibrarySources {
    db: Db,
    sources: Vec<Arc<dyn MetadataSource>>,
}

impl LibrarySources {
    /// The sources the library has: Deezer.
    pub fn new(db: Db) -> Self {
        Self {
            db,
            sources: vec![Arc::new(deezer::DeezerSource)],
        }
    }

    /// The sources, for [`Self::document_in`].
    pub fn sources(&self) -> &[Arc<dyn MetadataSource>] {
        &self.sources
    }

    /// The document for `entity` stand-in id `id`, read on a blocking
    /// connection.
    ///
    /// # Errors
    ///
    /// A store fault.
    pub fn document_in(
        sources: &[Arc<dyn MetadataSource>],
        conn: &Connection,
        entity: &str,
        id: &str,
    ) -> rusqlite::Result<Option<serde_json::Value>> {
        let Some(identity) = identity_of(conn, id)? else {
            return Ok(None);
        };
        let Some(source) = sources
            .iter()
            .find(|source| source.provider() == identity.provider)
        else {
            return Ok(None);
        };
        source.document(conn, entity, &identity)
    }
}

impl VirtualSource for LibrarySources {
    fn lookup<'a>(
        &'a self,
        entity: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Option<serde_json::Value>> {
        let sources = self.sources.clone();
        let (entity, id) = (entity.to_owned(), id.to_owned());
        Box::pin(async move {
            let read = self
                .db
                .read("sources.lookup", move |conn| {
                    Ok::<_, OperationError>(Self::document_in(&sources, conn, &entity, &id)?)
                })
                .await;
            read.unwrap_or_else(|error| {
                tracing::warn!(%error, "stand-in id could not be read");
                None
            })
        })
    }
}
