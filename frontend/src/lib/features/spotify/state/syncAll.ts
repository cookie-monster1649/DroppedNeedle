/**
 * Fork: "Sync all now" on the Spotify playlists page. The server runs one at
 * most once in five minutes and never while Spotify asks to wait; the
 * button rests for a minute after a press, like the library card's
 * "Sync now".
 */
import { ApiError } from '$lib/api/errors';

/** How long the button stays disabled after a press. */
export const SYNC_ALL_HOLD_MS = 60_000;

/** The toast for a press the server refused or that failed. */
export function syncAllRefusal(error: unknown): string {
	if (error instanceof ApiError && error.status === 409 && error.message) {
		return error.message;
	}
	return 'Could not start the sync';
}
