import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { listenSourceHealth } from '$lib/stores/listenSourceHealth.svelte';

import SimpleSourceSwitcher from './SimpleSourceSwitcher.svelte';

const linked = {
	'GET /api/v3/me/connections': {
		connections: [
			{ service: 'listenbrainz', enabled: true, username: 'jj' },
			{ service: 'lastfm', enabled: true, username: 'jj' }
		]
	}
};

afterEach(() => listenSourceHealth.markUp());

describe('the ListenBrainz / Last.fm switcher', () => {
	it('greys out an unavailable ListenBrainz and moves to Last.fm by itself', async () => {
		fakeApi(linked);
		const changes: string[] = [];
		await renderWithQuery(SimpleSourceSwitcher, {
			currentSource: 'listenbrainz',
			unavailable: ['listenbrainz'],
			onSourceChange: (source: string) => changes.push(source)
		});
		const lb = page.getByRole('button', { name: 'ListenBrainz (unavailable right now)' });
		await expect.element(lb).toBeDisabled();
		await expect.poll(() => changes).toEqual(['lastfm']);
		await expect
			.element(page.getByTitle('ListenBrainz is unavailable right now'))
			.toBeInTheDocument();
	});

	it('reads what the app last heard, and offers ListenBrainz again without moving anyone', async () => {
		fakeApi(linked);
		listenSourceHealth.markDown();
		const changes: string[] = [];
		await renderWithQuery(SimpleSourceSwitcher, {
			currentSource: 'lastfm',
			onSourceChange: (source: string) => changes.push(source)
		});
		await expect
			.element(page.getByRole('button', { name: 'ListenBrainz (unavailable right now)' }))
			.toBeDisabled();
		listenSourceHealth.markUp();
		await expect.element(page.getByRole('button', { name: 'ListenBrainz' })).toBeEnabled();
		expect(changes).toEqual([]);
	});
});
