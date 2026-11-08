import { describe, expect, it, vi } from 'vitest';

vi.mock('$lib/api/client', () => ({ api: { v3: { POST: vi.fn(), GET: vi.fn() } } }));

import { planMissingRequests, type PlaylistTrack } from './playlists';

function row(over: Partial<PlaylistTrack>): PlaylistTrack {
	return {
		id: 'r',
		position: 0,
		track_name: 'Song',
		artist_name: 'Artist',
		album_name: 'Album',
		album_id: null,
		artist_id: null,
		track_source_id: null,
		cover_url: null,
		source_type: '',
		available_sources: null,
		format: null,
		track_number: null,
		disc_number: null,
		duration: 183,
		plex_rating_key: null,
		library_file_id: null,
		recording_mbid: null,
		spotify_track_id: null,
		isrc: null,
		created_at: '',
		...over
	} as PlaylistTrack;
}

describe('planMissingRequests', () => {
	it('asks for a track when the recording is proven, by name otherwise', () => {
		const plan = planMissingRequests([
			row({ recording_mbid: 'rec-1', album_id: 'rg-1' }),
			row({ recording_mbid: 'rec-1', album_id: 'rg-1' }),
			row({ album_id: 'rg-2', track_name: 'Halo (Live)', artist_name: 'Beyoncé' }),
			row({ track_name: 'halo (live)', artist_name: 'beyoncé' }),
			row({ album_id: 'rg-3', library_file_id: 'owned' }),
			row({ recording_mbid: 'rec-4', available_sources: ['local'] }),
			row({ track_name: '' })
		]);
		expect(plan.tracks.map((t) => t.recording_mbid)).toEqual(['rec-1']);
		expect(plan.tracks[0]).toMatchObject({ release_group_mbid: 'rg-1', duration_seconds: 183 });
		expect(plan.wants).toEqual([
			{
				artist_name: 'Beyoncé',
				track_title: 'Halo (Live)',
				album_title: 'Album',
				duration_seconds: 183,
				isrc: null,
				spotify_track_id: null
			}
		]);
	});

	it("carries a row's provider ids with its want", () => {
		const plan = planMissingRequests([
			row({ spotify_track_id: '4uLU6hMCjMI75M1A2tKUQC', isrc: 'USUM71703861' }),
			row({ track_name: 'Other' })
		]);
		expect(plan.wants.map((w) => [w.spotify_track_id, w.isrc])).toEqual([
			['4uLU6hMCjMI75M1A2tKUQC', 'USUM71703861'],
			[null, null]
		]);
	});
});
