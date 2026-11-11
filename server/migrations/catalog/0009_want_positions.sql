-- Catalogue extension 9: where a want sits on the album it was asked from.
--
-- A track asked for by name from a provider's album (a Deezer-matched
-- album's Request) carries that album's disc and track number. The held
-- import files a file with no track number of its own at this position,
-- so one album's songs keep their order and one folder. One row per want
-- and album; a repeat ask from the same album keeps the newest numbers.

CREATE TABLE acquire_want_positions (
    want_key TEXT NOT NULL CHECK(length(want_key) = 37 AND substr(want_key, 1, 5) = 'want-'),
    album_title TEXT NOT NULL,
    disc_number INTEGER NOT NULL CHECK(disc_number > 0),
    track_number INTEGER NOT NULL CHECK(track_number > 0),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (want_key, album_title)
) STRICT;
