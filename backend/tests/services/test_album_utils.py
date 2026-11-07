from services.album_utils import extract_tracks


def test_extract_tracks_preserves_disc_numbers_and_track_positions():
    release_data = {
        "media": [
            {
                "position": "1",
                "tracks": [
                    {
                        "position": "1",
                        "title": "Disc One Intro",
                        "length": 1000,
                        "recording": {"id": "rec-1", "title": "Disc One Intro"},
                    },
                    {
                        "position": "2",
                        "title": "Disc One Main",
                        "recording": {
                            "id": "rec-2",
                            "title": "Disc One Main",
                            "length": 2000,
                        },
                    },
                ],
            },
            {
                "position": "2",
                "tracks": [
                    {
                        "position": "1",
                        "title": "Disc Two Outro",
                        "length": 3000,
                        "recording": {"id": "rec-3", "title": "Disc Two Outro"},
                    }
                ],
            },
        ]
    }

    tracks, total_length = extract_tracks(release_data)

    assert [
        (track.disc_number, track.position, track.title, track.recording_id)
        for track in tracks
    ] == [
        (1, 1, "Disc One Intro", "rec-1"),
        (1, 2, "Disc One Main", "rec-2"),
        (2, 1, "Disc Two Outro", "rec-3"),
    ]
    assert total_length == 6000


def test_extract_tracks_prefers_exact_release_track_title():
    release_data = {
        "media": [
            {
                "position": 1,
                "tracks": [
                    {
                        "position": 14,
                        "title": "The Fisherman Will Be Bewildered",
                        "recording": {
                            "id": "ec935e35-b2fa-4925-aa83-052d9e3e69f1",
                            "title": "The Fishermen Will Be Bewildered",
                        },
                    }
                ],
            }
        ]
    }

    tracks, _total_length = extract_tracks(release_data)

    assert tracks[0].title == "The Fisherman Will Be Bewildered"


from types import SimpleNamespace

from services.album_utils import audio_tracks, is_audio_medium


def test_extract_tracks_stamps_medium_format_per_medium():
    release_data = {
        "media": [
            {
                "position": "1",
                "format": "CD",
                "tracks": [
                    {
                        "position": "1",
                        "title": "Audio Song",
                        "recording": {"id": "rec-1", "title": "Audio Song"},
                    }
                ],
            },
            {
                "position": "2",
                "format": "DVD",
                "tracks": [
                    {
                        "position": "1",
                        "title": "Video Clip",
                        "recording": {"id": "rec-2", "title": "Video Clip"},
                    }
                ],
            },
            {
                "position": "3",
                "tracks": [
                    {
                        "position": "1",
                        "title": "Unknown Carrier",
                        "recording": {"id": "rec-3", "title": "Unknown Carrier"},
                    }
                ],
            },
        ]
    }

    tracks, _total = extract_tracks(release_data)

    assert [track.media_format for track in tracks] == ["CD", "DVD", None]


def test_is_audio_medium_video_carriers_excluded_audio_kept():
    for fmt in ("CD", "DVD-Audio", "SACD", "Vinyl", "Digital Media", "Cassette"):
        assert is_audio_medium(fmt) is True
    for fmt in ("DVD", "DVD-Video", "Blu-ray", "HD-DVD", "VHS", "Video CD", "Laserdisc"):
        assert is_audio_medium(fmt) is False
    assert is_audio_medium("  dvd  ") is False  # case/whitespace tolerant
    # Fail-open: missing or unrecognized formats never strand an acquisition.
    assert is_audio_medium(None) is True
    assert is_audio_medium("") is True
    assert is_audio_medium("Future-Carrier-3000") is True


def test_audio_tracks_filters_video_duck_typed_and_fail_open():
    tracks = [
        SimpleNamespace(title="a", media_format="CD"),
        SimpleNamespace(title="b", media_format="DVD"),
        SimpleNamespace(title="c"),  # legacy shape without the field: kept
        SimpleNamespace(title="d", media_format=None),  # local rows: kept
    ]

    assert [t.title for t in audio_tracks(tracks)] == ["a", "c", "d"]


def _rel(rid, date, country, tracks, *, status="Official", disambiguation="", fmt="CD"):
    return {
        "id": rid,
        "status": status,
        "date": date,
        "country": country,
        "disambiguation": disambiguation,
        "media": [{"track-count": tracks, "format": fmt}],
    }


def _ranked_ids(releases):
    from services.album_utils import get_ranked_releases

    return [r["id"] for r in get_ranked_releases({"releases": releases})]


def test_ranking_demotes_expanded_editions():
    """#638: a 48-track super deluxe must not beat the 12-track album."""
    releases = [
        _rel("aaa-deluxe", "1994", "XW", 48, disambiguation="super deluxe edition"),
        _rel("zzz-std", "1994-03-22", "US", 12),
    ]
    assert _ranked_ids(releases)[0] == "zzz-std"


def test_ranking_prefers_standard_track_count_over_early_bonus_edition():
    """An earlier JP +1 bonus edition loses to the standard 12-track album."""
    releases = [
        _rel("jp-13", "2010-03-13", "JP", 13),
        _rel("xw-12", "2010-03-23", "XW", 12, fmt="Digital Media"),
        _rel("gb-12", "2010-03-29", "GB", 12),
    ]
    assert _ranked_ids(releases)[0] == "xw-12"


def test_ranking_prefers_original_over_later_reissue():
    """Same track count: the earliest-dated (original) release wins, not MBID order."""
    releases = [
        _rel("0437-de-2007", "2007-06-26", "DE", 11),
        _rel("340d-au-2005", "2005-04-17", "AU", 11),
        _rel("3704-us-2007", "2007-02-06", "US", 11),
    ]
    assert _ranked_ids(releases)[0] == "340d-au-2005"


def test_ranking_prefers_cd_over_vinyl_on_same_date():
    releases = [
        _rel("a-vinyl", "2007-05-07", "GB", 11, fmt='12" Vinyl'),
        _rel("b-cd", "2007-05-07", "GB", 11),
    ]
    assert _ranked_ids(releases)[0] == "b-cd"


def test_ranking_without_dates_or_counts_keeps_mbid_tiebreak():
    releases = [
        {"id": "owned-rel", "status": "Official", "country": "GB"},
        {"id": "deluxe-rel", "status": "Official", "country": "XW"},
    ]
    assert _ranked_ids(releases) == ["deluxe-rel", "owned-rel"]
