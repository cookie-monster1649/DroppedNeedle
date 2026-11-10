import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

import ArtistSuggestions from './ArtistSuggestions.svelte';

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

const suggestions = {
	items: [
		{
			artist_id: 'artist-link',
			name: 'Poppy Ajudha',
			artist_revision: 1,
			album_count: 2,
			musicbrainz: {
				musicbrainz_artist_id: '5b11f4ce-a62d-471e-81fc-a69a8278c7da',
				name: 'Poppy Ajudha',
				disambiguation: 'UK singer'
			},
			merge: null,
			deezer_url: 'https://www.deezer.com/artist/55',
			fans: 1234
		},
		{
			artist_id: 'artist-merge',
			name: 'Tyler The Creator',
			artist_revision: 3,
			album_count: 1,
			musicbrainz: null,
			merge: {
				artist_id: 'artist-kept',
				name: 'Tyler, The Creator',
				artist_revision: 7,
				musicbrainz_artist_id: 'f6beac20-5dfe-4d1f-ae02-0b0a740aafd6'
			},
			deezer_url: null,
			fans: null
		}
	]
};

describe('Artist suggestions', () => {
	it('shows the MusicBrainz artist to link and the artist to merge into', async () => {
		signIn('admin');
		fakeApi({ 'GET /api/v3/library/artist-suggestions': suggestions });

		await renderWithQuery(ArtistSuggestions, {});

		await expect.element(page.getByText('2 albums · 1,234 fans on Deezer')).toBeVisible();
		await expect.element(page.getByText('(UK singer) on MusicBrainz')).toBeVisible();
		await expect.element(page.getByText('Same artist as')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Use this for Tyler The Creator' }))
			.toBeVisible();
	});

	it("doesn't offer a merge to a curator who can't merge", async () => {
		signIn('trusted');
		fakeApi({ 'GET /api/v3/library/artist-suggestions': suggestions });

		await renderWithQuery(ArtistSuggestions, {});

		await expect
			.element(page.getByRole('button', { name: 'Use this for Poppy Ajudha' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Use this for Tyler The Creator' }))
			.not.toBeInTheDocument();
	});
});
