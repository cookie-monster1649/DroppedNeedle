<script lang="ts">
	// Fork: the Match dialog's songs from your Spotify playlists, in the same
	// rows as its albums. Picking one imports the files tagged from Spotify.
	import SpotifyIcon from '$lib/media/icons/SpotifyIcon.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import { formatDurationSec } from '$lib/utils/formatting';
	import {
		createMatchSpotifyHeldMutation,
		getSpotifyCandidatesQuery,
		type SpotifyCandidate
	} from '$lib/features/downloads/queries/SpotifyMatchQueries.svelte';

	interface Props {
		entryId: number;
		/** The search typed; empty asks for the held file's own title. */
		search: string;
		releaseGroupMbid?: string | null;
		busy?: boolean;
		onmatched: () => void;
	}
	let { entryId, search, releaseGroupMbid = null, busy = false, onmatched }: Props = $props();

	const candidates = getSpotifyCandidatesQuery(
		() => entryId,
		() => search
	);
	const match = createMatchSpotifyHeldMutation();
	const songs = $derived(candidates.data?.candidates ?? []);

	async function pick(song: SpotifyCandidate) {
		if (match.isPending || busy) return;
		try {
			await match.mutateAsync({ entryId, candidateId: song.entry_id, releaseGroupMbid });
			onmatched();
		} catch {
			// the mutation toasted; the dialog stays open to retry
		}
	}
</script>

{#if songs.length > 0}
	<div class="mt-4">
		<p class="flex items-center gap-1.5 px-2 text-xs font-semibold text-muted">
			<SpotifyIcon class="size-3.5" /> From your Spotify playlists
		</p>
		<div class="mt-1 max-h-60 space-y-1 overflow-y-auto">
			{#each songs as song (song.entry_id)}
				<button
					class="flex w-full items-center gap-3 rounded-lg p-2 text-left transition-colors hover:bg-base-200 disabled:opacity-50"
					onclick={() => pick(song)}
					disabled={match.isPending || busy}
				>
					<AlbumImage
						customUrl={song.cover_url ?? null}
						size="xs"
						rounded="sm"
						className="h-10 w-10 shrink-0"
						alt=""
					/>
					<div class="min-w-0 flex-1">
						<p class="truncate text-sm font-medium">{song.title}</p>
						<p class="truncate text-xs text-muted">
							{song.artist || 'Unknown artist'}{song.album ? ` · ${song.album}` : ''}
						</p>
					</div>
					{#if song.duration_seconds}
						<span class="shrink-0 text-xs text-muted tabular-nums"
							>{formatDurationSec(song.duration_seconds)}</span
						>
					{/if}
				</button>
			{/each}
		</div>
	</div>
{/if}
{#if match.isPending}
	<p class="mt-3 text-sm text-muted">
		<span class="loading loading-spinner loading-xs"></span>
		Importing as the Spotify song…
	</p>
{/if}
