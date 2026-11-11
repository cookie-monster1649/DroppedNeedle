import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi, type FakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/state', () => ({
	page: { params: { id: 'local-album-1' }, url: new URL('http://localhost/album/local-album-1') }
}));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('$lib/features/player/state/player.svelte', () => ({
	playerStore: { playQueue: vi.fn(), addToQueue: vi.fn(), playNext: vi.fn() }
}));

import LocalAlbumPage from './LocalAlbumPage.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

const album: RouteData<'GET /api/v3/library/albums/{id}'> = {
	id: 'local-album-1',
	title: 'Local Only Album',
	artist_id: 'local-artist-1',
	artist_name: 'Local Artist',
	identity_state: 'local_only',
	is_compilation: false,
	track_count: 1,
	total_duration_seconds: 181,
	total_size_bytes: 1024,
	format: 'flac',
	year: 2026,
	cover_available: false,
	art_missing: false,
	favorite: false,
	date_added: 1,
	row_revision: 3,
	listing_revision: 1
};

const track: RouteData<'GET /api/v3/library/albums/{id}/tracks'>['items'][number] = {
	id: 'local-track-1',
	title: 'Unmatched Song',
	album_id: album.id,
	row_revision: 1,
	album_title: album.title,
	artist_id: album.artist_id,
	artist_name: album.artist_name,
	album_artist_id: album.artist_id,
	album_artist_name: album.artist_name,
	disc_number: 1,
	track_number: 1,
	year: 2026,
	genre: 'Electronic',
	duration_seconds: 181,
	format: 'flac',
	bit_rate: 900000,
	sample_rate: 48000,
	bit_depth: 24,
	channels: 2,
	file_size_bytes: 1024,
	date_added: 1,
	cover_available: false,
	art_missing: false,
	favorite: false
};

function fakeLibrary(options: { downloadAllowed: boolean }): FakeApi {
	return fakeApi({
		'GET /api/v3/library/albums/{id}': album,
		'GET /api/v3/library/albums/{id}/tracks': { items: [track], total: 1, offset: 0, limit: 100 },
		'GET /api/v3/download/access': { allowed: options.downloadAllowed },
		'GET /api/v3/library/albums/{album_id}/edition': {
			album_id: album.id,
			candidates: [],
			edition_kind: 'none',
			files_by_itself: true,
			keeps_own_tags: false,
			identifying: false,
			identity_revision: 0,
			placing: null,
			reason: { code: 'UNIDENTIFIED', message: 'Not matched yet.', action: '' },
			release_mbid: null,
			release_group_mbid: null,
			state: 'unidentified',
			undo_available: false,
			writing: false,
			disputed_tracks: [],
			looks_wrong: false,
			looks_wrong_count: 0
		},
		'GET /api/v3/library/albums/{album_id}/history': { items: [] },
		'POST /api/v3/ratings/lookup': { albums: {}, artists: {}, tracks: {} }
	});
}

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Someone',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'someone',
			username_display: 'Someone'
		})
	);
});

describe('local album page track menu', () => {
	it('hides the button and omits the menu Download item when restricted', async () => {
		expect.assertions(4);
		fakeLibrary({ downloadAllowed: false });
		await renderWithQuery(LocalAlbumPage, { albumId: album.id });

		await expect.element(page.getByText('Unmatched Song')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Download album/ }))
			.not.toBeInTheDocument();
		await (await page.getByLabelText('More actions').all())[0].click();
		await expect.element(page.getByRole('menuitem', { name: 'Add to Queue' })).toBeVisible();
		expect(page.getByRole('menuitem', { name: 'Download' }).elements()).toHaveLength(0);
	});
});

describe('local album page with a pending review', () => {
	const pending = { ...album, pending_review_id: 'review-1' };
	const guess: RouteData<'GET /api/v3/library/albums/{album_id}/edition'> = {
		album_id: album.id,
		candidates: [],
		edition_kind: 'release',
		files_by_itself: false,
		keeps_own_tags: false,
		identifying: false,
		identity_revision: 2,
		placing: null,
		reason: {
			code: 'WEAK_MATCH',
			message: 'Closest we found, but some tracks differ.',
			action: ''
		},
		release_mbid: 'release-1',
		release_group_mbid: 'group-1',
		state: 'unconfirmed',
		undo_available: false,
		writing: false,
		disputed_tracks: [],
		looks_wrong: false,
		looks_wrong_count: 0
	};
	const signIn = (role: 'admin' | 'user') =>
		authStore.setUser(
			toAuthUser({
				id: 'user-1',
				display_name: 'Someone',
				role,
				email: null,
				avatar_url: null,
				username: 'someone',
				username_display: 'Someone'
			})
		);

	it('shows a user why it is a guess and "Looks wrong", and an admin the review link', async () => {
		for (const role of ['user', 'admin'] as const) {
			signIn(role);
			fakeLibrary({ downloadAllowed: true }).use({
				'GET /api/v3/library/albums/{id}': pending,
				'GET /api/v3/library/albums': { items: [], total: 0, next: null },
				'GET /api/v3/library/albums/{album_id}/removals': { items: [] },
				'GET /api/v3/library/retag-pairs': { items: [], next_cursor: null, total: 0 },
				'GET /api/v3/library/albums/{album_id}/edition': guess,
				'GET /api/v3/library/albums/{album_id}/edition/plan': apiError(404, 'NOT_FOUND', 'No plan')
			});
			const view = await renderWithQuery(LocalAlbumPage, { albumId: album.id });
			const review = page.getByRole('link', { name: 'Needs you: open its review' });
			const looksWrong = page.getByRole('button', { name: 'Looks wrong' });
			if (role === 'user') {
				await expect
					.element(page.getByText('Closest we found, but some tracks differ.'))
					.toBeVisible();
				await expect.element(looksWrong).toBeVisible();
				await expect.element(review).not.toBeInTheDocument();
			} else {
				await expect
					.element(review)
					.toHaveAttribute('href', expect.stringContaining('/library/review?review=review-1'));
				await expect.element(looksWrong).not.toBeInTheDocument();
			}
			view.unmount();
		}
	});
});

describe('local album page for a Deezer-matched album', () => {
	const deezerAlbum = {
		...album,
		title: 'FEMME',
		provider_match: {
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
					title: 'Holy Moly',
					duration_seconds: 180,
					disc: 1,
					position: 1,
					isrc: 'GBUM71900001',
					track_id: null
				},
				{
					provider_track_id: '1002',
					title: 'Unmatched Song',
					duration_seconds: 181,
					disc: 1,
					position: 2,
					isrc: null,
					track_id: track.id
				}
			]
		}
	} as RouteData<'GET /api/v3/library/albums/{id}'>;

	function fakeDeezerLibrary(): FakeApi {
		const api = fakeLibrary({ downloadAllowed: true });
		api.use({
			'GET /api/v3/library/albums/{id}': deezerAlbum,
			'GET /api/v3/library/albums': { items: [], total: 0, next: null },
			'GET /api/v3/library/albums/{album_id}/removals': { items: [] },
			'GET /api/v3/library/retag-pairs': { items: [], next_cursor: null, total: 0 },
			'POST /api/v3/requests/wants': { key: 'want-1', status: 'queued' }
		});
		return api;
	}

	beforeEach(() => {
		authStore.setUser(
			toAuthUser({
				id: 'admin-1',
				display_name: 'Admin',
				role: 'admin',
				email: null,
				avatar_url: null,
				username: 'admin',
				username_display: 'Admin'
			})
		);
	});

	it('lists Deezer’s tracks in order with the missing one inline, and asks for it', async () => {
		const api = fakeDeezerLibrary();

		await renderWithQuery(LocalAlbumPage, { albumId: album.id });

		await expect.element(page.getByText('Holy Moly')).toBeVisible();
		await expect.element(page.getByText('Missing')).toBeVisible();
		await expect.element(page.getByText('EP', { exact: true })).toBeVisible();
		await expect.element(page.getByText('In Library · 1/2')).toBeVisible();
		await expect.element(page.getByText('Own Label').first()).toBeVisible();
		const rows = page.getByRole('listitem').filter({ hasText: /Holy Moly|Unmatched Song/ });
		await expect.element(rows.first()).toHaveTextContent(/Holy Moly/);
		await expect.element(page.getByText('The full album')).not.toBeInTheDocument();

		await page.getByRole('button', { name: 'Request Holy Moly' }).click();
		await expect.element(page.getByText('Wanted')).toBeVisible();
		expect(api.calls('POST /api/v3/requests/wants')[0].body).toMatchObject({
			artist_name: 'Poppy Ajudha',
			track_title: 'Holy Moly',
			album_title: 'FEMME',
			isrc: 'GBUM71900001'
		});
	});

	it('keeps Open on Deezer and the tag write in the album menu', async () => {
		fakeDeezerLibrary();

		await renderWithQuery(LocalAlbumPage, { albumId: album.id });

		await page.getByLabelText('More album actions').click();
		await expect.element(page.getByText('Open on Deezer')).toBeVisible();
		await page.getByText("Use Deezer's title and artist...").click();
		await expect.element(page.getByText(/and no MusicBrainz ids/)).toBeVisible();
	});
});
