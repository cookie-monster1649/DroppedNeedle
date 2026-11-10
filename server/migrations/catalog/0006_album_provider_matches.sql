-- Catalogue extension 6: an album's match outside MusicBrainz.
--
-- An album MusicBrainz has no candidate for gets a best guess from another
-- catalogue (Deezer). Check albums suggests it; a person accepts it or says
-- it is not the album. An album the provider has nothing for is kept as
-- not_found, so it is asked again only now and then. The row keeps what the album page shows from it:
--   tracks_json   the provider's track list, in album order
--   genres_json   the provider's genre names
-- A MusicBrainz link the album gets later wins; the row stays as extra data.

CREATE TABLE library_album_provider_matches (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    provider TEXT NOT NULL CHECK(provider IN ('deezer')),
    provider_album_id TEXT NOT NULL CHECK(length(provider_album_id) > 0),
    title TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    provider_artist_id TEXT,
    artist_picture_url TEXT,
    release_date TEXT,
    label TEXT,
    upc TEXT,
    record_type TEXT,
    cover_url TEXT,
    link_url TEXT,
    genres_json TEXT NOT NULL DEFAULT '[]',
    tracks_json TEXT NOT NULL DEFAULT '[]',
    score REAL NOT NULL,
    match_state TEXT NOT NULL CHECK(match_state IN ('suggested','confirmed','declined','not_found')),
    decided_by_user_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_library_album_provider_matches_provider
    ON library_album_provider_matches(provider, provider_album_id);
