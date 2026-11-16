-- Catalogue extension 20: bulk playlist requests.
--
-- An admin asks for a playlist's missing songs in one go. The run paces
-- what it sends (songs per hour), may stop after a pilot of N songs for a
-- look, and lists each song with what became of it.
--   state     'running', 'pilot_done' (the pilot was sent; waits for a
--             person to go on), 'paused', 'done' or 'cancelled'
--   per_hour  songs sent per hour at most
--   pilot     songs in the pilot; 0 for none
CREATE TABLE acquire_bulk_runs (
    id TEXT PRIMARY KEY CHECK(length(id) > 0),
    playlist_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(state IN ('running', 'pilot_done', 'paused', 'done', 'cancelled')),
    per_hour INTEGER NOT NULL CHECK(per_hour > 0),
    pilot INTEGER NOT NULL DEFAULT 0 CHECK(pilot >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- One song of a run. `state` is 'queued' until sent, then 'sent'; what
-- became of it is read from its download task. `ask_key` is what the
-- download was keyed on: the recording or want key of a song asked for on
-- its own, the release group of an album fetched for several songs.
--   album_group  1 when sent within one album download for several songs
--   solo         1 when the album could not take it: it is asked for alone
CREATE TABLE acquire_bulk_items (
    id INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES acquire_bulk_runs(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    playlist_track_id TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    track_title TEXT NOT NULL,
    album_title TEXT NOT NULL DEFAULT '',
    release_group_mbid TEXT,
    recording_mbid TEXT,
    isrc TEXT,
    spotify_track_id TEXT,
    duration_ms INTEGER,
    state TEXT NOT NULL DEFAULT 'queued' CHECK(state IN ('queued', 'sent', 'failed')),
    solo INTEGER NOT NULL DEFAULT 0,
    album_group INTEGER NOT NULL DEFAULT 0,
    ask_key TEXT,
    task_id TEXT,
    detail TEXT,
    sent_at INTEGER,
    UNIQUE(run_id, playlist_track_id)
) STRICT;

CREATE INDEX acquire_bulk_runs_by_user ON acquire_bulk_runs(user_id);
CREATE INDEX acquire_bulk_items_by_state ON acquire_bulk_items(run_id, state, position);
CREATE INDEX acquire_bulk_items_by_sent ON acquire_bulk_items(sent_at);
