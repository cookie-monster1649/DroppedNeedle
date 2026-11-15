//! Fork: the statements of the playlist import's "link spotify ids" step
//! ([`super::link_ids`]).

droppedneedle_kernel::sql! {
    /// Every indexed track: id, the source file it was imported from (when
    /// an import placed it), and its path below its library folder. The
    /// lowest row first, so the first of several copies wins.
    pub(super) INDEXED_TRACK_PATHS = "SELECT id, source_path, relative_path \
             FROM local_tracks WHERE availability = 'indexed' ORDER BY search_key";
}
