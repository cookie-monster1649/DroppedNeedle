-- Catalogue extension 13: source artists found by search.
--
-- A search hit for an artist only a source knows gets a stand-in id. Its
-- name and picture are kept here, so its page has a header before any of
-- its albums is kept; its discography comes from the source catalogue.

CREATE TABLE source_artists (
    provider TEXT NOT NULL,
    provider_artist_id TEXT NOT NULL CHECK(length(provider_artist_id) > 0),
    name TEXT NOT NULL,
    picture_url TEXT,
    kept_at INTEGER NOT NULL,
    PRIMARY KEY (provider, provider_artist_id)
) STRICT;
