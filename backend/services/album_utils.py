from typing import Optional
from api.v1.schemas.album import Track


def parse_year(date_str: Optional[str]) -> Optional[int]:
    if not date_str:
        return None
    year = date_str.split("-", 1)[0]
    return int(year) if year.isdigit() else None


def find_primary_release(release_group: dict) -> Optional[dict]:
    ranked = get_ranked_releases(release_group)
    return ranked[0] if ranked else None


_EXPANDED_EDITION_KEYWORDS = (
    "deluxe",
    "box set",
    "boxset",
    "expanded",
    "legacy",
    "collector",
    "complete",
    "ultimate",
    "anniversary",
    "special edition",
)
_PHYSICAL_EDITION_KEYWORDS = ("vinyl", "cassette", "gatefold")


def _is_expanded_edition(release: dict) -> bool:
    text = f"{release.get('disambiguation') or ''} {release.get('title') or ''}".lower()
    return any(keyword in text for keyword in _EXPANDED_EDITION_KEYWORDS)


def _release_track_count(release: dict) -> int:
    try:
        return sum(int(m.get("track-count") or 0) for m in (release.get("media") or []))
    except (TypeError, ValueError):
        return 0


def get_ranked_releases(release_group: dict) -> list[dict]:
    """Return official releases with the standard original edition first.

    Order: non-expanded editions (no deluxe/box set/anniversary...) first, then
    releases with the group's standard track count (the most common count across
    non-expanded editions, ties to the smaller count, so early regional bonus
    editions lose), then the earliest release date, then CD/digital over
    vinyl/cassette, with the MBID only as a stable final tie-break (#638).
    """
    from repositories.edition_policy import edition_date_key

    releases = release_group.get("releases") or release_group.get("release-list", [])
    official = [r for r in releases if r.get("status") == "Official"]
    if not official:
        official = list(releases)

    counts: dict[int, int] = {}
    for release in official:
        track_count = _release_track_count(release)
        if track_count > 0 and not _is_expanded_edition(release):
            counts[track_count] = counts.get(track_count, 0) + 1
    standard = max(counts, key=lambda n: (counts[n], -n)) if counts else None

    def _release_sort_key(r: dict) -> tuple:
        packaging = (r.get("packaging") or "").lower()
        formats = " ".join((m.get("format") or "") for m in (r.get("media") or [])).lower()
        physical = any(
            keyword in packaging or keyword in formats
            for keyword in _PHYSICAL_EDITION_KEYWORDS
        )
        non_standard = 0 if standard is None or _release_track_count(r) == standard else 1
        return (
            1 if _is_expanded_edition(r) else 0,
            non_standard,
            edition_date_key(r.get("date")),
            1 if physical else 0,
            r.get("id", ""),
        )

    official.sort(key=_release_sort_key)
    return official


def extract_artist_info(release_group: dict) -> tuple[str, str]:
    artist_credit = release_group.get("artist-credit", [])
    artist_name = "Unknown Artist"
    artist_id = ""
    if artist_credit and isinstance(artist_credit, list):
        first_artist = artist_credit[0]
        if isinstance(first_artist, dict):
            artist_obj = first_artist.get("artist", {})
            artist_name = first_artist.get("name") or artist_obj.get(
                "name", "Unknown Artist"
            )
            artist_id = artist_obj.get("id", "")
    return artist_name, artist_id


def extract_tracks(release_data: dict) -> tuple[list[Track], int]:
    tracks = []
    total_length = 0
    medium_list = release_data.get("media") or release_data.get("medium-list", [])
    for medium in medium_list:
        try:
            disc_number = int(medium.get("position") or medium.get("number") or 1)
        except (TypeError, ValueError):
            disc_number = 1
        medium_format = medium.get("format")
        if not isinstance(medium_format, str):
            medium_format = None
        track_list = medium.get("tracks") or medium.get("track-list", [])
        for track in track_list:
            recording = track.get("recording", {})
            length_ms = track.get("length") or recording.get("length")
            if length_ms:
                try:
                    total_length += int(length_ms)
                except (ValueError, TypeError):
                    pass
            tracks.append(
                Track(
                    position=int(track.get("position") or track.get("number", 0)),
                    disc_number=disc_number,
                    title=track.get("title") or recording.get("title") or "Unknown",
                    length=int(length_ms) if length_ms else None,
                    recording_id=recording.get("id"),
                    release_track_id=track.get("id"),
                    media_format=medium_format,
                )
            )
    return tracks, total_length


# MusicBrainz medium-format vocabulary names video carriers explicitly ("DVD",
# "DVD-Video", "Blu-ray", ...); DVD-Audio discs are labeled "DVD-Audio" and stay.
# Live-verified 2026-09-06: release 0b683a6a (Tom Jones DE 2006 Greatest Hits)
# returns media formats "CD" (16 tracks) + "DVD" (14 tracks).
_VIDEO_MEDIUM_FORMATS = frozenset(
    {
        "dvd",
        "dvd-video",
        "blu-ray",
        "hd-dvd",
        "hdv",
        "vhs",
        "betamax",
        "video cd",
        "video-cd",
        "vcd",
        "svcd",
        "laserdisc",
    }
)


def is_audio_medium(medium_format: str | None) -> bool:
    """Whether a MusicBrainz medium carries downloadable audio.

    Anything unrecognized - or missing, as on local-library tracklists and old
    cache blobs - reads as audio: acquisition must fail open, never strand.
    """
    if not medium_format:
        return True
    return medium_format.strip().casefold() not in _VIDEO_MEDIUM_FORMATS


def audio_tracks(tracks: list) -> list:  # noqa: ANN001 - Track shape, duck-typed
    """The canonical audio-only acquisition target set (Slices 4+5).

    Every Soulseek/Usenet acquisition denominator - request-time
    ``track_count``, the enqueue manifest, the completion coverage gate, the
    wanted watcher's satisfaction check - filters through this one helper so
    request targets and coverage can never measure different editions (the
    CD+DVD 16-vs-30 split). Display paths (album page) keep the full list, as
    do explicit per-track requests and the Free Music backend's soft
    count-matching signal.
    """
    return [
        track
        for track in tracks
        if is_audio_medium(getattr(track, "media_format", None))
    ]


def extract_label(release_data: dict) -> Optional[str]:
    label_info_list = release_data.get("label-info") or release_data.get(
        "label-info-list", []
    )
    if label_info_list:
        label_obj = label_info_list[0].get("label")
        if label_obj:
            return label_obj.get("name")
    return None


def build_album_basic_info(
    release_group: dict,
    release_group_id: str,
    artist_name: str,
    artist_id: str,
    in_library: bool,
) -> dict:
    return {
        "title": release_group.get("title", "Unknown Album"),
        "musicbrainz_id": release_group_id,
        "artist_name": artist_name,
        "artist_id": artist_id,
        "release_date": release_group.get("first-release-date"),
        "year": parse_year(release_group.get("first-release-date")),
        "type": release_group.get("primary-type"),
        "disambiguation": release_group.get("disambiguation"),
        "tracks": [],
        "total_tracks": 0,
        "in_library": in_library,
    }


def mb_to_basic_info(
    release_group: dict, release_group_id: str, in_library: bool, is_requested: bool
) -> dict:
    artist_name, artist_id = extract_artist_info(release_group)
    return {
        "title": release_group.get("title", "Unknown Album"),
        "musicbrainz_id": release_group_id,
        "artist_name": artist_name,
        "artist_id": artist_id,
        "release_date": release_group.get("first-release-date"),
        "year": parse_year(release_group.get("first-release-date")),
        "type": release_group.get("primary-type"),
        "disambiguation": release_group.get("disambiguation"),
        "in_library": in_library,
        "requested": is_requested and not in_library,
        "cover_url": None,
    }
