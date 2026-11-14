<!--
	Fork: "Your Spotify library" on the Profile Spotify card. Followed artists
	are followed here (never auto-download, nothing downloaded); saved albums
	are reviewed and requested in the modal. Both can follow Spotify every
	15 minutes, with the playlist sync.
-->
<script lang="ts">
	import { RefreshCw, TriangleAlert, Users, Disc3 } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { formatRelativeTime } from '$lib/utils/formatting';
	import SavedAlbumsModal from './SavedAlbumsModal.svelte';
	import { getSpotifyLibraryQuery, spotifyLibraryActions } from '../queries/library.svelte';

	interface Props {
		onreconnect: () => void;
		reconnecting?: boolean;
	}

	let { onreconnect, reconnecting = false }: Props = $props();

	const libraryQuery = getSpotifyLibraryQuery(() => true);
	const actions = spotifyLibraryActions();
	const lib = $derived(libraryQuery.data);
	let reviewOpen = $state(false);
	let saving = $state(false);
	// Fork: one "Sync now" a minute from here; the server also runs a part
	// at most once in five minutes and never during a Spotify rate limit.
	const SYNC_NOW_HOLD_MS = 60_000;
	let syncHeld = $state(false);
	const rateLimited = $derived(Boolean(lib?.last_error?.includes('rate limit')));

	const count = (value: number | null | undefined) =>
		value === null || value === undefined ? '' : ` (${value})`;

	async function setToggles(followSync: boolean, albumSync: boolean) {
		saving = true;
		try {
			await actions.set(followSync, albumSync);
		} catch {
			toastStore.show({ message: 'Could not save the Spotify library sync', type: 'error' });
		} finally {
			saving = false;
		}
	}

	async function syncArtists() {
		if (syncHeld) return;
		syncHeld = true;
		setTimeout(() => (syncHeld = false), SYNC_NOW_HOLD_MS);
		try {
			await actions.syncArtists();
			toastStore.show({
				message: 'Following your Spotify artists in the background',
				type: 'success'
			});
		} catch {
			toastStore.show({ message: 'Could not start the sync', type: 'error' });
		}
	}
</script>

<section class="mt-4" aria-labelledby="sp-library-title">
	<h3 id="sp-library-title" class="mb-2 text-sm font-semibold">Your Spotify library</h3>
	{#if libraryQuery.isLoading}
		<div class="skeleton h-16 w-full"></div>
	{:else if lib?.needs_reconnect}
		<div class="alert alert-warning text-sm" role="alert">
			<TriangleAlert class="size-4" aria-hidden="true" />
			<span>Reconnect to enable Liked Songs, saved albums and followed artists.</span>
			<Button size="sm" variant="spotify" loading={reconnecting} onclick={onreconnect}>
				Reconnect
			</Button>
		</div>
	{:else if lib}
		<div class="pf-box divide-y divide-base-300">
			<div class="flex flex-wrap items-center gap-3 px-4 py-3">
				<Users class="size-5 text-muted" aria-hidden="true" />
				<div class="min-w-0 flex-1">
					<p class="font-medium">Followed artists{count(lib.followed_artists)}</p>
					<p class="pf-meta">
						Followed here without auto-download.
						{#if lib.artists_syncing}
							Syncing…
						{:else if lib.artists_checked_at}
							Synced {formatRelativeTime(new Date(lib.artists_checked_at))}.
						{/if}
					</p>
				</div>
				<label class="label cursor-pointer gap-2 p-0 text-sm">
					<input
						type="checkbox"
						class="toggle toggle-sm toggle-primary"
						checked={lib.follow_sync}
						disabled={saving}
						onchange={(e) => void setToggles(e.currentTarget.checked, lib.album_sync)}
					/>
					Sync
				</label>
				<Button
					size="sm"
					variant="outline"
					icon={RefreshCw}
					loading={lib.artists_syncing}
					disabled={syncHeld}
					onclick={() => void syncArtists()}
				>
					Sync now
				</Button>
			</div>
			<div class="flex flex-wrap items-center gap-3 px-4 py-3">
				<Disc3 class="size-5 text-muted" aria-hidden="true" />
				<div class="min-w-0 flex-1">
					<p class="font-medium">
						Saved albums{count(lib.saved_albums)}
						{#if lib.new_albums > 0}
							<span class="badge badge-accent badge-sm ml-1">{lib.new_albums} new</span>
						{/if}
					</p>
					<p class="pf-meta">
						Nothing is requested until you confirm the review.
						{#if lib.albums_scanning}
							Reading…
						{:else if lib.albums_checked_at}
							Checked {formatRelativeTime(new Date(lib.albums_checked_at))}.
						{/if}
					</p>
				</div>
				<label class="label cursor-pointer gap-2 p-0 text-sm">
					<input
						type="checkbox"
						class="toggle toggle-sm toggle-primary"
						checked={lib.album_sync}
						disabled={saving}
						onchange={(e) => void setToggles(lib.follow_sync, e.currentTarget.checked)}
					/>
					Sync
				</label>
				<Button size="sm" variant="primary" onclick={() => (reviewOpen = true)}>
					Review &amp; request…
				</Button>
			</div>
		</div>
		{#if lib.last_error && rateLimited}
			<p class="mt-2 text-sm text-warning">Sync paused. {lib.last_error}.</p>
		{:else if lib.last_error}
			<p class="mt-2 text-sm text-error">Last sync failed: {lib.last_error}</p>
		{/if}
	{:else if libraryQuery.isError}
		<p class="text-sm text-error">Could not read your Spotify library.</p>
	{/if}
</section>

<SavedAlbumsModal bind:open={reviewOpen} onclose={() => void libraryQuery.refetch()} />
