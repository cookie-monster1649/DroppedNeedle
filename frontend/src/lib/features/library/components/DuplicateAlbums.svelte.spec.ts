import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

import DuplicateAlbums from './DuplicateAlbums.svelte';

afterEach(() => authStore.clear());

function signIn(role: 'admin' | 'trusted'): void {
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Curator',
			role,
			email: null,
			avatar_url: null,
			username: 'curator',
			username_display: 'Curator'
		})
	);
}

const track = (id: string, format: string, kbps: number) => ({
	track_id: id,
	title: 'One Night',
	file_format: format,
	bit_rate: kbps * 1000,
	bit_depth: format === 'flac' ? 16 : null,
	duration_seconds: 200,
	row_revision: 1
});

const groups = {
	groups: [
		{
			kind: 'same_songs' as const,
			keep_album_id: 'album-flac',
			albums: [
				{
					album_id: 'album-flac',
					title: 'One Night',
					artist_name: 'Two Another',
					row_revision: 1,
					release_group_mbid: null,
					tracks: [track('t-flac', 'flac', 900)]
				},
				{
					album_id: 'album-mp3',
					title: 'One Night',
					artist_name: 'Two Another',
					row_revision: 1,
					release_group_mbid: null,
					tracks: [track('t-mp3', 'mp3', 320)]
				}
			],
			surplus_track_ids: ['t-mp3']
		}
	]
};

describe('Duplicates', () => {
	it('shows each copy, marks the lesser one, and offers Tidy up to admins', async () => {
		signIn('admin');
		fakeApi({ 'GET /api/v3/library/duplicates': groups });

		await renderWithQuery(DuplicateAlbums, {});

		await expect.element(page.getByText('Same songs kept twice')).toBeVisible();
		await expect.element(page.getByText('One Night · MP3 320k · lesser copy')).toBeVisible();
		await expect.element(page.getByText('One Night · FLAC 16-bit')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Tidy up One Night' })).toBeVisible();
	});

	it("doesn't offer Tidy up to a curator who can't merge", async () => {
		signIn('trusted');
		fakeApi({ 'GET /api/v3/library/duplicates': groups });

		await renderWithQuery(DuplicateAlbums, {});

		await expect.element(page.getByText('Same songs kept twice')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Tidy up One Night' }))
			.not.toBeInTheDocument();
	});
});
