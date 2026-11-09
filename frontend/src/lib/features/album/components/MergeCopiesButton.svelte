<!--
	Fork: "Merge albums" on the copies card of a MusicBrainz release. Every
	copy folds into the one with the most tracks (the first on a tie), keeping
	its MusicBrainz match, through the same preview and apply as "Merge with
	another local album...". A song held by more than one copy keeps its best
	file: a modal lists them and, once confirmed, the others go to the
	recycle bin before the merge. Album organization is admin-only.
-->
<script lang="ts">
	import { Combine } from 'lucide-svelte';
	import { api } from '$lib/api/client';
	import { v3 } from '$lib/api/v3/endpoint';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { AlbumView, TrackView } from '$lib/features/library/queries/types';
	import {
		createApplyAlbumMembershipMutation,
		createPreviewAlbumMembershipMutation,
		type MembershipPreviewInput
	} from '$lib/features/library/queries/LibraryCatalogMutations.svelte';
	import { pickDuplicates, qualityLabel, type DuplicateSong } from './mergeCopies';

	interface Props {
		copies: AlbumView[];
		onmerged?: (keptId: string) => void;
	}

	let { copies, onmerged }: Props = $props();

	const preview = createPreviewAlbumMembershipMutation('merge');
	const apply = createApplyAlbumMembershipMutation('merge');
	let busy = $state(false);
	let duplicates = $state<DuplicateSong[]>([]);
	let asking = $state(false);
	let pending: { kept: AlbumView; tracks: TrackView[] } | null = null;

	/** The copy the others fold into: the most tracks, the first on a tie. */
	function keeper(list: AlbumView[]): AlbumView {
		return list.reduce((best, copy) => (copy.track_count > best.track_count ? copy : best));
	}

	async function albumTracks(albumId: string): Promise<TrackView[]> {
		const page = await api.v3.GET(
			v3('/api/v3/library/albums/{id}/tracks', {
				path: { id: albumId },
				query: { limit: 500 }
			})
		);
		return page.items;
	}

	async function start(): Promise<void> {
		if (copies.length < 2 || busy) return;
		busy = true;
		try {
			const kept = keeper(copies);
			const tracks = (await Promise.all(copies.map((copy) => albumTracks(copy.id)))).flat();
			const found = pickDuplicates(tracks, kept.id);
			pending = { kept, tracks };
			if (found.length > 0) {
				duplicates = found;
				asking = true;
				busy = false;
				return;
			}
			await merge([]);
		} catch {
			toastStore.show({ message: "The albums couldn't be read", type: 'error' });
			busy = false;
		}
	}

	async function confirmDuplicates(): Promise<void> {
		asking = false;
		busy = true;
		await merge(duplicates.flatMap((song) => song.discard));
	}

	/** Discard `discard`, then fold every other copy's remaining tracks into the keeper. */
	async function merge(discard: TrackView[]): Promise<void> {
		const plan = pending;
		if (!plan) return;
		try {
			for (const track of discard) {
				await api.v3.DELETE(
					v3('/api/v3/library/tracks/{id}', {
						path: { id: track.id },
						query: { expected_revision: track.row_revision }
					})
				);
			}
			const gone = new Set(discard.map((track) => track.id));
			const moving = plan.tracks.filter(
				(track) => track.album_id !== plan.kept.id && !gone.has(track.id)
			);
			if (moving.length > 0) {
				const albums = [plan.kept.id, ...new Set(moving.map((track) => track.album_id))];
				// Fresh revisions: the discards above move them.
				const details = await Promise.all(
					albums.map((id) => api.v3.GET(v3('/api/v3/library/albums/{id}', { path: { id } })))
				);
				const request: MembershipPreviewInput = {
					track_ids: moving.map((track) => track.id),
					expected_album_revisions: Object.fromEntries(
						details.map((detail) => [detail.id, detail.row_revision])
					),
					target_album_id: plan.kept.id,
					identity_choice: 'retain_manual'
				};
				const planned = await preview.mutateAsync({ albumId: plan.kept.id, request });
				await apply.mutateAsync({
					albumId: plan.kept.id,
					request,
					previewToken: planned.preview_token
				});
			} else if (discard.length > 0) {
				toastStore.show({ message: 'Duplicate files removed', type: 'success' });
			}
			onmerged?.(plan.kept.id);
		} catch {
			toastStore.show({
				message: "The albums couldn't be merged; reload the page and try again",
				type: 'error'
			});
			onmerged?.(plan.kept.id);
		} finally {
			busy = false;
			pending = null;
			duplicates = [];
		}
	}
</script>

<Button variant="outline" size="sm" icon={Combine} loading={busy} onclick={start}>
	Merge albums
</Button>

<Modal
	bind:open={asking}
	title="Keep the best copy of each song?"
	description="These songs are in more than one copy. The best file of each stays; the others go to the recycle bin, where you can put them back."
	icon={Combine}
	size="md"
	onclose={() => {
		pending = null;
		duplicates = [];
	}}
>
	<ul class="flex max-h-80 flex-col gap-3 overflow-y-auto text-sm">
		{#each duplicates as song (song.keep.id)}
			<li>
				<p class="font-medium">{song.keep.title}</p>
				<p class="text-success">Keep: {qualityLabel(song.keep)}</p>
				{#each song.discard as track (track.id)}
					<p class="text-base-content/60">Discard: {qualityLabel(track)}</p>
				{/each}
			</li>
		{/each}
	</ul>
	{#snippet actions()}
		<Button
			variant="ghost"
			onclick={() => {
				asking = false;
				pending = null;
				duplicates = [];
			}}>Cancel</Button
		>
		<Button variant="primary" onclick={confirmDuplicates}>Keep best and merge</Button>
	{/snippet}
</Modal>
