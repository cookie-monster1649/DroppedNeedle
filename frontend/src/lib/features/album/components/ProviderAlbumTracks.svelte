<!--
	Fork: an album MusicBrainz doesn't know, as the catalogue it was matched
	to (Deezer) has it: its label, release date, type and genres, and its
	whole track list. Tracks the library holds play from here; tracks it
	lacks are marked Missing, with Want to ask for them by name. An admin
	can write Deezer's names into the files ("Use Deezer's title and
	artist"), previewed, written and undone like an edition's tags.
-->
<script lang="ts">
	import { ExternalLink, Play } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import TagWritePreview from '$lib/features/library/components/edition/TagWritePreview.svelte';
	import PaperPanel from '$lib/ui/PaperPanel.svelte';
	import { formatDurationSec } from '$lib/utils/formatting';
	import { toastStore } from '$lib/stores/toast';
	import { createWantTrackMutation } from '$lib/features/album/queries/AlbumMutations.svelte';
	import type { AlbumDetailView } from '$lib/features/library/queries/types';

	type ProviderMatch = NonNullable<AlbumDetailView['provider_match']>;
	type ProviderTrack = ProviderMatch['tracks'][number];

	interface Props {
		match: ProviderMatch;
		/** Play the album's file `trackId`. */
		onplay: (trackId: string) => void;
		/** The album, when the viewer may write Deezer's names into its files. */
		retag?: { userId: string | null | undefined; localId: string } | null;
	}

	let { match, onplay, retag = null }: Props = $props();

	let retagging = $state(false);
	const RETAG_SENTENCE =
		"Writes Deezer's album, album artist, artist, title, track number, date, ISRC and label into this album's files, and no MusicBrainz ids. The album takes the new names once its folder is scanned, which starts by itself.";

	const want = createWantTrackMutation();
	let asking = $state<string | null>(null);
	let wanted = $state<Set<string>>(new Set());

	const held = $derived(match.tracks.filter((track) => track.track_id).length);
	const multiDisc = $derived(match.tracks.some((track) => track.disc > 1));
	const TYPES: Record<string, string> = {
		album: 'Album',
		ep: 'EP',
		single: 'Single',
		compile: 'Compilation'
	};
	const facts = $derived(
		[
			match.record_type ? (TYPES[match.record_type] ?? match.record_type) : null,
			match.release_date,
			match.label,
			`${held} of ${match.tracks.length} tracks in your library`
		].filter(Boolean)
	);

	async function ask(track: ProviderTrack): Promise<void> {
		asking = track.provider_track_id;
		try {
			await want.mutateAsync({
				artistName: match.artist_name,
				trackTitle: track.title,
				albumTitle: match.title,
				durationSeconds: track.duration_seconds ?? null,
				isrc: track.isrc ?? null
			});
			wanted = new Set([...wanted, track.provider_track_id]);
			toastStore.show({ message: `Asked for ${track.title}.`, type: 'success' });
		} catch {
			toastStore.show({ message: `Couldn't ask for ${track.title}.`, type: 'error' });
		} finally {
			asking = null;
		}
	}
</script>

<section class="space-y-3" aria-labelledby="provider-tracks-title">
	<div class="flex flex-wrap items-end justify-between gap-2">
		<div>
			<h2 id="provider-tracks-title" class="text-2xl text-base-content sm:text-3xl">
				The full album
			</h2>
			<p class="text-sm text-muted">From Deezer · {facts.join(' · ')}</p>
		</div>
		<Button
			variant="outline"
			size="sm"
			icon={ExternalLink}
			iconPosition="end"
			href={match.url}
			target="_blank"
			rel="noreferrer">Open on Deezer</Button
		>
	</div>
	{#if retag}
		{#if retagging}
			<div class="rounded-box border border-base-content/10 bg-base-100 p-3">
				<TagWritePreview
					album={retag}
					edition={{ releaseMbid: `deezer:${match.provider_album_id}`, identityRevision: 0 }}
					placing={false}
					placingSentenceId="provider-tags-sentence"
					sentence={RETAG_SENTENCE}
				/>
			</div>
		{:else}
			<Button variant="outline" size="sm" onclick={() => (retagging = true)}
				>Use Deezer's title and artist</Button
			>
		{/if}
	{/if}
	{#if match.genres.length}
		<ul class="flex flex-wrap gap-1.5" aria-label="Album genres">
			{#each match.genres as genre (genre)}
				<li class="badge badge-ghost badge-sm">{genre}</li>
			{/each}
		</ul>
	{/if}
	<PaperPanel>
		<ol class="divide-y divide-paper-border" aria-label="Deezer track list">
			{#each match.tracks as track (track.provider_track_id)}
				<li
					class="flex items-center gap-3 px-4 py-2.5"
					class:opacity-60={!track.track_id}
					data-held={track.track_id ? 'yes' : 'no'}
				>
					<span class="w-8 shrink-0 text-right text-sm text-muted tabular-nums">
						{multiDisc ? `${track.disc}.${track.position}` : track.position}
					</span>
					<span class="min-w-0 flex-1 truncate">{track.title}</span>
					<span class="shrink-0 text-sm text-muted tabular-nums">
						{formatDurationSec(track.duration_seconds)}
					</span>
					<span class="flex w-40 shrink-0 items-center justify-end">
						{#if track.track_id}
							<Button
								variant="ghost"
								size="sm"
								icon={Play}
								aria-label={`Play ${track.title}`}
								onclick={() => track.track_id && onplay(track.track_id)}
							/>
						{:else if wanted.has(track.provider_track_id)}
							<span class="badge badge-sm badge-info">Wanted</span>
						{:else}
							<span class="mr-2 badge badge-ghost badge-sm">Missing</span>
							<Button
								variant="outline"
								size="sm"
								loading={asking === track.provider_track_id}
								disabled={asking !== null}
								aria-label={`Want ${track.title}`}
								onclick={() => void ask(track)}>Want</Button
							>
						{/if}
					</span>
				</li>
			{/each}
		</ol>
	</PaperPanel>
</section>
