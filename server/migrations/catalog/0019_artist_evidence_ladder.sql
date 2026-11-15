-- Catalogue extension 19: the artist evidence ladder.
--
-- An unlinked artist is checked against further evidence (its tracks'
-- ISRCs, the Deezer artists its tracks link to, the artists it shares
-- credits with, and Deezer alone when MusicBrainz has no such artist).
-- Each check records when it ran, what evidence it saw (so new evidence
-- brings it back early), and, when nothing settled it, a short reason the
-- Artists tab shows.

ALTER TABLE library_artist_suggestions ADD COLUMN ladder_checked_at INTEGER;
ALTER TABLE library_artist_suggestions ADD COLUMN ladder_evidence TEXT;
ALTER TABLE library_artist_suggestions ADD COLUMN open_reason TEXT;

-- The providers' answers the ladder asked for, so nothing is fetched
-- twice: an ISRC's recording credits, a Deezer artist's MusicBrainz URL
-- relations, a name search, and the like. `lookup` names the question,
-- `answer` is its JSON.
CREATE TABLE library_artist_evidence_cache (
    lookup TEXT PRIMARY KEY CHECK(length(lookup) > 0),
    answer TEXT NOT NULL,
    fetched_at INTEGER NOT NULL
) STRICT;
