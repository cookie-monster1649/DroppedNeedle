-- Catalogue extension 1: what identifies a want outside MusicBrainz.
--
-- A want is a track asked for by name (request key `want-<32 hex>`, see
-- acquire::requests::ledger). Its identities ride beside it here until its
-- files are placed, when they move to the placed track's
-- local_entity_source_links. Same shape as those links: provider and
-- entity type name what the id is.
--   isrc     recording   ISRC, upper case, 12 characters
--   spotify  track       Spotify track id, 22 base-62 characters

CREATE TABLE acquire_want_identities (
    want_key TEXT NOT NULL CHECK(length(want_key) = 37 AND substr(want_key, 1, 5) = 'want-'),
    provider TEXT NOT NULL CHECK(provider IN ('isrc', 'spotify')),
    external_entity_type TEXT NOT NULL CHECK(length(external_entity_type) > 0),
    external_id TEXT NOT NULL CHECK(length(external_id) > 0),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (want_key, provider, external_id)
) STRICT;
