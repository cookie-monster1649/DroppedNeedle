//! Fork: the download hold. While a playlist-folder import is pending or
//! running, a synced Spotify playlist that is one of the import's
//! playlists (the Spotify ids the public-playlist match mapped to its
//! folders) does not ask for its new songs: the import is about to bring
//! them in. The songs a sync would have asked for are kept here and asked
//! for once the import's real run has finished, or when an admin releases
//! the hold. Kept in `hold.json` beside the import's state, so a restart
//! keeps it.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::imports::spotify::ImportedTrack;

/// Songs a sync held back for one user's playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deferred {
    pub user_id: String,
    /// The internal playlist id.
    pub playlist_id: String,
    pub tracks: Vec<ImportedTrack>,
}

/// The hold as kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HoldFile {
    /// The Spotify playlist ids the import covers.
    pub playlist_ids: BTreeSet<String>,
    /// An admin released it.
    pub released: bool,
    /// The import's real run finished.
    pub finished: bool,
    pub deferred: Vec<Deferred>,
}

/// The hold's state, for the status.
#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
pub struct HoldStatus {
    /// `off` (no playlists known), `holding`, `released` or `finished`.
    pub state: String,
    /// Spotify playlists the hold covers.
    pub playlists: usize,
    /// Songs held back, waiting to be asked for.
    pub deferred_songs: usize,
}

/// The download hold.
#[derive(Debug, Default)]
pub struct PlaylistHold {
    path: Option<PathBuf>,
    inner: Mutex<HoldFile>,
}

impl PlaylistHold {
    /// The hold kept at `path` (read now; missing reads as empty).
    pub fn at(path: PathBuf) -> Self {
        let inner = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path: Some(path),
            inner: Mutex::new(inner),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut HoldFile) -> (R, bool)) -> R {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let (out, changed) = f(&mut inner);
        if changed && let Some(path) = &self.path {
            let written = serde_json::to_vec(&*inner)
                .map_err(std::io::Error::other)
                .and_then(|bytes| {
                    let dir = path.parent().map(PathBuf::from).unwrap_or_default();
                    super::write_atomic(&dir, path, &bytes)
                });
            if let Err(error) = written {
                tracing::warn!(%error, "playlist import hold not kept");
            }
        }
        out
    }

    /// Whether a sync of Spotify playlist `spotify_id` holds its asks.
    pub fn holds(&self, spotify_id: &str) -> bool {
        self.with(|hold| {
            (
                !hold.released && !hold.finished && hold.playlist_ids.contains(spotify_id),
                false,
            )
        })
    }

    /// The playlists the import covers, as the match found them. Ids
    /// already known stay (a step that could not list keeps them).
    pub fn set_playlists(&self, ids: BTreeSet<String>) {
        self.with(|hold| {
            let before = hold.playlist_ids.len();
            hold.playlist_ids.extend(ids);
            ((), hold.playlist_ids.len() != before)
        });
    }

    /// Keep songs a sync held back.
    pub fn defer(&self, user_id: &str, playlist_id: &str, tracks: Vec<ImportedTrack>) {
        if tracks.is_empty() {
            return;
        }
        self.with(|hold| {
            hold.deferred.push(Deferred {
                user_id: user_id.to_owned(),
                playlist_id: playlist_id.to_owned(),
                tracks,
            });
            ((), true)
        });
    }

    /// An admin releases the hold: answers the songs to ask for now.
    pub fn release(&self) -> Vec<Deferred> {
        self.with(|hold| {
            hold.released = true;
            (std::mem::take(&mut hold.deferred), true)
        })
    }

    /// The real run finished: answers the songs to ask for now.
    pub fn finish(&self) -> Vec<Deferred> {
        self.with(|hold| {
            hold.finished = true;
            (std::mem::take(&mut hold.deferred), true)
        })
    }

    /// The hold's state.
    pub fn status(&self) -> HoldStatus {
        self.with(|hold| {
            let state = if hold.finished {
                "finished"
            } else if hold.released {
                "released"
            } else if hold.playlist_ids.is_empty() {
                "off"
            } else {
                "holding"
            };
            (
                HoldStatus {
                    state: state.to_owned(),
                    playlists: hold.playlist_ids.len(),
                    deferred_songs: hold.deferred.iter().map(|d| d.tracks.len()).sum(),
                },
                false,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(name: &str) -> ImportedTrack {
        ImportedTrack {
            track_name: name.to_owned(),
            artist_name: "A".to_owned(),
            album_name: String::new(),
            album_id: String::new(),
            source_type: String::new(),
            track_number: None,
            disc_number: None,
            duration: Some(200),
            cover_url: None,
            recording_mbid: None,
            spotify_track_id: Some("t".to_owned()),
            isrc: None,
            spotify_album_id: None,
        }
    }

    #[test]
    fn a_hold_defers_until_the_run_finishes_and_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("dn-hold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("hold.json");
        let hold = PlaylistHold::at(path.clone());
        assert!(!hold.holds("p1"));
        assert_eq!(hold.status().state, "off");
        hold.set_playlists(["p1".to_owned()].into());
        assert!(hold.holds("p1"));
        assert!(!hold.holds("other"));
        hold.defer("u", "pl", vec![track("x"), track("y")]);
        let again = PlaylistHold::at(path.clone());
        assert_eq!(again.status().deferred_songs, 2);
        assert_eq!(again.status().state, "holding");
        let asked = again.finish();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].tracks.len(), 2);
        assert!(!again.holds("p1"));
        assert_eq!(again.status().state, "finished");
        assert_eq!(again.status().deferred_songs, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_admin_release_lets_the_songs_go() {
        let hold = PlaylistHold::default();
        hold.set_playlists(["p1".to_owned()].into());
        hold.defer("u", "pl", vec![track("x")]);
        assert_eq!(hold.release().len(), 1);
        assert!(!hold.holds("p1"));
        assert_eq!(hold.status().state, "released");
    }
}
