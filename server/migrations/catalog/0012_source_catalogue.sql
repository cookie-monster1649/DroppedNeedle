-- Catalogue extension 12: a source artist's whole discography.
--
-- An artist on a stand-in id lists every album its source has, held or
-- not. Each album the source lists is kept here, with its release group
-- and release minted, so clicking it opens the normal album page and a
-- request can ask for it. The track list is filled the first time the
-- album is looked at (tracks_json NULL until then) and read again after a
-- month.

CREATE TABLE source_catalogue (
    provider TEXT NOT NULL,
    provider_album_id TEXT NOT NULL CHECK(length(provider_album_id) > 0),
    provider_artist_id TEXT NOT NULL,
    artist_name TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL,
    release_date TEXT,
    record_type TEXT,
    cover_url TEXT,
    upc TEXT,
    label TEXT,
    genres_json TEXT NOT NULL DEFAULT '[]',
    tracks_json TEXT,
    listed_at INTEGER NOT NULL,
    fetched_at INTEGER,
    PRIMARY KEY (provider, provider_album_id)
) STRICT;

CREATE INDEX idx_source_catalogue_artist ON source_catalogue(provider, provider_artist_id);
