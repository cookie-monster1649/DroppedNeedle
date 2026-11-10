-- Catalogue extension 7: suggestions for an artist with no MusicBrainz link.
--
-- A background loop looks at artists the library holds with no MusicBrainz
-- link and keeps one row each:
--   merge_artist_id        another live artist that is plainly the same
--                          (same name once folded, or nearly), to merge into
--   musicbrainz_artist_id  the MusicBrainz artist of the same name, to link
--   deezer_artist_id       Deezer's artist of the same name, whose picture
--                          stands in for the artist's own
-- Check albums shows the merge or link; a person takes it or says it is not
-- them (declined), which the loop leaves alone. The picture stays either
-- way. merge_artist_id names no foreign key: the artist it names may be
-- merged away meanwhile, and reads keep only a live one.

CREATE TABLE library_artist_suggestions (
    local_artist_id TEXT PRIMARY KEY REFERENCES local_artists(id) ON DELETE CASCADE,
    merge_artist_id TEXT,
    musicbrainz_artist_id TEXT
        CHECK (musicbrainz_artist_id IS NULL OR length(musicbrainz_artist_id) = 36),
    musicbrainz_name TEXT,
    musicbrainz_disambiguation TEXT,
    deezer_artist_id TEXT,
    picture_url TEXT,
    link_url TEXT,
    fans INTEGER,
    suggestion_state TEXT NOT NULL CHECK(suggestion_state IN ('open','declined')),
    decided_by_user_id TEXT,
    checked_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
