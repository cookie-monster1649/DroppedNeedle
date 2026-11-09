<!--
	Fork: asked once, right after a playlist is imported from Spotify
	(`?sync=ask`). Two separate choices: sync (mirror the playlist as it
	changes on Spotify) and request songs (ask now for every song the library
	lacks, and later for each song added there).
-->
<script lang="ts">
	import { page } from '$app/state';
	import { RefreshCw } from 'lucide-svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import Button from '$lib/ui/Button.svelte';
	import type { PlaylistSpotifySync } from '../state/spotifySync.svelte';

	interface Props {
		sync: PlaylistSpotifySync;
		onrequestall?: () => Promise<void>;
	}

	let { sync, onrequestall }: Props = $props();

	let answered = $state(false);
	let open = $state(false);
	let syncChosen = $state(true);
	let requestChosen = $state(true);
	let busy = $state(false);

	$effect(() => {
		if (answered || !sync.state) return;
		if (page.url.searchParams.get('sync') !== 'ask') return;
		if (sync.state.enabled) finish();
		else open = true;
	});

	function finish() {
		answered = true;
		open = false;
		const url = new URL(window.location.href);
		url.searchParams.delete('sync');
		history.replaceState(history.state, '', url);
	}

	async function confirm() {
		busy = true;
		try {
			if (syncChosen) await sync.set(true, requestChosen);
		} finally {
			busy = false;
		}
		finish();
		if (requestChosen) void onrequestall?.();
	}
</script>

<Modal
	bind:open
	title="Keep this playlist going?"
	description="Choose what DroppedNeedle does with this playlist from now on."
	icon={RefreshCw}
	size="sm"
	onclose={() => !answered && finish()}
>
	<div class="flex flex-col gap-4 text-sm">
		<label class="flex cursor-pointer items-start gap-3">
			<input
				type="checkbox"
				class="toggle toggle-sm toggle-primary mt-0.5"
				bind:checked={syncChosen}
			/>
			<span>
				<span class="font-medium">Sync with Spotify</span>
				<span class="block text-base-content/60">
					Mirror changes made on Spotify, checked every 15 minutes.
				</span>
			</span>
		</label>
		<label class="flex cursor-pointer items-start gap-3">
			<input
				type="checkbox"
				class="toggle toggle-sm toggle-primary mt-0.5"
				bind:checked={requestChosen}
			/>
			<span>
				<span class="font-medium">Request songs</span>
				<span class="block text-base-content/60">
					Request every song you don't have now{syncChosen
						? ', and each song added on Spotify later'
						: ''}.
				</span>
			</span>
		</label>
		<p class="text-base-content/60">You can change these later from the ··· menu.</p>
	</div>
	{#snippet actions()}
		<Button variant="ghost" onclick={finish} disabled={busy}>Not now</Button>
		<Button
			variant="primary"
			onclick={confirm}
			loading={busy}
			disabled={!syncChosen && !requestChosen}
		>
			Continue
		</Button>
	{/snippet}
</Modal>
