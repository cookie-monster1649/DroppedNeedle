# Fork plan: provider-independent catalogue on v3

Branch `nuc/v3-fixes`, based on `v3.0.0-beta.3`. Source brief: "DroppedNeedle –
Provider-independent catalogue and Spotify-to-collection pipeline" v0.1
(7 Nov 2026). This file maps that brief onto the v3 Rust code as it actually is.

## Where v3 already is

| Brief | v3 | Status |
|---|---|---|
| Internal library IDs, provider IDs alongside (§6.2) | `local_artists/albums/tracks` + `local_*_external_identities` + `local_entity_source_links` | Done for the library; identities `CHECK(provider = 'musicbrainz')` |
| Never delete on upstream change (§6.2.6) | MusicBrainz redirects followed (`providers/src/musicbrainz/redirects.rs`); undo everywhere | Done |
| #576 year-less search (§11.2) | `acquire/src/slskd/query.rs` ladders drop the year | Done for albums; tracks extended in this fork (below) |
| Review before library (§12) | `acquire_held_files` (~70 reasons), quarantine, import decisions | Done, but checked against a MusicBrainz tracklist |
| Spotify playlist import (§9) | `acquire/src/imports/*`; albums resolved to release groups **only by ISRC proof** | Playlists only; rows are name copies; "request missing" is album-level |
| Edition choice (#460, #638) | Edition sheet + `library/src/edition_prefs.rs` settings | Done natively; fork adds a track-count term |

## What the fork carries (done)

1. `edition_prefs`: `uncommon_track_count` term ranked before date, so an
   earlier bonus-track pressing (JP Black Sands, 13 tracks) loses to the
   standard count. Replaces v2 fork commit b0ff43fb7 (#638). The rest of that
   commit is covered by v3's native preferences (earliest + standard + country).
2. `slskd::query::track_query_ladder` (§11.2 levels 1–2): core title without
   `feat.` and noise brackets but with distinctive version words kept
   (Live, Remix, VIP, Edit…); rungs per co-credited artist. Never a year.

Not ported from v2 (`nuc/v2.15.0-fixes`): 9550f0b2c (#532) and 485731878
(#527). v3's importer places files against a chosen release with explicit
track assignment (`acquire/src/intake/place.rs`), so the v2 root causes
(attribution-less imports, mixed release-track IDs) do not exist as written.
Re-test both scenarios on v3; port only with a reproduction.
32839f926 (#460) is native in v3 (edition sheet, `?edition=choose`).

## Next slices, in order

### Slice A – Phase 0 core: requests without a MusicBrainz ID
The blocker for G1/#201. Today `download_tasks.release_group_mbid` is
`NOT NULL`, `request_history` is keyed by `musicbrainz_id_lower`, and
`wanted_watches`, `download_quarantine`, follows and approvals key on MBIDs.

- New migration: `catalog_wants` (internal ULID PK; kind track|release;
  artist, title, version_label, album, duration_ms, year, hint; nullable
  `release_group_mbid` / `recording_mbid`; `source` user|spotify|musicbrainz).
- `request_history`/`download_tasks`: add `want_id`; relax MBID `NOT NULL`
  via table rebuild; key new rows on `want_id`, keep MBID rows as they are.
- Acquire: a want with no MBID runs the track ladder + matcher with its own
  title/duration as the target, and lands in held review (`no_release`
  today) with a new "place as your own track list" action that writes a
  custom edition (`library_custom_edition_*`) instead of discarding.
- Identities: drop the `provider = 'musicbrainz'` CHECK on track identities
  (or store ISRC/Spotify in `local_entity_source_links`) so a placed track
  carries its ISRC/Spotify ID.

### Slice B – Phase 1b: playlist reconciliation (#204)
- Link `catalog_playlist_tracks` to `local_tracks` (existing
  `library_file_id`, `recording_mbid`) and store the Spotify track ID + ISRC.
- Per-row status (owned / requested / held / missing / unresolved) and
  counters on the playlist page.
- "Request missing" sends **track** requests (request_kind `track` exists),
  falling back to Slice A wants when the album isn't proven by ISRC.
- Liked songs (`/me/tracks`) and saved albums as pseudo-playlists.

### Slice C – sync
`snapshot_id` stored per playlist; scheduled job skips unchanged playlists;
additive/mirror; errors mark the playlist degraded and never empty it.

### Slice D – scoring
Score breakdown persisted per candidate; "no results" vs "none qualified"
outcome codes; version discrimination using the same version dictionary as
`core_track_title`; per-peer reliability.

## Building and testing
The server is Linux-only (`O_PATH`). Build in `rust:1.89.0-bookworm` with
`cmake nasm clang`:

    docker run --rm -v "$PWD":/src -v dn-cargo:/usr/local/cargo/registry \
      -v dn-target:/src/server/target -w /src/server rust:1.89.0-bookworm \
      bash -c 'apt-get update -qq && apt-get install -y -qq cmake nasm clang && cargo test -p droppedneedle-acquire --lib'

Full image: `docker build -t droppedneedle:nuc .` at the repo root.
