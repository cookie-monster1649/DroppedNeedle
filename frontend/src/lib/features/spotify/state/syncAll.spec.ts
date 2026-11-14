import { describe, expect, it } from 'vitest';
import { ApiError } from '$lib/api/errors';
import { SYNC_ALL_HOLD_MS, syncAllRefusal } from './syncAll';

describe('syncAllRefusal', () => {
	it('shows the wait the server gives', () => {
		const error = new ApiError(
			409,
			'Your playlists were synced moments ago. Try again in 4 minutes.',
			'CONFLICT'
		);
		expect(syncAllRefusal(error)).toBe(
			'Your playlists were synced moments ago. Try again in 4 minutes.'
		);
	});

	it('falls back for anything else', () => {
		expect(syncAllRefusal(new ApiError(500, 'boom'))).toBe('Could not start the sync');
		expect(syncAllRefusal(new Error('offline'))).toBe('Could not start the sync');
	});

	it('rests the button for a minute', () => {
		expect(SYNC_ALL_HOLD_MS).toBe(60_000);
	});
});
