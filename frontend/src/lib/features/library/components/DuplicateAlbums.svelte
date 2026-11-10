<!--
	Fork: Check albums' Duplicates list. Albums the library holds twice (the
	same artist and title): one album split in two, or the same songs kept
	twice. Each group names the album to keep and, per song held more than
	once, the copy that stays. "Tidy up" merges the others into the kept
	album and sends the surplus copies to the recycle bin, through the
	album merge and file removal the album page uses.
-->
<script lang="ts">
	import { Copy, Layers } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		createTidyDuplicateGroupMutation,
		getDuplicateAlbumsQuery,
		type DuplicateGroup
	} from '$lib/features/library/queries/EditionQueries.svelte';
	import { count, failureSentence } from './edition/editionCopy';

	type Album = DuplicateGroup['albums'][number];
	type Track = Album['tracks'][number];

	const query = getDuplicateAlbumsQuery(() => authStore.user?.id);
	const groups = $derived(query.data?.groups ?? []);
	const tidyGroup = createTidyDuplicateGroupMutation();

	let working = $state<string | null>(null);
	let asking = $state<DuplicateGroup | null>(null);

	const groupKey = (group: DuplicateGroup) => group.keep_album_id;

	function quality(track: Track): string {
		const format = (track.file_format ?? '').toUpperCase();
		if (track.bit_depth) return `${format} ${track.bit_depth}-bit`;
		if (track.bit_rate) return `${format} ${Math.round(track.bit_rate / 1000)}k`;
		return format || 'Unknown format';
	}

	function surplus(group: DuplicateGroup, track: Track): boolean {
		return group.surplus_track_ids.includes(track.track_id);
	}

	function summary(group: DuplicateGroup): string {
		const others = group.albums.length - 1;
		const removed = group.surplus_track_ids.length;
		const merge = others > 0 ? `Merge ${count(others, 'album', 'albums')} into the kept album` : '';
		const remove =
			removed > 0
				? `send ${count(removed, 'lesser copy', 'lesser copies')} to the recycle bin`
				: '';
		return [merge, remove].filter(Boolean).join(', then ') + '.';
	}

	async function tidy(group: DuplicateGroup): Promise<void> {
		asking = null;
		const keep = group.albums[0];
		working = groupKey(group);
		try {
			await tidyGroup.mutateAsync(group);
			toastStore.show({ message: `Tidied ${keep.title}.`, type: 'success' });
		} catch (error) {
			toastStore.show({
				message: failureSentence(error, `${keep.title} couldn't be tidied. Open it to see why.`),
				type: 'error'
			});
		} finally {
			working = null;
			void query.refetch();
		}
	}
</script>

<div class="space-y-3">
	<QueryBoundary {query} what="the list" isEmpty={() => groups.length === 0}>
		{#snippet loading()}
			<div class="space-y-2">
				{#each Array(4) as _, index (index)}<div class="skeleton h-32 w-full"></div>{/each}
			</div>
		{/snippet}
		{#snippet empty()}
			<Card class="text-center text-muted">No album is in your library twice.</Card>
		{/snippet}
		<p class="text-sm text-muted">{count(groups.length, 'group', 'groups')}</p>
		<ul class="space-y-2">
			{#each groups as group (groupKey(group))}
				<li
					class="space-y-2 rounded-box border border-base-content/10 bg-base-100 p-3"
					data-group={group.keep_album_id}
				>
					<div class="flex flex-wrap items-center justify-between gap-2">
						<p
							class="flex items-center gap-1.5 text-xs font-semibold tracking-wide text-warning uppercase"
						>
							{#if group.kind === 'same_album'}
								<Layers class="h-3.5 w-3.5" aria-hidden="true" />One album split in two
							{:else}
								<Copy class="h-3.5 w-3.5" aria-hidden="true" />Same songs kept twice
							{/if}
						</p>
						{#if authStore.isAdmin}
							<Button
								variant="primary"
								size="sm"
								loading={working === groupKey(group)}
								disabled={working !== null}
								aria-label={`Tidy up ${group.albums[0].title}`}
								onclick={() => (asking = group)}>Tidy up</Button
							>
						{/if}
					</div>
					<ul class="grid gap-2 lg:grid-cols-2">
						{#each group.albums as album, index (album.album_id)}
							<li class="flex min-w-0 items-start gap-3">
								<AlbumImage
									albumId={album.album_id}
									alt=""
									size="sm"
									rounded="md"
									className="h-12 w-12 shrink-0 border border-base-content/10"
								/>
								<div class="min-w-0">
									<p class="text-xs text-muted">
										{index === 0 ? 'Keep' : 'Merge into it'}{album.release_group_mbid
											? ' · Matched on MusicBrainz'
											: ''}
									</p>
									<a
										class="block truncate font-medium hover:underline"
										href={withBasePath(`/album/${encodeURIComponent(album.album_id)}`)}
										>{album.title}</a
									>
									<ul class="text-xs text-muted">
										{#each album.tracks as track (track.track_id)}
											<li class:line-through={surplus(group, track)}>
												{track.title} · {quality(track)}{surplus(group, track)
													? ' · lesser copy'
													: ''}
											</li>
										{/each}
									</ul>
								</div>
							</li>
						{/each}
					</ul>
				</li>
			{/each}
		</ul>
	</QueryBoundary>
</div>

<ConfirmDialog
	open={asking !== null}
	title={asking ? `Tidy up ${asking.albums[0].title}?` : ''}
	message={asking ? summary(asking) : ''}
	confirmLabel="Tidy up"
	onconfirm={() => asking && void tidy(asking)}
	oncancel={() => (asking = null)}
/>
