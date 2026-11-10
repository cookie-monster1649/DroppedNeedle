import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import ProviderAlbumTracks from './ProviderAlbumTracks.svelte';

const match = {
	provider: 'deezer',
	provider_album_id: '42',
	url: 'https://www.deezer.com/album/42',
	title: 'FEMME',
	artist_name: 'Poppy Ajudha',
	provider_artist_id: '7',
	artist_picture_url: null,
	release_date: '2019-05-03',
	label: 'Own Label',
	upc: '0123',
	record_type: 'ep',
	genres: ['R&B'],
	tracks: [
		{
			provider_track_id: '1001',
			title: 'Femme',
			duration_seconds: 200,
			disc: 1,
			position: 1,
			isrc: null,
			track_id: 'file-1'
		},
		{
			provider_track_id: '1002',
			title: 'Holy Moly',
			duration_seconds: 180,
			disc: 1,
			position: 2,
			isrc: 'GBUM71900001',
			track_id: null
		}
	]
};

describe('Deezer track list', () => {
	it('shows the release facts, plays held tracks and marks missing ones', async () => {
		const onplay = vi.fn();
		fakeApi({});

		await renderWithQuery(ProviderAlbumTracks, { match, onplay });

		await expect
			.element(
				page.getByText('From Deezer · EP · 2019-05-03 · Own Label · 1 of 2 tracks in your library')
			)
			.toBeVisible();
		await expect.element(page.getByText('Missing')).toBeVisible();
		await page.getByRole('button', { name: 'Play Femme' }).click();
		expect(onplay).toHaveBeenCalledWith('file-1');
	});

	it('asks for a missing track by name, with its ISRC', async () => {
		const api = fakeApi({
			'POST /api/v3/requests/wants': { key: 'want-0123', status: 'queued' }
		});

		await renderWithQuery(ProviderAlbumTracks, { match, onplay: vi.fn() });

		await page.getByRole('button', { name: 'Want Holy Moly' }).click();
		await expect.element(page.getByText('Wanted')).toBeVisible();
		expect(api.calls('POST /api/v3/requests/wants')[0].body).toMatchObject({
			artist_name: 'Poppy Ajudha',
			track_title: 'Holy Moly',
			album_title: 'FEMME',
			duration_seconds: 180,
			isrc: 'GBUM71900001'
		});
	});

	it("previews Deezer's names in the files as the stand-in edition", async () => {
		const api = fakeApi({
			'POST /api/v3/library/albums/{album_id}/edition/tags': {
				job_id: 'write-1',
				preview_token: 'token-1',
				expires_at: 0
			},
			'GET /api/v3/library/albums/{album_id}/edition/tags/{job_id}': {
				job_id: 'write-1',
				step: 'planning',
				undo: false,
				undo_available: false,
				row_revision: 1,
				ready_to_apply: false,
				expired: false,
				stale_reasons: [],
				reason: null,
				files: []
			}
		});

		await renderWithQuery(ProviderAlbumTracks, {
			match,
			onplay: vi.fn(),
			retag: { userId: 'user-1', localId: 'local-1' }
		});

		await page.getByRole('button', { name: "Use Deezer's title and artist" }).click();
		await expect.element(page.getByText(/and no MusicBrainz ids/)).toBeVisible();
		await page.getByRole('button', { name: 'Preview the new tags' }).click();
		await expect
			.poll(() => api.calls('POST /api/v3/library/albums/{album_id}/edition/tags').length)
			.toBe(1);
		expect(api.calls('POST /api/v3/library/albums/{album_id}/edition/tags')[0].body).toEqual({
			release_mbid: 'deezer:42',
			expected_identity_revision: 0
		});
	});

	it('offers no write without the right to make one', async () => {
		fakeApi({});

		await renderWithQuery(ProviderAlbumTracks, { match, onplay: vi.fn() });

		await expect.element(page.getByText('Missing')).toBeVisible();
		expect(page.getByRole('button', { name: "Use Deezer's title and artist" }).query()).toBeNull();
	});
});
