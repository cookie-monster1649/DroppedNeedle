<!--
	Fork: a 30-second sample (Deezer, then iTunes) of the song a track
	download stands for, on its row in the downloads page. Shares audio focus
	with every other player, so only one sound plays at a time.
-->
<script lang="ts">
	import SampleButton from '$lib/features/discover/components/SampleButton.svelte';
	import type { DownloadTask } from '../queries/types';

	let { task }: { task: DownloadTask } = $props();

	const artist = $derived(task.artist_name?.trim() || '');
	const title = $derived(task.download_type === 'track' ? task.track_title?.trim() || '' : '');
</script>

{#if artist && title}
	<span class="ml-1.5 inline-flex shrink-0 align-middle" title="Play a sample of this song">
		<SampleButton sampleKey={`task:${task.id}`} {artist} {title} kind="track" size="sm" />
	</span>
{/if}
