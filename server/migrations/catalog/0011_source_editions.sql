-- Catalogue extension 11: the other editions a source has of an album.
--
-- An album on stand-in ids is one Deezer album. Deezer often has the same
-- record more than once (explicit and clean, deluxe, remastered). Each
-- such album is kept here under the album it is an edition of, with the
-- track list a release document needs, so the album page lists it as an
-- edition and a request can ask for it. Rows are replaced as a set when
-- the album is checked again.
--
-- The check records when an album was last looked up at the source, so
-- it is asked again only after a month.

CREATE TABLE source_editions (
    provider TEXT NOT NULL,
    provider_album_id TEXT NOT NULL CHECK(length(provider_album_id) > 0),
    parent_album_id TEXT NOT NULL CHECK(length(parent_album_id) > 0),
    title TEXT NOT NULL,
    release_date TEXT,
    upc TEXT,
    label TEXT,
    record_type TEXT,
    cover_url TEXT,
    tracks_json TEXT NOT NULL DEFAULT '[]',
    fetched_at INTEGER NOT NULL,
    PRIMARY KEY (provider, provider_album_id)
) STRICT;

CREATE INDEX idx_source_editions_parent ON source_editions(provider, parent_album_id);

CREATE TABLE source_edition_checks (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    checked_at INTEGER NOT NULL
) STRICT;
