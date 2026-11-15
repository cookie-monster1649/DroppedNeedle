-- Catalogue extension 18: how many MusicBrainz artists share the name.
--
-- The artist-suggestion loop counts the MusicBrainz artists whose name or
-- alias folds to the artist's name. Only a suggestion whose name is the
-- one such artist may be accepted in bulk ("Accept all exact-name best
-- guesses") or on Deezer's corroboration; a name several artists share
-- always waits for a person. Null until the loop looks again.

ALTER TABLE library_artist_suggestions ADD COLUMN musicbrainz_exact_count INTEGER;

-- When the artist's matched albums were last read for its MusicBrainz
-- artist (their release credits under its name), and when its name was
-- last tried as a joined credit ("A/B") to split into its artists.
ALTER TABLE library_artist_suggestions ADD COLUMN album_evidence_checked_at INTEGER;
ALTER TABLE library_artist_suggestions ADD COLUMN split_checked_at INTEGER;
