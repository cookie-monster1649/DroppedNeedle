-- Catalogue extension 17: stand-in albums MusicBrainz turned out to know.
--
-- A Deezer album on a stand-in release group can be the same record as a
-- MusicBrainz release group: its files were placed under that group (by
-- a person's Match or a landing), or a check before a request found it
-- at MusicBrainz by barcode or by a confident artist and title match.
-- The stand-in then points at the MusicBrainz group: its album page
-- opens the group's, the library counts it as held when it holds the
-- group, and its request and wanted watch are settled.
--   via   'files'    files of a download or held entry for the stand-in
--                    sit under, or carry, the group
--         'barcode'  the Deezer UPC is a barcode of a release of the group
--         'name'     artist and title (and track count) agree
--         'library'  a library album of the same artist and title holds
--                    the group

CREATE TABLE source_links (
    stand_in TEXT PRIMARY KEY
        CHECK (length(stand_in) = 36 AND stand_in = lower(stand_in)
               AND substr(stand_in, 1, 15) = 'd0ab1e00-50c3-8'),
    musicbrainz_id TEXT NOT NULL
        CHECK (length(musicbrainz_id) = 36 AND musicbrainz_id = lower(musicbrainz_id)
               AND substr(musicbrainz_id, 1, 15) <> 'd0ab1e00-50c3-8'),
    via TEXT NOT NULL CHECK (via IN ('files', 'barcode', 'name', 'library')),
    linked_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_source_links_musicbrainz ON source_links(musicbrainz_id);
