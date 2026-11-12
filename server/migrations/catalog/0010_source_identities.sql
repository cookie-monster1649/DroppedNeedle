-- Catalogue extension 10: stand-in ids for metadata from other sources.
--
-- An album confirmed from a source other than MusicBrainz (Deezer today)
-- takes MBID-shaped stand-in ids in its identity rows: a version-8 UUID
-- that starts `d0ab1e00-50c3-8` (see providers `virtual_ids`). Each id
-- minted is kept here with what it names, so a page, a lookup or a tag
-- can turn it back into the source's item. Ids are derived from the
-- source item, so a row is never rewritten: minting again finds it.
--
-- The promotion check records when an album on stand-in ids was last
-- looked up at MusicBrainz, so a confident match there replaces them and
-- an album MusicBrainz cannot place is asked again only after a month.

CREATE TABLE source_identities (
    uuid TEXT PRIMARY KEY CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    provider TEXT NOT NULL CHECK(provider IN ('deezer')),
    kind TEXT NOT NULL
        CHECK(kind IN ('release_group','release','release_track','recording','artist','label')),
    provider_id TEXT NOT NULL CHECK(length(provider_id) > 0),
    provider_album_id TEXT,
    created_at INTEGER NOT NULL,
    UNIQUE (provider, kind, provider_id)
) STRICT;

CREATE TABLE source_promotion_checks (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    checked_at INTEGER NOT NULL,
    outcome TEXT NOT NULL CHECK(outcome IN ('promoted','kept','unplaceable'))
) STRICT;
