//! Fork: covers for albums on stand-in ids.
//!
//! The Cover Art Archive knows nothing under a stand-in id (and the
//! providers refuse to ask it). An album on stand-in ids with no art of
//! its own takes its source's cover instead: the URL stored on its
//! confirmed match, fetched through the image client and its host list.
//! Every other id goes to the archive as before.

use std::sync::Arc;

use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_providers::coverart::{ArtworkBytes, DownloadSize, EntityKind};
use droppedneedle_providers::virtual_ids::is_virtual;
use droppedneedle_providers::{ProviderError, RequestPriority};
use futures_util::future::BoxFuture;
use rusqlite::{OptionalExtension, params};

use super::remote::{ImageFetch, RemoteCovers, RemoteImages};

pub(crate) mod sql;

/// A cover that could not be had now; worth asking again later.
const UNAVAILABLE: ProviderError = ProviderError::Unavailable {
    provider: "deezer",
    retry_after: None,
};

/// The archive for real ids, the source's cover for stand-in ones.
pub struct SourceCovers {
    archive: Arc<dyn RemoteCovers>,
    images: Arc<dyn RemoteImages>,
    db: Db,
}

impl SourceCovers {
    /// Covers from `archive`, and for stand-in ids the URL on the match in
    /// `db` fetched through `images`.
    pub fn new(archive: Arc<dyn RemoteCovers>, images: Arc<dyn RemoteImages>, db: Db) -> Self {
        Self {
            archive,
            images,
            db,
        }
    }

    async fn source_url(&self, id: &str) -> Result<Option<String>, ProviderError> {
        let id = id.trim().to_lowercase();
        self.db
            .read("artwork.source_cover_url", move |conn| {
                conn.prepare_cached(sql::SOURCE_COVER_URL.text())?
                    .query_row(params![id], |row| row.get::<_, String>(0))
                    .optional()
                    .map_err(DbError::from)
            })
            .await
            .map_err(|error| {
                tracing::warn!(%error, "source cover URL unreadable");
                UNAVAILABLE
            })
    }

    async fn source_front(
        &self,
        id: &str,
        priority: RequestPriority,
    ) -> Result<Option<ArtworkBytes>, ProviderError> {
        let Some(url) = self.source_url(id).await? else {
            return Ok(None);
        };
        match self.images.fetch(&url, priority).await {
            ImageFetch::Found(bytes) => Ok(Some(bytes)),
            ImageFetch::Unusable => Ok(None),
            ImageFetch::Failed => Err(UNAVAILABLE),
        }
    }
}

impl RemoteCovers for SourceCovers {
    fn front<'a>(
        &'a self,
        entity: EntityKind,
        mbid: &'a str,
        size: DownloadSize,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<ArtworkBytes>, ProviderError>> {
        if !is_virtual(mbid) {
            return self.archive.front(entity, mbid, size, priority);
        }
        Box::pin(async move {
            // Release and release group alike: the source has one cover.
            let _ = entity;
            self.source_front(mbid, priority).await
        })
    }
}
