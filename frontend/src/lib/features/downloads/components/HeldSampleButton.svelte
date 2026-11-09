<!--
	Fork: a 30-second sample of the song that was asked for (Deezer, then
	iTunes), beside a held file's player, so the two can be compared by ear.
	Sample and file share audio focus: starting one pauses the other.
-->
<script lang="ts">
	import SampleButton from '$lib/features/discover/components/SampleButton.svelte';
	import type { HeldEntry } from '../queries/types';

	interface Props {
		entry: HeldEntry;
		file: HeldEntry['files'][number];
	}

	let { entry, file }: Props = $props();

	const artist = $derived(entry.artist_name?.trim() || '');
	// The song the file stands for: the track it was paired to, else, for a
	// song asked for by name (a want), the asked title, which the entry keeps
	// as its album title. An album's entry has no single song to sample.
	const isWant = $derived(entry.recording_mbid?.startsWith('want-') ?? false);
	const title = $derived(
		file.track_title?.trim() || (isWant ? entry.album_title?.trim() : '') || ''
	);
</script>

{#if artist && title}
	<span class="shrink-0" title="Play a sample of the requested song">
		<SampleButton
			sampleKey={`held:${entry.id}:${file.id}`}
			{artist}
			{title}
			kind="track"
			size="sm"
		/>
	</span>
{/if}
