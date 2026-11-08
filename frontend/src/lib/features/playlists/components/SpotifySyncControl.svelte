<!--
	Fork: keep a playlist imported from Spotify in step with Spotify.
	Sync re-imports the playlist when it changes on Spotify (checked every
	15 minutes); "Request new songs" asks for the songs added there, which
	join the playlist as they land in the library.
-->
<script lang="ts">
	import { fetchSpotifySync, setSpotifySync, type SpotifySyncState } from '$lib/api/playlists';
	import { toastStore } from '$lib/stores/toast';
	import { formatRelativeTime } from '$lib/utils/formatting';

	interface Props {
		playlistId: string;
	}

	let { playlistId }: Props = $props();

	let sync = $state<SpotifySyncState | null>(null);
	let saving = $state(false);

	$effect(() => {
		const id = playlistId;
		sync = null;
		fetchSpotifySync(id)
			.then((state) => {
				if (id === playlistId) sync = state;
			})
			.catch(() => {
				// Not shown when it cannot be read.
			});
	});

	async function save(enabled: boolean, autoRequest: boolean) {
		saving = true;
		try {
			sync = await setSpotifySync(playlistId, enabled, autoRequest);
			toastStore.show({
				message: enabled
					? autoRequest
						? 'Syncing with Spotify; new songs will be requested.'
						: 'Syncing with Spotify.'
					: 'No longer syncing with Spotify.',
				type: 'success'
			});
		} catch {
			toastStore.show({ message: "Couldn't change Spotify sync", type: 'error' });
		} finally {
			saving = false;
		}
	}
</script>

{#if sync}
	<div class="pl-sync flex flex-wrap items-center gap-x-5 gap-y-2 text-sm">
		<label class="inline-flex cursor-pointer items-center gap-2">
			<input
				type="checkbox"
				class="toggle toggle-sm toggle-primary"
				checked={sync.enabled}
				disabled={saving}
				onchange={(event) =>
					save(event.currentTarget.checked, sync?.auto_request || !sync?.enabled)}
			/>
			<span>Sync with Spotify</span>
		</label>
		{#if sync.enabled}
			<label class="inline-flex cursor-pointer items-center gap-2">
				<input
					type="checkbox"
					class="toggle toggle-sm toggle-primary"
					checked={sync.auto_request}
					disabled={saving}
					onchange={(event) => save(true, event.currentTarget.checked)}
				/>
				<span>Request new songs</span>
			</label>
			<span class="text-base-content/60">
				{#if sync.last_error}
					Last check failed: {sync.last_error}
				{:else if sync.checked_at}
					Checked {formatRelativeTime(new Date(sync.checked_at))}
				{:else}
					First check within 15 minutes
				{/if}
			</span>
		{/if}
	</div>
{/if}
