<script module lang="ts">
	import type { TrackView as SlotTrack } from '$lib/features/library/queries/types';

	/**
	 * Fork: one row of a full track list (a Deezer-matched album's): a file
	 * the library holds, or a track it lacks, shown inline as Missing.
	 */
	export type TrackSlot =
		| { key: string; number: string; track: SlotTrack }
		| {
				key: string;
				number: string;
				missing: {
					title: string;
					duration_seconds: number | null;
					wanted: boolean;
					pending: boolean;
					/** Ask for the track; none shows only the Missing mark. */
					onwant?: () => void;
				};
		  };
</script>

<script lang="ts">
	import { ChevronDown, ChevronUp, Download, Play } from 'lucide-svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import RatingStars from '$lib/ui/RatingStars.svelte';
	import LocalFilesIcon from '$lib/media/icons/LocalFilesIcon.svelte';
	import FavoriteHeart from '$lib/features/library/components/FavoriteHeart.svelte';
	import LibraryFormatBadge from '$lib/features/library/components/LibraryFormatBadge.svelte';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { buildDiscoveryQueueFromLocal } from '$lib/features/player/playback/queueHelpers';
	import { formatBytes, formatDurationSec } from '$lib/utils/formatting';
	import type { TrackView } from '$lib/features/library/queries/types';
	import { artistHref } from '$lib/utils/entityRoutes';
	import ContextMenu, { type MenuItem } from '$lib/ui/ContextMenu.svelte';

	interface Props {
		tracks: TrackView[];
		/** The whole track list in order, missing tracks inline; none lists `tracks`. */
		slots?: TrackSlot[];
		/** A sentence about a file, by track id, shown on its row. */
		notes?: Map<string, string>;
		getTrackMenuItems?: (track: TrackView) => MenuItem[];
		/** The caller's rating of each track they rated, by track id. */
		trackRatings?: Record<string, number>;
		/** Rate a track, or clear it with 0. Rows show stars only with this. */
		onRateTrack?: (trackId: string, rating: number) => void;
		/** Heart or unheart a track. Rows show the heart beside the stars. */
		onFavoriteTrack?: (trackId: string, favorite: boolean) => void;
		/** Off inside a panel that draws its own frame (the album page's paper). */
		framed?: boolean;
	}

	let {
		tracks,
		slots,
		notes,
		getTrackMenuItems,
		trackRatings = {},
		onRateTrack,
		onFavoriteTrack,
		framed = true
	}: Props = $props();

	const rated = $derived(Boolean(onRateTrack || onFavoriteTrack));
	const rows = $derived<TrackSlot[]>(
		slots ??
			tracks.map((track) => ({
				key: track.id,
				number: `${track.disc_number}.${track.track_number}`,
				track
			}))
	);

	function playFrom(track: TrackView): void {
		const index = tracks.findIndex((held) => held.id === track.id);
		playerStore.playQueue(buildDiscoveryQueueFromLocal(tracks), Math.max(index, 0), false);
	}

	// On a narrow screen a row's size, stars and heart sit behind its toggle.
	const opened = new SvelteSet<string>();
	function toggle(id: string): void {
		if (opened.has(id)) opened.delete(id);
		else opened.add(id);
	}
</script>

{#snippet rateAndHeart(track: TrackView)}
	{#if onRateTrack}
		<RatingStars
			quiet
			value={trackRatings[track.id] ?? 0}
			label={track.title}
			onchange={(rating) => onRateTrack(track.id, rating)}
		/>
	{/if}
	{#if onFavoriteTrack}
		<FavoriteHeart
			quiet
			favorite={track.favorite ?? false}
			onchange={(favorite) => onFavoriteTrack(track.id, favorite)}
		/>
	{/if}
{/snippet}

<ol
	class="divide-y {framed
		? 'mt-3 divide-base-content/10 overflow-hidden rounded-box border border-base-content/10 bg-base-100'
		: 'divide-paper-border/70'}"
>
	{#each rows as row (row.key)}
		{#if 'missing' in row}
			<li class="px-2 py-0.5 transition-colors hover:bg-base-content/4 sm:px-4" data-missing="yes">
				<div class="flex min-h-9 flex-wrap items-center gap-x-2 sm:flex-nowrap sm:gap-x-3">
					<span class="w-8 shrink-0 text-center text-sm text-muted tabular-nums">{row.number}</span>
					<div class="order-last flex basis-full items-center gap-2 pb-1 pl-10 sm:contents">
						<div class="shrink-0 sm:order-1 sm:w-11"></div>
						<span class="shrink-0 sm:order-3">
							{#if row.missing.wanted}
								<span class="badge badge-sm badge-info badge-outline">Wanted</span>
							{:else}
								<span class="badge badge-sm badge-warning badge-outline">Missing</span>
							{/if}
						</span>
						<span
							class="shrink-0 text-xs text-muted tabular-nums sm:order-5 sm:w-10 sm:text-right sm:text-sm sm:text-base-content"
							>{formatDurationSec(row.missing.duration_seconds)}</span
						>
					</div>
					<div class="order-2 min-w-0 flex-1 py-1">
						<p class="text-[0.9375rem] leading-snug font-semibold break-words">
							{row.missing.title}
						</p>
					</div>
					<span class="order-6 hidden w-16 shrink-0 text-right text-sm tabular-nums sm:block"
						>—</span
					>
					{#if row.missing.onwant}
						<div class="order-7 ml-auto flex shrink-0 items-center gap-1">
							<IconButton
								size="xs"
								icon={Download}
								label={`Request ${row.missing.title}`}
								loading={row.missing.pending}
								disabled={row.missing.wanted}
								onclick={row.missing.onwant}
							/>
						</div>
					{/if}
				</div>
			</li>
		{:else}
			{@const track = row.track}
			<li
				class="px-2 py-0.5 transition-colors hover:bg-base-content/4 hover:[--dn-quiet-ink:currentColor] focus-within:[--dn-quiet-ink:currentColor] sm:px-4"
			>
				<!-- One line from 640 up; narrower, the format and length drop under the title. -->
				<div class="flex min-h-9 flex-wrap items-center gap-x-2 sm:flex-nowrap sm:gap-x-3">
					<span class="w-8 shrink-0 text-center text-sm text-muted tabular-nums">{row.number}</span>
					<div class="order-last flex basis-full items-center gap-2 pb-1 pl-10 sm:contents">
						<div class="shrink-0 sm:order-1 sm:w-11">
							<LibraryFormatBadge format={track.format} size="badge-xs" />
						</div>
						<span
							class="shrink-0 text-xs text-muted tabular-nums sm:order-5 sm:w-10 sm:text-right sm:text-sm sm:text-base-content"
							>{formatDurationSec(track.duration_seconds)}</span
						>
					</div>
					<div class="order-2 min-w-0 flex-1 py-1">
						<p class="text-[0.9375rem] leading-snug font-semibold break-words">{track.title}</p>
						<a
							href={artistHref(track.artist_mbid ?? track.artist_id ?? '')}
							class="text-xs text-muted hover:underline">{track.artist_name || 'Unknown artist'}</a
						>
						{#if notes?.get(track.id)}
							<p class="text-xs text-info">{notes.get(track.id)}</p>
						{/if}
					</div>
					{#if rated}
						<div class="order-4 hidden shrink-0 items-center md:flex">
							{@render rateAndHeart(track)}
						</div>
					{/if}
					<span class="order-6 hidden w-16 shrink-0 text-right text-sm tabular-nums sm:block"
						>{track.file_size_bytes > 0 ? formatBytes(track.file_size_bytes) : '—'}</span
					>
					<Button
						variant="accent"
						size="xs"
						icon={Play}
						class="order-7 shrink-0 [&_svg]:fill-current"
						aria-label={`Play ${track.title}`}
						onclick={() => playFrom(track)}
					>
						<LocalFilesIcon class="h-4 w-4" />
					</Button>
					{#if getTrackMenuItems}
						<div class="order-7">
							<ContextMenu items={getTrackMenuItems(track)} position="end" size="xs" />
						</div>
					{/if}
					<IconButton
						size="xs"
						icon={opened.has(track.id) ? ChevronUp : ChevronDown}
						label={opened.has(track.id) ? 'Hide track details' : 'Show track details'}
						class="order-8 shrink-0 {rated ? 'md:hidden' : 'sm:hidden'}"
						aria-expanded={opened.has(track.id)}
						onclick={() => toggle(track.id)}
					/>
				</div>
				{#if opened.has(track.id)}
					<div
						class="flex flex-wrap items-center gap-x-4 gap-y-1 pb-1.5 pl-10 text-xs {rated
							? 'md:hidden'
							: 'sm:hidden'}"
					>
						<span class="sm:hidden"
							><span class="text-muted">Size:</span>
							{track.file_size_bytes > 0 ? formatBytes(track.file_size_bytes) : '—'}</span
						>
						{#if rated}
							<div class="flex items-center">{@render rateAndHeart(track)}</div>
						{/if}
					</div>
				{/if}
			</li>
		{/if}
	{/each}
</ol>
