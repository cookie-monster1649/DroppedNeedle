import { afterEach, describe, expect, it, vi } from 'vitest';

import { DOWN_FOR_MS, listenSourceHealth } from './listenSourceHealth.svelte';

afterEach(() => {
	listenSourceHealth.markUp();
	vi.useRealTimers();
});

describe('listenSourceHealth', () => {
	it('marks ListenBrainz down from an answer, up when it answers, and forgets after a while', () => {
		vi.useFakeTimers();
		listenSourceHealth.observe({ source: 'lastfm', unavailable_sources: ['listenbrainz'] });
		expect(listenSourceHealth.unavailable).toEqual(['listenbrainz']);
		listenSourceHealth.observe({ source: 'lastfm', unavailable_sources: [] });
		expect(listenSourceHealth.unavailable).toEqual(['listenbrainz']);
		listenSourceHealth.observe({ source: 'listenbrainz', unavailable_sources: [] });
		expect(listenSourceHealth.unavailable).toEqual([]);
		listenSourceHealth.markDown();
		vi.advanceTimersByTime(DOWN_FOR_MS + 50);
		expect(listenSourceHealth.unavailable).toEqual([]);
	});
});
