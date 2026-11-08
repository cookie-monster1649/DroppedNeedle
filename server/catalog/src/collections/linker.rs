//! Fork: the playlist linker loop.
//!
//! An entry shows as playable once it points at a library track. v3 links
//! an entry only when a person adds a library track by hand, so a download
//! that lands never reaches the playlists that asked for it. The loop
//! links every entry the library can now play
//! ([`super::store::PlaylistStore::link_library`]),
//! polling rather than hooking each import, so no path into the library
//! can be missed (the same reasoning as the Navidrome playlist export).

use std::time::Duration;

use droppedneedle_kernel::jobs::{AlreadyRunning, FnTick, Next, Schedule, Supervisor, TickError};

use super::state::CollectionsState;

/// Name of the linker loop.
pub const JOB_NAME: &str = "playlist-library-link";

/// How often entries are linked: soon enough that a landed download shows
/// on its playlist within a couple of minutes, cheap enough to run always
/// (one indexed update that changes nothing when nothing landed).
pub const LINK_INTERVAL: Duration = Duration::from_secs(120);

/// Start the linker on `supervisor`. The first pass runs shortly after
/// boot, so entries a download filled before an upgrade link at once.
pub fn spawn_playlist_linker(
    supervisor: &Supervisor,
    collections: &CollectionsState,
) -> Result<(), AlreadyRunning> {
    let playlists = collections.stores.playlists.clone();
    supervisor.periodic(
        JOB_NAME,
        Schedule::new(LINK_INTERVAL)
            .with_initial_delay(Duration::from_secs(30))
            .with_jitter(Duration::from_secs(15)),
        FnTick(move |_| {
            let playlists = playlists.clone();
            async move {
                match playlists.link_library().await {
                    Ok(0) => {}
                    Ok(linked) => tracing::info!(linked, "playlist entries linked to the library"),
                    Err(error) => {
                        return Err(TickError::new("PLAYLIST_LINK_FAILED", error.to_string()));
                    }
                }
                Ok(Next::Continue)
            }
        }),
    )
}
