<!--
	Fork: the status line of a playlist that follows Spotify. Sync itself is
	switched from the header's "..." menu or the prompt after an import.
-->
<script lang="ts">
	import type { PlaylistSpotifySync } from '../state/spotifySync.svelte';
	import { formatRelativeTime } from '$lib/utils/formatting';

	interface Props {
		sync: PlaylistSpotifySync;
	}

	let { sync }: Props = $props();
</script>

{#if sync.state?.enabled}
	<p class="pl-sync text-sm text-base-content/60">
		Syncing with Spotify ·
		{#if sync.state.last_error}
			Last check failed: {sync.state.last_error}
		{:else if sync.state.checked_at}
			Checked {formatRelativeTime(new Date(sync.state.checked_at))}
		{:else}
			First check within 15 minutes
		{/if}
	</p>
{/if}
