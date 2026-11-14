<script lang="ts">
	import { type MusicSource } from '$lib/stores/musicSource';
	import { getConnectionsQuery } from '$lib/features/profile/queries/ConnectionsQuery.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { listenSourceHealth } from '$lib/stores/listenSourceHealth.svelte';

	interface Props {
		currentSource: MusicSource;
		onSourceChange?: (source: MusicSource) => void;
		/** Sources that cannot answer right now; defaults to what the app last heard. */
		unavailable?: MusicSource[];
	}

	let { currentSource, onSourceChange, unavailable }: Props = $props();

	// Fork: an unavailable ListenBrainz is greyed out and the switcher moves
	// to Last.fm by itself. When it comes back the tab is offered again, but
	// nobody is moved off the tab they are on.
	const down = $derived(new Set(unavailable ?? listenSourceHealth.unavailable));
	const lbDown = $derived(down.has('listenbrainz'));

	const connectionsQuery = getConnectionsQuery();
	const connections = $derived(connectionsQuery.data?.connections ?? []);

	let switching = $state(false);

	let lbEnabled = $derived(connections.some((c) => c.service === 'listenbrainz'));
	// A Last.fm connection without an account name holds only the user's own
	// API key pair: the sign-in has not finished.
	let lfmEnabled = $derived(connections.some((c) => c.service === 'lastfm' && Boolean(c.username)));
	let showSwitcher = $derived(lbEnabled && lfmEnabled);

	$effect(() => {
		if (showSwitcher && lbDown && currentSource === 'listenbrainz' && !down.has('lastfm')) {
			onSourceChange?.('lastfm');
		}
	});

	async function handleSwitch(source: MusicSource) {
		if (source === currentSource || switching) return;
		if (down.has(source)) return;
		switching = true;
		onSourceChange?.(source);
		switching = false;
	}
</script>

{#if showSwitcher}
	<div class="join">
		<span
			class="join-item inline-flex"
			title={lbDown ? 'ListenBrainz is unavailable right now' : undefined}
		>
			<Button
				variant={currentSource === 'listenbrainz' && !lbDown ? 'primary' : 'ghost'}
				size="sm"
				class="join-item {lbDown ? 'opacity-50' : ''}"
				aria-pressed={currentSource === 'listenbrainz' && !lbDown}
				aria-label={lbDown ? 'ListenBrainz (unavailable right now)' : undefined}
				disabled={switching || lbDown}
				onclick={() => handleSwitch('listenbrainz')}
			>
				ListenBrainz
			</Button>
		</span>
		<Button
			variant={currentSource === 'lastfm' ? 'lastfm' : 'ghost'}
			size="sm"
			class="join-item"
			aria-pressed={currentSource === 'lastfm'}
			disabled={switching}
			onclick={() => handleSwitch('lastfm')}
		>
			Last.fm
		</Button>
	</div>
{/if}
