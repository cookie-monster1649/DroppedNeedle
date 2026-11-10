<!--
	Fork: Check albums' Artists list. Artists the library holds with no
	MusicBrainz link, each with what would give them one: another artist in
	the library that is plainly the same, to merge into (admins), or the
	MusicBrainz artist of the same name, to link (curators). "Use this"
	goes through the artist merge or the MusicBrainz link the artist page
	uses; "Not it" stops the suggestion coming back.
-->
<script lang="ts">
	import { GitMerge, Link } from 'lucide-svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		createDeclineArtistSuggestionMutation,
		createTakeArtistSuggestionMutation,
		getArtistSuggestionsQuery,
		type ArtistSuggestion
	} from '$lib/features/library/queries/EditionQueries.svelte';
	import { count, failureSentence } from './edition/editionCopy';

	const query = getArtistSuggestionsQuery(() => authStore.user?.id);
	const items = $derived(query.data?.items ?? []);
	const take = createTakeArtistSuggestionMutation();
	const decline = createDeclineArtistSuggestionMutation();

	let working = $state<string | null>(null);

	// A merge is an admin's to make; a link, any curator's.
	const canTake = (item: ArtistSuggestion) => (item.merge ? authStore.isAdmin : true);

	function fans(item: ArtistSuggestion): string {
		if (!item.fans) return '';
		return ` · ${item.fans.toLocaleString()} fans on Deezer`;
	}

	async function act(item: ArtistSuggestion, accept: boolean): Promise<void> {
		working = item.artist_id;
		try {
			if (accept) {
				await take.mutateAsync(item);
				toastStore.show({
					message: item.merge
						? `Merged ${item.name} into ${item.merge.name}.`
						: `Linked ${item.name} to MusicBrainz.`,
					type: 'success'
				});
			} else {
				await decline.mutateAsync(item);
			}
		} catch (error) {
			toastStore.show({
				message: failureSentence(error, `${item.name} couldn't be changed. Open it to see why.`),
				type: 'error'
			});
		} finally {
			working = null;
			void query.refetch();
		}
	}
</script>

<div class="space-y-3">
	<QueryBoundary {query} what="the list" isEmpty={() => items.length === 0}>
		{#snippet loading()}
			<div class="space-y-2">
				{#each Array(4) as _, index (index)}<div class="skeleton h-20 w-full"></div>{/each}
			</div>
		{/snippet}
		{#snippet empty()}
			<Card class="text-center text-muted">Nothing to suggest for your artists right now.</Card>
		{/snippet}
		<p class="text-sm text-muted">{count(items.length, 'artist', 'artists')}</p>
		<ul class="space-y-2">
			{#each items as item (item.artist_id)}
				<li
					class="flex flex-wrap items-center gap-3 rounded-box border border-base-content/10 bg-base-100 p-3"
					data-artist={item.artist_id}
				>
					<ArtistImage
						mbid=""
						localId={item.artist_id}
						alt=""
						size="sm"
						className="h-12 w-12 shrink-0"
					/>
					<div class="min-w-0 flex-1">
						<a
							class="block truncate font-medium hover:underline"
							href={withBasePath(`/library/artists/${encodeURIComponent(item.artist_id)}`)}
							>{item.name}</a
						>
						<p class="text-xs text-muted">
							{count(item.album_count, 'album', 'albums')}{fans(item)}
						</p>
						{#if item.merge}
							<p class="flex items-center gap-1.5 text-sm">
								<GitMerge class="h-3.5 w-3.5 text-warning" aria-hidden="true" />
								Same artist as
								<a
									class="font-medium hover:underline"
									href={withBasePath(
										`/library/artists/${encodeURIComponent(item.merge.artist_id)}`
									)}>{item.merge.name}</a
								>{item.merge.musicbrainz_artist_id ? ' · Matched on MusicBrainz' : ''}
							</p>
						{:else if item.musicbrainz}
							<p class="flex items-center gap-1.5 text-sm">
								<Link class="h-3.5 w-3.5 text-info" aria-hidden="true" />
								<span>
									Best guess ·
									<a
										class="font-medium hover:underline"
										href={`https://musicbrainz.org/artist/${item.musicbrainz.musicbrainz_artist_id}`}
										target="_blank"
										rel="noopener noreferrer">{item.musicbrainz.name}</a
									>{item.musicbrainz.disambiguation ? ` (${item.musicbrainz.disambiguation})` : ''} on
									MusicBrainz
								</span>
							</p>
						{/if}
					</div>
					<div class="flex gap-2">
						{#if canTake(item)}
							<Button
								variant="primary"
								size="sm"
								loading={working === item.artist_id}
								disabled={working !== null}
								aria-label={`Use this for ${item.name}`}
								onclick={() => void act(item, true)}>Use this</Button
							>
						{/if}
						<Button
							variant="ghost"
							size="sm"
							disabled={working !== null}
							aria-label={`Not ${item.name}`}
							onclick={() => void act(item, false)}>Not it</Button
						>
					</div>
				</li>
			{/each}
		</ul>
	</QueryBoundary>
</div>
