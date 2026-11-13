//! Fork: the statements of the live playlist folders loop.

crate::sql! {
    /// Every owned playlist with its owner and entries in order, each entry
    /// with its indexed local file when it has one.
    pub(super) FOLDER_ENTRIES = "SELECT p.user_id AS user_id, u.display_name AS display_name, \
        p.id AS playlist_id, p.name AS playlist_name, pt.position AS position, \
        COALESCE(t.artist_name, pt.artist_name) AS artist_name, \
        COALESCE(t.title, pt.track_name) AS track_name, t.file_path AS file_path, \
        t.file_size_bytes AS file_size_bytes, t.file_mtime_ns AS file_mtime_ns \
        FROM catalog_playlists p JOIN auth_users u ON u.id = p.user_id \
        LEFT JOIN catalog_playlist_tracks pt ON pt.playlist_id = p.id \
        LEFT JOIN local_tracks t ON t.id = pt.library_file_id AND t.availability = 'indexed' \
        ORDER BY p.user_id, p.name, p.id, pt.position";
    /// One indexed library file a playlist links, for the link probe
    /// (reached by key; CROSS JOIN keeps the entries first).
    pub(super) PROBE_FILE = "SELECT t.file_path FROM catalog_playlist_tracks pt \
        CROSS JOIN local_tracks t ON t.id = pt.library_file_id \
        WHERE pt.library_file_id IS NOT NULL AND t.availability = 'indexed' LIMIT 1";
    /// Every user's sign-in names: username, email and SSO subjects (local
    /// passwords left out), for the users manifest.
    pub(super) USER_IDENTITIES = "SELECT u.id AS user_id, u.username AS username, \
        u.email AS email, p.provider AS provider, p.provider_uid AS subject \
        FROM auth_users u LEFT JOIN auth_providers p \
        ON p.user_id = u.id AND p.provider <> 'local' \
        ORDER BY u.id, p.provider, p.provider_uid";

    /// One user's sign-in name.
    pub(super) row IdentityRow for [USER_IDENTITIES] {
        user_id: String,
        username: Option<String>,
        email: Option<String>,
        provider: Option<String>,
        subject: Option<String>,
    }

    /// One playlist entry and its owner.
    pub(super) row FolderEntryRow for [FOLDER_ENTRIES] {
        user_id: String,
        display_name: String,
        playlist_id: String,
        playlist_name: String,
        position: Option<i64>,
        artist_name: Option<String>,
        track_name: Option<String>,
        file_path: Option<String>,
        file_size_bytes: Option<i64>,
        file_mtime_ns: Option<i64>,
    }
}
