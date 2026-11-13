<!--
	Fork: Liked Songs at the top of Import from Spotify. It imports as a normal
	playlist ("Liked Songs", Spotify's order) and syncs like one: songs liked
	at Spotify are added (and requested while sync is on), unliked songs leave
	the playlist. Nothing is deleted from the library.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { Heart, Download, RefreshCw, TriangleAlert } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import { providerUnavailableMessage } from '$lib/utils/serviceError';
	import { fetchSpotifySync, setSpotifySync } from '$lib/api/playlists';
	import { createImportSpotifyPlaylistMutation } from '$lib/features/playlists/queries/spotify.svelte';
	import type { components } from '$lib/api/v3/openapi';

	type Item = components['schemas']['SpotifyPlaylistItem'];

	interface Props {
		liked: Item | null | undefined;
		needsReconnect: boolean;
	}

	let { liked, needsReconnect }: Props = $props();

	const importMutation = createImportSpotifyPlaylistMutation();
	let busy = $state<'import' | 'download' | 'sync' | null>(null);
	let syncOn = $state<boolean | null>(null);
	// Writable: an import made here sets it before the list is read again.
	let playlistId = $derived<string | null>(liked?.imported_playlist_id ?? null);

	$effect(() => {
		const id = playlistId;
		if (!id) {
			syncOn = false;
			return;
		}
		fetchSpotifySync(id)
			.then((state) => (syncOn = state.enabled))
			.catch(() => (syncOn = null));
	});

	async function ensureImported(): Promise<string> {
		const result = await importMutation.mutateAsync({ id: 'liked', name: 'Liked Songs' });
		playlistId = result.playlist_id;
		return result.playlist_id;
	}

	async function run(kind: 'import' | 'download' | 'sync', work: () => Promise<void>) {
		if (busy) return;
		busy = kind;
		try {
			await work();
		} catch (error) {
			toastStore.show({
				message: providerUnavailableMessage(error) ?? 'Liked Songs could not be read from Spotify',
				type: 'error'
			});
		} finally {
			busy = null;
		}
	}

	const syncNow = () =>
		run('import', async () => {
			await ensureImported();
			toastStore.show({ message: 'Liked Songs is syncing in the background', type: 'success' });
		});

	const downloadAll = () =>
		run('download', async () => {
			const id = await ensureImported();
			await goto(withBasePath(`/playlists/${id}?request=all`));
		});

	const setSync = (enabled: boolean) =>
		run('sync', async () => {
			const id = playlistId ?? (await ensureImported());
			const state = await setSpotifySync(id, enabled, true);
			syncOn = state.enabled;
		});
</script>

{#if needsReconnect}
	<div class="alert alert-warning" role="alert">
		<TriangleAlert class="size-5" aria-hidden="true" />
		<span>
			Reconnect Spotify on your <a class="link" href={withBasePath('/profile')}>profile</a> to enable
			Liked Songs, saved albums and followed artists.
		</span>
	</div>
{:else if liked}
	<section
		class="ls-card pf-box flex flex-wrap items-center gap-4 px-4 py-4"
		aria-label="Liked Songs"
	>
		<span class="ls-art" aria-hidden="true"><Heart class="size-8" fill="currentColor" /></span>
		<div class="min-w-0 flex-1">
			<p class="text-lg font-semibold">Liked Songs</p>
			<p class="text-sm text-muted">
				{liked.track_count} song{liked.track_count === 1 ? '' : 's'}
				{#if playlistId}
					· <a class="link" href={withBasePath(`/playlists/${playlistId}`)}>View playlist</a>
				{/if}
			</p>
		</div>
		<label class="label cursor-pointer gap-2 p-0 text-sm">
			<input
				type="checkbox"
				class="toggle toggle-sm toggle-primary"
				checked={syncOn === true}
				disabled={busy !== null || syncOn === null}
				onchange={(e) => void setSync(e.currentTarget.checked)}
			/>
			Sync
		</label>
		<Button
			variant="outline"
			size="display-sm"
			icon={RefreshCw}
			loading={busy === 'import'}
			disabled={busy !== null && busy !== 'import'}
			onclick={() => void syncNow()}
		>
			Sync Liked Songs
		</Button>
		<Button
			variant="primary"
			size="display-sm"
			icon={Download}
			loading={busy === 'download'}
			disabled={busy !== null && busy !== 'download'}
			onclick={() => void downloadAll()}
		>
			Download all
		</Button>
	</section>
{/if}

<style>
	.ls-art {
		display: grid;
		place-items: center;
		width: 4rem;
		height: 4rem;
		border-radius: 0.5rem;
		background: linear-gradient(135deg, #450af5, #8e8ee5);
		color: white;
	}
</style>
