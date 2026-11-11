-- Catalogue extension 8: an artist's Deezer link, asked of MusicBrainz.
--
-- The artist-suggestion loop asks MusicBrainz which artist links to the
-- Deezer artist of an artist's accepted album matches. One artist found
-- links the artist (or offers a merge into the one already linked); none or
-- several change nothing. The row keeps which Deezer artists were asked
-- (comma-joined) and when, so they are asked again only after a month or
-- when a new album match names another Deezer artist.

ALTER TABLE library_artist_suggestions ADD COLUMN deezer_link_ids TEXT;
ALTER TABLE library_artist_suggestions ADD COLUMN deezer_link_checked_at INTEGER;
