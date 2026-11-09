// Fork: a playlist's Spotify sync, shared by the header's "..." menu, the
// status line and the prompt after an import. Sync (mirror the playlist)
// and requesting new songs are separate choices, so other sources can sync
// the same way later.
import { fetchSpotifySync, setSpotifySync, type SpotifySyncState } from '$lib/api/playlists';
import { toastStore } from '$lib/stores/toast';

export class PlaylistSpotifySync {
	state = $state<SpotifySyncState | null>(null);
	saving = $state(false);
	#id: () => string | null;

	/** `id` returns the playlist to follow, or null when sync doesn't apply. */
	constructor(id: () => string | null) {
		this.#id = id;
		$effect(() => {
			const current = this.#id();
			this.state = null;
			if (!current) return;
			fetchSpotifySync(current)
				.then((state) => {
					if (current === this.#id()) this.state = state;
				})
				.catch(() => {
					// Not offered when it cannot be read.
				});
		});
	}

	async set(enabled: boolean, autoRequest: boolean): Promise<void> {
		const id = this.#id();
		if (!id || this.saving) return;
		this.saving = true;
		try {
			this.state = await setSpotifySync(id, enabled, enabled && autoRequest);
			toastStore.show({
				message: !enabled
					? 'No longer syncing with Spotify.'
					: autoRequest
						? 'Syncing with Spotify; new songs will be requested.'
						: 'Syncing with Spotify.',
				type: 'success'
			});
		} catch {
			toastStore.show({ message: "Couldn't change Spotify sync", type: 'error' });
		} finally {
			this.saving = false;
		}
	}
}
