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
});
