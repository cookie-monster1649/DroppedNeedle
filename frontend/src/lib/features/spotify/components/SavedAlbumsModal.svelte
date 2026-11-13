<!--
	Fork: review the albums saved at Spotify and request them. Looks like the
	discography download, larger. Every album that can be asked for starts
	selected; one not found here, already held or already requested cannot be
	selected. Albums saved since the last review are marked new and are never
	requested unless they are confirmed here. Confirming requests every album
	still selected (one album request each, in batches of at most 500) and
	marks the review done.
-->
<script lang="ts">
	import {
		Download,
		Disc3,
		Check,
		LoaderCircle,
		SearchX,
		Sparkles,
		RefreshCw
	} from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { createRequestBatchMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import {
		getSavedAlbumsQuery,
		spotifyLibraryActions,
		type SpotifySavedAlbum
	} from '../queries/library.svelte';
	import {
		initialSelection,
		isSelectable,
		requestBatches,
		selectAll,
		toggle
	} from '../state/savedAlbumSelection';

	interface Props {
		open: boolean;
		onclose: () => void;
	}

	let { open = $bindable(), onclose }: Props = $props();

	const albumsQuery = getSavedAlbumsQuery(() => open);
	const actions = spotifyLibraryActions();
	const batchRequest = createRequestBatchMutation();

	let selection = $state(new Set<string>());
	let seeded = $state(false);
	let submitting = $state(false);
	let scanStarted = $state(false);

	const albums = $derived<SpotifySavedAlbum[]>(albumsQuery.data?.albums ?? []);
	const scanning = $derived(albumsQuery.data?.scanning ?? false);
	const notFound = $derived(albums.filter((a) => !a.release_group).length);
	const held = $derived(albums.filter((a) => a.held).length);
	const requested = $derived(albums.filter((a) => a.requested && !a.held).length);
	const fresh = $derived(albums.filter((a) => a.is_new).length);
	const selectable = $derived(albums.filter(isSelectable).length);

	// Read the saved albums once when the review opens.
	$effect(() => {
		if (!open) {
			seeded = false;
			scanStarted = false;
			return;
		}
		if (!scanStarted) {
			scanStarted = true;
			void actions.scanAlbums().catch(() => undefined);
		}
	});

	// Select everything once the list is in, and again after a scan ends.
	$effect(() => {
		if (scanning) seeded = false;
	});
	$effect(() => {
		if (!open || !albumsQuery.data || scanning || seeded) return;
		selection = initialSelection(albums);
		seeded = true;
	});

	function close() {
		open = false;
		onclose();
	}

	async function submitReview() {
		const batches = requestBatches(albums, selection);
		submitting = true;
		let asked = 0;
		let failed = false;
		for (const items of batches) {
			const result = await batchRequest.mutateAsync({ items }).catch(() => null);
			if (!result?.success) {
				failed = true;
				break;
			}
			asked += result.requested;
		}
		if (!failed) await actions.markReviewed().catch(() => undefined);
		submitting = false;
		toastStore.show({
			message: failed
				? `Requested ${asked} albums; the rest could not be submitted`
				: asked === 0
					? 'Review saved; nothing to request'
					: `Requested ${asked} album${asked === 1 ? '' : 's'}`,
			type: failed ? 'error' : 'success'
		});
		if (!failed) close();
	}
</script>

{#if open}
	<Modal
		open
		title="Your saved albums"
		description="Albums you saved on Spotify. Choose the ones to request."
		icon={Disc3}
		size="xl"
		closeButton
		onclose={close}
	>
		<div class="mb-4 flex flex-wrap gap-2">
			<div class="badge badge-ghost gap-1">
				<Disc3 class="h-3 w-3" />
				{albums.length} saved
			</div>
			{#if fresh > 0}
				<div class="badge badge-accent gap-1">
					<Sparkles class="h-3 w-3" />
					{fresh} new
				</div>
			{/if}
			{#if held > 0}
				<div class="badge badge-success gap-1">
					<Check class="h-3 w-3" />
					{held} in library
				</div>
			{/if}
			{#if requested > 0}
				<div class="badge badge-info gap-1">
					<LoaderCircle class="h-3 w-3" />
					{requested} requested
				</div>
			{/if}
			{#if notFound > 0}
				<div class="badge badge-warning gap-1">
					<SearchX class="h-3 w-3" />
					{notFound} not found
				</div>
			{/if}
			{#if scanning}
				<div class="badge badge-ghost gap-1" role="status">
					<RefreshCw class="h-3 w-3 animate-spin motion-keep" />
					Reading from Spotify…
				</div>
			{/if}
		</div>

		{#if albumsQuery.isLoading || (scanning && albums.length === 0)}
			<Card padding="md" class="mb-4 text-center">
				<LoaderCircle class="mx-auto h-8 w-8 animate-spin motion-keep text-base-content/40" />
				<p class="text-sm text-muted">Matching your saved albums. Large libraries take a while.</p>
			</Card>
		{:else if albums.length === 0}
			<Card padding="md" class="mb-4 text-center">
				<p class="text-sm text-muted">No saved albums on Spotify.</p>
			</Card>
		{:else}
			<div class="mb-2 flex items-center justify-between gap-2">
				<p class="text-xs font-medium tracking-wider text-muted uppercase">
					{selection.size} of {selectable} selected
				</p>
				<div class="flex gap-1">
					<Button size="xs" variant="ghost" onclick={() => (selection = selectAll(albums, true))}>
						Select all
					</Button>
					<Button size="xs" variant="ghost" onclick={() => (selection = selectAll(albums, false))}>
						Select none
					</Button>
				</div>
			</div>
			<ul
				class="grid max-h-[55vh] grid-cols-[repeat(auto-fill,minmax(8.5rem,1fr))] gap-3 overflow-y-auto pr-1"
				aria-label="Saved albums"
			>
				{#each albums as album (album.spotify_album_id)}
					{@const enabled = isSelectable(album)}
					{@const checked = selection.has(album.spotify_album_id)}
					<li>
						<label
							class="sa-tile {enabled ? 'cursor-pointer' : 'is-disabled'}"
							class:is-selected={checked}
							title={album.release_group ? album.title : 'Not found on MusicBrainz or Deezer'}
						>
							<div class="relative aspect-square overflow-hidden rounded-lg">
								<AlbumImage
									mbid={album.release_group ?? undefined}
									customUrl={album.cover_url ?? null}
									alt={album.title}
									size="full"
									requestSize={250}
									rounded="lg"
									className="h-full w-full"
								/>
								<input
									type="checkbox"
									class="checkbox checkbox-sm absolute top-1.5 left-1.5 bg-base-100"
									{checked}
									disabled={!enabled}
									aria-label="Request {album.title}"
									onchange={(e) => (selection = toggle(selection, album, e.currentTarget.checked))}
								/>
								{#if album.is_new}
									<span class="badge badge-accent badge-xs absolute top-1.5 right-1.5">new</span>
								{/if}
							</div>
							<span class="mt-1 block truncate text-sm font-medium">{album.title}</span>
							<span class="block truncate text-xs text-muted">
								{album.artist_name}{album.year ? ` · ${album.year}` : ''}
							</span>
							<span class="block text-xs">
								{#if !album.release_group}
									<span class="text-warning">Not found</span>
								{:else if album.held}
									<span class="text-success">In library</span>
								{:else if album.requested}
									<span class="text-info">Requested</span>
								{/if}
							</span>
						</label>
					</li>
				{/each}
			</ul>
		{/if}

		{#snippet actions()}
			<Button variant="ghost" onclick={close} disabled={submitting}>Cancel</Button>
			<Button
				variant="accent"
				icon={Download}
				loading={submitting}
				disabled={scanning || albums.length === 0}
				onclick={() => void submitReview()}
			>
				{#if submitting}
					Requesting...
				{:else if selection.size === 0}
					Done
				{:else}
					Request {selection.size} Album{selection.size !== 1 ? 's' : ''}
				{/if}
			</Button>
		{/snippet}
	</Modal>
{/if}

<style>
	.sa-tile {
		display: block;
		border-radius: 0.75rem;
		padding: 0.25rem;
		outline: 2px solid transparent;
		transition: outline-color 120ms;
	}
	.sa-tile.is-selected {
		outline-color: var(--color-accent);
	}
	.sa-tile.is-disabled {
		opacity: 0.5;
	}
</style>
