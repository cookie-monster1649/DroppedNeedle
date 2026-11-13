//! Fork: where live playlist folders are kept (`jobs::playlist_folders`).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::Section;
use crate::runtime_config::error::ConfigError;

/// The folder each user's playlists are mirrored into as hardlinks to the
/// library files. An empty path turns it off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PlaylistFolders {
    /// An absolute folder path as the server sees it, on the same mount as
    /// the library, or empty for off.
    pub path: String,
    /// Optional explicit Nextcloud user per DroppedNeedle username, written
    /// into the folders' `.users.json` for the host helper; it wins over
    /// matching by email or SSO identity.
    pub nextcloud_users: std::collections::BTreeMap<String, String>,
}

impl Section for PlaylistFolders {
    const KEY: &'static str = "fork_playlist_folders";

    fn validate(&self) -> Result<(), ConfigError> {
        if self
            .nextcloud_users
            .iter()
            .any(|(from, to)| from.trim().is_empty() || to.trim().is_empty())
        {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "nextcloud_users",
                reason: "each entry needs a username and a Nextcloud user".to_owned(),
            });
        }
        let path = self.path.trim();
        if path.is_empty() {
            return Ok(());
        }
        let parsed = std::path::Path::new(path);
        let climbs = parsed
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir));
        if !parsed.is_absolute() || climbs {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "path",
                reason: "must be an absolute folder path without ..".to_owned(),
            });
        }
        Ok(())
    }
}
