# Fork plan: provider-independent catalogue on v3

Branch `nuc/v3.0.0-beta.4`, based on `v3.0.0-beta.4` (rebased from beta.3). Source brief: "DroppedNeedle –
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

3. Landing match (Heavy Is the Crown, Nov 2026: one album fetched three
   times, each good copy held and the next source tried):
   - `matching::score::names_agree`: a joint album ("Anderson .Paak &
     Cordae") agrees with files crediting one of its artists when the album
     title agrees and every file pairs with a track (before, only when
     embedded ids proved the files).
   - `landing::matching::take_sibling_copies`: with a pinned edition, files
     tagged as another edition of the same group with as many tracks have
     their MusicBrainz ids set aside and are judged against the pin by title
     and length; `PinnedEdition` skips them. Another group, or another track
     count, still contradicts.
   - `LandingReport::whole_copy`: a weak-match hold on the album's names
     alone (every track paired, nothing extra, distance within
     `ACCEPT_ALBUM`) stops the task for a person instead of fetching another
     copy. Files that name other tracks still fail over.

Not ported from v2 (`nuc/v2.15.0-fixes`): 9550f0b2c (#532) and 485731878
(#527). v3's importer places files against a chosen release with explicit
track assignment (`acquire/src/intake/place.rs`), so the v2 root causes
(attribution-less imports, mixed release-track IDs) do not exist as written.
Re-test both scenarios on v3; port only with a reproduction.
32839f926 (#460) is native in v3 (edition sheet, `?edition=choose`).

## Next slices, in order

### Slice A – Phase 0 core: requests without a MusicBrainz ID
The blocker for G1/#201.

**Fork schema: the extension chain.** v3 stamps the database with one
sequential migration number (`PRAGMA user_version`, `kernel/src/schema/mod.rs`).
A fork `0145` would collide with upstream's next migration and strand the
NUC's database off the upstream chain. So fork tables live on a second,
independent chain (`kernel/src/db/extend.rs`, `kernel/src/schema/extensions.rs`):
named chains of files under `server/migrations/<chain>/`, applied after the
upstream chain, counted in their own ledger `schema_extensions(chain,
version)`. Each file runs in one IMMEDIATE transaction. A binary older than
the stored count refuses to boot (`ExtensionTooNew`). The rules, enforced by
tests: files only create, and never alter or drop an upstream table, never
touch `user_version`, and are never edited once shipped (byte-pinned in
`persist_schema`). Every gate still applies: SQL in `sql.rs`, owners in
`table-owners.txt`, and enums registered. Upstream rebases stay clean, and if upstream
ever ships the same thing, a later extension file copies the data across and the table is retired.

A1 (done): `POST /api/v3/requests/wants {artist_name, track_title,
album_title?, duration_seconds?}`. A want is a `track` request whose key is
`want-<32 hex>` = sha256(folded artist, folded title), so asking twice joins
the first ask. The key sits where a recording MBID would (request ledger
`track:want-…`, `download_tasks.recording_mbid`), so cancel, approve,
reject, retry and history work unchanged (`validate_request_key`). The
worker skips MusicBrainz for a want key and runs the existing lone-track
search; with no release group, landing holds the files (`no_release`).
Playlist "request missing" now sends wants for rows with no recording
instead of whole-album requests.

A2 (done): the held queue's Import on a want's files places them as
tagged. `held::service::want_release` builds a release from the ask (no
MusicBrainz ids; a lone track with no album is its own single), and the
existing intake/publisher path files it by the naming script and catalogues
it. The library import skips keeping a release document when the release
has no id, and `identity::adopt_import` seals nothing for id-less files and
offers the album to identification instead. The asked length is read from
the want's request row into the lone-track target, so Soulseek candidates
are judged against it.

A3 (done): a want may carry `isrc` and `spotify_track_id` (validated and
normalised; a Spotify URI or link is accepted). They are stored in
`acquire_want_identities` (chain `catalog`, file 0001). On Import the held
queue puts them on the release document as `ReleaseTrack.links`
(`matching::SourceLink`), and the ISRC is also written as a tag. The links
ride the publisher's `Adoption` (journaled in provenance, so a resumed
publish keeps them), and `identity::link_track` writes them to
`local_entity_source_links` as automatic decisions. The same path is open to
any future provider (Deezer, Tidal, Bandcamp): add a variant to
`WantIdentityProvider`. Playlist wants send the Spotify track ID; ISRCs follow
once playlist rows store them (Slice B).

### Slice B – Phase 1b: playlist reconciliation (#204)
- B1 (done): the playlist linker (`catalog::collections::linker`, every two
  minutes, and after each Spotify import) sets `library_file_id` on entries
  the library can play: same MusicBrainz recording, or the same Spotify
  track / ISRC among the track's `local_entity_source_links`. v3 only
  linked entries added by hand, so landed downloads never showed on their
  playlist (nor in Subsonic/Jellyfin playlists). A linked entry also gets
  `local` in `available_sources` (what the player reads) and, with no source
  yet, `source_type = 'local'`. The page labels Spotify imports "Imported
  from Spotify".
- B2 (done): `catalog_playlist_entry_identities` (chain `catalog` 0002)
  keeps each imported entry's Spotify track ID and ISRC; the API returns
  them and playlist wants send them.
- B3 (done): a want's file that agrees with the ask (title, artist or
  one of its credited artists, length within 7 s of the provider's length;
  `landing::want`) is held paired to the ask, so the task stops fetching
  other copies, and the held loop (`retries_by_itself`, `RETRIES`) files it
  through the want Import (A2/A3) within five minutes, with its Spotify
  id/ISRC as source links. A file that disagrees (another song, another
  length) is held unpaired for a person and the next source is tried.
- B4 (next): request status for wants: a want filed from the held list
  should show as imported, not "failed".
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
