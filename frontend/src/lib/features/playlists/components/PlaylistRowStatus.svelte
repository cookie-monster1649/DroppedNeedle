<script lang="ts">
	// Fork: a playlist song the library doesn't play, shown the way the album
	// page shows a missing track (AlbumTrackList's edition badges, its "held"
	// button and TrackRequestButton's download icon), so the two read alike.
	import { Download, TriangleAlert } from 'lucide-svelte';

	import type { PlaylistRowState } from '$lib/api/playlists';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		status: PlaylistRowState;
		title: string;
		onrequest?: () => Promise<void>;
	}

	let { status, title, onrequest }: Props = $props();

	let asking = $state(false);
	let asked = $state(false);

	async function request() {
		if (!onrequest || asking) return;
		asking = true;
		try {
			await onrequest();
			asked = true;
		} finally {
			asking = false;
		}
	}
</script>

<span class="inline-flex shrink-0 items-center gap-1">
	{#if status === 'requested' || status === 'fetching' || status === 'imported'}
		<span class="badge badge-sm badge-info badge-outline">Fetching</span>
	{:else if status === 'failed'}
		<span class="badge badge-sm badge-error badge-outline">Failed</span>
	{:else if status === 'held'}
		<Button
			variant="ghost"
			size="xs"
			href={withBasePath('/downloads')}
			title="A downloaded file waits in To decide"
		>
			<TriangleAlert class="size-3.5 text-warning" aria-hidden="true" /> held
		</Button>
	{:else}
		<span class="badge badge-sm badge-warning badge-outline">Missing</span>
	{/if}
	{#if onrequest && (status === 'missing' || status === 'failed')}
		<IconButton
			size="xs"
			icon={Download}
			label="Request {title}"
			loading={asking}
			disabled={asked}
			onclick={() => void request()}
		/>
	{/if}
</span>
