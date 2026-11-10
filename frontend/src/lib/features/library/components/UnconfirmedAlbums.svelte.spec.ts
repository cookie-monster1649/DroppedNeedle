import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

import UnconfirmedAlbums from './UnconfirmedAlbums.svelte';

afterEach(() => authStore.clear());

type Waiting = RouteData<'GET /api/v3/library/unconfirmed'>['items'][number];

function album(id: string, title: string, code: string, message: string): Waiting {
	return {
		album_id: id,
		title,
		artist_name: 'The Lanterns',
		state: 'unmatched',
		release_mbid: null,
		release_group_mbid: null,
		reason: { code, message, action: '' },
		updated_at: 1,
		identity_revision: 0,
		guess: null,
		file_count: 3,
		looks_wrong_count: 0,
		provider_guess: null
	};
}

describe('Check albums', () => {
	it('lists an edition taken anyway as needing its files matched, not as not found', async () => {
		authStore.setUser(
			toAuthUser({
				id: 'user-1',
				display_name: 'Curator',
				role: 'trusted',
				email: null,
				avatar_url: null,
				username: 'curator',
				username_display: 'Curator'
			})
		);
		fakeApi({
			'GET /api/v3/library/unconfirmed': {
				items: [
					album(
						'album-1',
						'Track Album',
						'CHOSEN_EDITION_FITS_NO_FILE',
						"The edition you chose doesn't match these files."
					),
					album('album-2', 'Lost Album', 'NO_CANDIDATE', 'Nothing on MusicBrainz looks like it.')
				],
				next_cursor: null,
				total: 2
			}
		});

		await renderWithQuery(UnconfirmedAlbums, { view: 'unmatched' });

		const row = (title: string) => page.getByRole('listitem').filter({ hasText: title });
		await expect.element(row('Track Album').getByText('Needs matching')).toBeVisible();
		await expect
			.element(row('Track Album').getByRole('link', { name: 'Match files' }))
			.toBeVisible();
		await expect.element(row('Track Album').getByText('Not found')).not.toBeInTheDocument();
		await expect.element(row('Lost Album').getByText('Not found')).toBeVisible();
		await expect
			.element(row('Lost Album').getByRole('link', { name: 'Pick an edition' }))
			.toBeVisible();
	});

	it('offers the closest candidate of an album nothing fits as a best guess to use', async () => {
		authStore.setUser(
			toAuthUser({
				id: 'user-1',
				display_name: 'Curator',
				role: 'trusted',
				email: null,
				avatar_url: null,
				username: 'curator',
				username_display: 'Curator'
			})
		);
		const near = album('album-3', 'Near Album', 'INSUFFICIENT_EVIDENCE', 'Not enough to be sure.');
		near.guess = {
			title: 'The Closest',
			artist_name: 'The Lanterns',
			release_group_mbid: 'rg-1',
			release_mbid: 'rel-1',
			matched_files: 2
		};
		fakeApi({
			'GET /api/v3/library/unconfirmed': { items: [near], next_cursor: null, total: 1 }
		});

		await renderWithQuery(UnconfirmedAlbums, { view: 'unmatched' });

		const row = page.getByRole('listitem').filter({ hasText: 'Near Album' });
		await expect.element(row.getByText('Best guess')).toBeVisible();
		await expect.element(row.getByText('The Closest')).toBeVisible();
		await expect.element(row.getByRole('button', { name: /^Use this/ })).toBeVisible();
		await expect.element(row.getByRole('link', { name: 'Compare' })).toBeVisible();
		await expect.element(row.getByText('Not found')).not.toBeInTheDocument();
	});

	it("offers Deezer's closest album when MusicBrainz has no candidate", async () => {
		authStore.setUser(
			toAuthUser({
				id: 'user-1',
				display_name: 'Curator',
				role: 'trusted',
				email: null,
				avatar_url: null,
				username: 'curator',
				username_display: 'Curator'
			})
		);
		const lone = album('album-4', 'Lone EP', 'NO_CANDIDATE', 'Nothing on MusicBrainz fits.');
		lone.provider_guess = {
			provider: 'deezer',
			provider_album_id: '42',
			title: 'Lone EP',
			artist_name: 'The Lanterns',
			year: 2019,
			record_type: 'ep',
			cover_url: null,
			url: 'https://www.deezer.com/album/42',
			track_count: 5,
			matched_files: 3,
			score: 0.9
		};
		fakeApi({
			'GET /api/v3/library/unconfirmed': { items: [lone], next_cursor: null, total: 1 }
		});

		await renderWithQuery(UnconfirmedAlbums, { view: 'unmatched' });

		const row = page.getByRole('listitem').filter({ hasText: 'Lone EP' }).first();
		await expect.element(row.getByText('Best guess · from Deezer')).toBeVisible();
		await expect.element(row.getByText(/2019 · 3 of 3 files on its 5 tracks/)).toBeVisible();
		await expect.element(row.getByRole('button', { name: /^Use this/ })).toBeVisible();
		await expect.element(row.getByRole('button', { name: /^Not it/ })).toBeVisible();
		await expect.element(row.getByText('Not found')).not.toBeInTheDocument();
	});
});
