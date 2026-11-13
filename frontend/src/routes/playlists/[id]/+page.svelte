<script lang="ts">
	import { page as pageState } from '$app/state';
	import { goto } from '$app/navigation';
	import { onDestroy, untrack } from 'svelte';
	import {
		fetchPlaylistRows,
		planMissingRequests,
		requestMissingTracks,
		type PlaylistDetail,
		type PlaylistRowState,
		type PlaylistTrack
	} from '$lib/api/playlists';
	import { playlistTrackToQueueItem } from '$lib/features/player/playback/queueHelpers';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		getPlaylistDetailV3Query,
		getPlaylistSourcesV3QueryOptions,
		type PlaylistDetailV3
	} from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
	import {
		createDeletePlaylistV3Mutation,
		forgetPlaylistSources,
		refreshPlaylist,
		createSetPlaylistVisibilityV3Mutation
	} from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import {
		privatePlaylistLine,
		privatePlaylistStub,
		toPageDetail
	} from '$lib/features/playlists/queries/playlistV3Adapter';
	import { queryClient } from '$lib/queries/QueryClient';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		Music,
		Download,
		Lock,
		Loader,
		LoaderCircle,
		RefreshCw,
		TriangleAlert
	} from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import type { PageData } from './$types';
	import PlaylistHeader from '$lib/features/playlists/components/PlaylistHeader.svelte';
	import PlaylistTrackList from '$lib/features/playlists/components/PlaylistTrackList.svelte';
	import DeletePlaylistModal from '$lib/features/playlists/components/DeletePlaylistModal.svelte';
	import PlaylistNotice from '$lib/features/playlists/components/PlaylistNotice.svelte';
	import {
		createImportSpotifyPlaylistMutation,
		getSpotifyImportStatusQuery,
		IMPORT_POLL_MS
	} from '$lib/features/playlists/queries/spotify.svelte';
	import { providerUnavailableMessage } from '$lib/utils/serviceError';

	let { data }: { data: PageData } = $props();

	const detailQuery = getPlaylistDetailV3Query(
		() => data.playlistId,
		() => true
	);
	const shareMutation = createSetPlaylistVisibilityV3Mutation();
	const deleteMutation = createDeletePlaylistV3Mutation();

	// A local mutable copy of the (full) playlist so child components can keep
	// applying optimistic updates; redaction/loading/error leave it null.
	let playlist = $state<PlaylistDetail | null>(null);
	let deleting = $state(false);

	let deleteModal = $state<ReturnType<typeof DeletePlaylistModal> | null>(null);
	let trackList = $state<ReturnType<typeof PlaylistTrackList> | null>(null);
	let header = $state<ReturnType<typeof PlaylistHeader> | null>(null);

	let loading = $derived(detailQuery.isLoading);
	let locked = $derived(privatePlaylistStub(detailQuery.error));
	let loadError = $derived.by(() => {
		if (!detailQuery.isError) return null;
		const msg = detailQuery.error instanceof Error ? detailQuery.error.message : '';
		return /404|not found/i.test(msg) ? 'Playlist not found' : "Couldn't load this playlist";
	});

	let isOwner = $derived(playlist?.is_owner ?? false);
	let canDelete = $derived(playlist?.can_delete ?? false);

	// Fork: a playlist imported from Spotify fills in the background. While
	// its import runs the page waits on it (checked every 2 s) instead of
	// showing an empty list, then shows the tracklist and each song's status
	// the moment it is in; a failed import shows why, with a retry.
	let spotifyId = $derived(
		playlist?.source_ref?.startsWith('spotify:') ? playlist.source_ref.slice(8) : null
	);
	const importStatus = getSpotifyImportStatusQuery(() =>
		isOwner && spotifyId && playlist ? playlist.id : null
	);
	const reimport = createImportSpotifyPlaylistMutation();
	let importState = $derived(importStatus.data?.state ?? null);
	let importing = $derived(importState === 'running' || reimport.isPending);
	let importFailed = $derived(
		!importing && importState === 'failed' && (playlist?.tracks.length ?? 0) === 0
	);
	let lastImportState: string | null = null;
	let importSettledAt = $state(0);
	$effect(() => {
		const state = importState;
		const id = playlist?.id;
		if (state === lastImportState) return;
		const finished = lastImportState === 'running' && state !== 'running';
		lastImportState = state;
		if (!finished || !id) return;
		// The tracklist is in: read it and every song's status afresh, and
		// keep checking the statuses a while for the import's own requests.
		importSettledAt = Date.now();
		untrack(() => {
			void detailQuery.refetch().then(() => loadRowStates(id));
		});
	});

	async function retryImport() {
		if (!playlist || !spotifyId) return;
		try {
			await reimport.mutateAsync({ id: spotifyId, name: playlist.name });
			await importStatus.refetch();
		} catch (error) {
			toastStore.show({
				message: providerUnavailableMessage(error) ?? "Couldn't start the import again",
				type: 'error'
			});
		}
	}

	// Fork: a missing song already requested (requested, fetching, held, or
	// landed and not yet linked) is being found; only the rest are offered.
	const FINDING: ReadonlySet<PlaylistRowState> = new Set([
		'requested',
		'fetching',
		'held',
		'imported'
	]);
	const isFinding = (track: PlaylistTrack) => {
		const state = rowStates?.get(track.id);
		return state !== undefined && FINDING.has(state);
	};
	let requestable = $derived(playlist ? playlist.tracks.filter((t) => !isFinding(t)) : []);
	let findingPlan = $derived(
		playlist ? planMissingRequests(playlist.tracks.filter(isFinding)) : { tracks: [], wants: [] }
	);
	let findingCount = $derived(findingPlan.tracks.length + findingPlan.wants.length);
	let missingPlan = $derived(planMissingRequests(requestable));
	let missingCount = $derived(missingPlan.tracks.length + missingPlan.wants.length);
	let missingLabel = $derived(`${missingCount} ${missingCount === 1 ? 'track' : 'tracks'}`);

	let requesting = $state(false);

	// Fork: per-row status for the owner (Missing / Fetching / Held / Failed),
	// refreshed every 15 s while anything is on its way.
	let rowStates = $state<Map<string, PlaylistRowState> | undefined>(undefined);
	const ROW_POLL_MS = 15_000;
	// Fork: how long after an import the statuses keep being read, for the
	// songs its auto-request asks for.
	const AFTER_IMPORT_MS = 120_000;
	// Fork: true from a list change until its statuses are read, so the
	// "Request N tracks" offer never counts songs already on their way.
	let rowsStale = $state(false);
	async function loadRowStates(playlistId: string) {
		try {
			rowStates = await fetchPlaylistRows(playlistId);
		} catch {
			// Status is extra: the list still works without it.
		} finally {
			rowsStale = false;
		}
	}
	let rowsFor = $derived(isOwner ? (playlist?.id ?? null) : null);
	$effect(() => {
		const id = rowsFor;
		if (!id) return;
		void loadRowStates(id);
		const timer = setInterval(() => {
			const moving = [...(rowStates?.values() ?? [])].some(
				(state) => state === 'requested' || state === 'fetching' || state === 'imported'
			);
			const justImported = Date.now() - importSettledAt < AFTER_IMPORT_MS;
			if (moving || justImported) void loadRowStates(id);
		}, ROW_POLL_MS);
		return () => clearInterval(timer);
	});

	async function handleRequestTrack(track: PlaylistTrack) {
		const result = await requestMissingTracks([track]);
		toastStore.show({
			message: result.requested ? `Requested ${track.track_name}` : result.message,
			type: result.success ? 'success' : 'error'
		});
		if (playlist) await loadRowStates(playlist.id);
	}

	async function handleRequestMissing() {
		if (requesting || !playlist) return;
		requesting = true;
		try {
			const result = await requestMissingTracks(requestable);
			toastStore.show({ message: result.message, type: 'success' });
			void loadRowStates(playlist.id);
		} catch {
			toastStore.show({ message: "Couldn't submit requests", type: 'error' });
		} finally {
			requesting = false;
		}
	}

	// Fork: "Request all songs" from the prompt after a Spotify import. The
	// import fills the playlist in the background, so wait for it to finish
	// (two minutes at most), read the list and its statuses, then ask.
	async function handleRequestAll() {
		for (let tries = 0; tries < 60; tries += 1) {
			const status = await importStatus.refetch();
			if (status.data?.state !== 'running') break;
			await new Promise((done) => setTimeout(done, IMPORT_POLL_MS));
		}
		const fresh = await detailQuery.refetch();
		if (fresh.data) {
			const page = toPageDetail(fresh.data);
			playlist = { ...page, tracks: page.tracks.map((t) => ({ ...t })) };
			await loadRowStates(fresh.data.id);
		}
		await handleRequestMissing();
	}

	// Fork: "Download all" on Liked Songs opens the playlist with
	// `?request=all`: ask for every song once the list is in.
	let requestAllAsked = false;
	$effect(() => {
		if (requestAllAsked || !playlist) return;
		if (pageState.url.searchParams.get('request') !== 'all') return;
		requestAllAsked = true;
		const url = new URL(window.location.href);
		url.searchParams.delete('request');
		history.replaceState(history.state, '', url);
		void handleRequestAll();
	});

	function applySourcesMap(sources: Record<string, string[]>, onlyMissing = false) {
		if (!playlist) return;
		for (const track of playlist.tracks) {
			const resolved = sources[track.id];
			if (!resolved || resolved.length === 0) continue;
			if (onlyMissing) {
				// Live detailQuery values (clone) win: never let a stale cache entry
				// overwrite a healed row that already has sources or a local link.
				const hasLive =
					(track.available_sources && track.available_sources.length > 0) ||
					Boolean(track.library_file_id);
				if (hasLive) continue;
			}
			track.available_sources = resolved;
		}
	}

	function hasUsableSourcesForPlaylist(
		sources: Record<string, string[]>,
		trackIds: Set<string>
	): boolean {
		for (const [id, value] of Object.entries(sources)) {
			if (trackIds.has(id) && Array.isArray(value) && value.length > 0) return true;
		}
		return false;
	}

	async function resolveSources(playlistId: string) {
		const options = getPlaylistSourcesV3QueryOptions(authStore.user?.id, playlistId);
		const before = queryClient.getQueryData(options.queryKey);
		try {
			const sources = await queryClient.fetchQuery(options);
			if (!playlist || playlist.id !== playlistId) return;
			const ids = new Set(playlist.tracks.map((t) => t.id));
			if (!hasUsableSourcesForPlaylist(sources, ids)) {
				// Nothing usable is never fresh: the next visit asks again.
				await forgetSources(playlistId);
				return;
			}
			if (sources === before) {
				// Live detailQuery values (clone) already hold the healed rows;
				// the kept answer only fills gaps.
				applySourcesMap(sources, true);
				return;
			}
			applySourcesMap(sources);
			await refreshPlaylist(playlistId);
		} catch {
			// non-critical - tracks keep their stored available_sources
		}
	}

	async function forgetSources(playlistId: string) {
		try {
			await forgetPlaylistSources(playlistId);
		} catch {
			// the kept answer only goes stale on its own later
		}
	}

	let lastSyncedData: PlaylistDetailV3 | undefined;
	$effect(() => {
		const d = detailQuery.data;
		if (d === lastSyncedData) return;
		lastSyncedData = d;
		untrack(() => {
			trackList?.clearReorderState();
			header?.cleanupPreview();
			if (d) {
				// Clone so optimistic child mutations never touch the query cache.
				const page = toPageDetail(d);
				const grew = page.tracks.length !== (playlist?.tracks.length ?? -1);
				playlist = { ...page, tracks: page.tracks.map((t) => ({ ...t })) };
				void resolveSources(d.id);
				// Fork: a list that changed (an import landing) reads its
				// statuses again rather than keeping the ones of the old list.
				if (grew && page.is_owner) {
					rowsStale = true;
					void loadRowStates(d.id);
				}
			} else {
				playlist = null;
			}
		});
	});

	function playAll() {
		if (!playlist || playlist.tracks.length === 0) return;
		const items = playlist.tracks
			.map(playlistTrackToQueueItem)
			.filter((item): item is NonNullable<typeof item> => item !== null);
		if (items.length === 0) {
			toastStore.show({ message: 'Nothing here can be played right now', type: 'info' });
			return;
		}
		playerStore.playQueue(items, 0, false);
	}

	function shuffleAll() {
		if (!playlist || playlist.tracks.length < 2) return;
		const items = playlist.tracks
			.map(playlistTrackToQueueItem)
			.filter((item): item is NonNullable<typeof item> => item !== null);
		if (items.length === 0) {
			toastStore.show({ message: 'Nothing here can be played right now', type: 'info' });
			return;
		}
		playerStore.playQueue(items, 0, true);
	}

	function playFromTrack(index: number) {
		if (!playlist || playlist.tracks.length === 0) return;
		const rowItems = playlist.tracks.map(playlistTrackToQueueItem);
		if (!rowItems[index]) {
			toastStore.show({ message: 'Track is not playable', type: 'error' });
			return;
		}
		const items = rowItems.filter((item): item is NonNullable<typeof item> => item !== null);
		// Rows with no file are not queued, so count only the playable rows above this one.
		const startIndex = rowItems.slice(0, index).filter((item) => item !== null).length;
		playerStore.playQueue(items, startIndex, false);
	}

	function handleSourceChange() {
		if (playlist) void forgetSources(playlist.id);
	}

	function handlePlaylistUpdate(updatedPlaylist: PlaylistDetail) {
		playlist = updatedPlaylist;
	}

	async function handleShare(isPublic: boolean) {
		if (!playlist || shareMutation.isPending) return;
		try {
			const updated = await shareMutation.mutateAsync({ id: playlist.id, isPublic });
			playlist = { ...playlist, is_public: updated.is_public };
			toastStore.show({
				message: updated.is_public ? 'Playlist is now public' : 'Playlist is now private',
				type: 'success'
			});
		} catch {
			toastStore.show({ message: "Couldn't update sharing", type: 'error' });
		}
	}

	async function confirmDelete() {
		if (!playlist || deleting) return;
		deleting = true;
		try {
			await deleteMutation.mutateAsync(playlist.id);
			toastStore.show({ message: 'Playlist deleted', type: 'success' });
			await goto(withBasePath('/playlists'));
		} catch {
			toastStore.show({ message: "Couldn't delete the playlist", type: 'error' });
		} finally {
			deleting = false;
		}
	}

	onDestroy(() => {
		trackList?.clearReorderState();
		header?.cleanupPreview();
	});
</script>

<svelte:head>
	<title>{locked ? 'Private playlist' : (playlist?.name ?? 'Playlist')} - DroppedNeedle</title>
</svelte:head>

<div class="w-full">
	{#if loading}
		<div class="mx-auto max-w-7xl space-y-8 px-4 py-5 sm:px-6 lg:px-8" aria-busy="true">
			<div class="skeleton size-12 rounded-full"></div>
			<div class="flex flex-col gap-8 md:flex-row md:items-center">
				<div class="skeleton aspect-square w-full max-w-72 shrink-0 rounded-md lg:max-w-80"></div>
				<div class="flex flex-1 flex-col gap-4">
					<div class="skeleton h-16 w-3/4"></div>
					<div class="skeleton h-4 w-48"></div>
					<div class="skeleton h-4 w-64"></div>
					<div class="mt-4 flex gap-3">
						<div class="skeleton h-12 w-36"></div>
						<div class="skeleton h-12 w-32"></div>
					</div>
				</div>
			</div>
			<div class="space-y-2">
				{#each Array(4) as _, i (`loading-track-${i}`)}
					<div class="skeleton h-14 w-full"></div>
				{/each}
			</div>
		</div>
	{:else if locked}
		<PlaylistNotice icon={Lock} title="Private playlist" message={privatePlaylistLine(locked)} />
	{:else if loadError}
		<PlaylistNotice
			icon={Music}
			title="Couldn't load this playlist"
			message={loadError}
			onretry={() => void detailQuery.refetch()}
		/>
	{:else if !playlist}
		<PlaylistNotice icon={Music} title="Playlist not found" />
	{:else}
		<PlaylistHeader
			bind:this={header}
			{playlist}
			canEdit={isOwner}
			{canDelete}
			sharePending={shareMutation.isPending}
			onplayall={playAll}
			onshuffleall={shuffleAll}
			ondeleteclick={() => deleteModal?.showModal()}
			onplaylistupdate={handlePlaylistUpdate}
			onshare={handleShare}
			onrequestall={handleRequestAll}
		/>

		<div class="mx-auto max-w-7xl space-y-6 px-4 pb-10 sm:px-6 lg:px-8">
			{#if importing && playlist.tracks.length === 0}
				<div class="space-y-3" aria-busy="true">
					<p class="flex items-center gap-2 text-sm font-medium" role="status">
						<LoaderCircle class="size-5 animate-spin motion-keep" aria-hidden="true" />
						Fetching tracklist from Spotify…
					</p>
					{#each Array(6) as _, i (`importing-track-${i}`)}
						<div class="skeleton h-14 w-full"></div>
					{/each}
				</div>
			{:else if importFailed}
				<div
					class="flex flex-wrap items-center gap-x-4 gap-y-3 rounded-box border border-error/60 bg-error/8 px-4 py-3"
					role="alert"
				>
					<TriangleAlert class="size-5 shrink-0 text-error" aria-hidden="true" />
					<p class="min-w-0 flex-1 basis-48 text-sm font-medium">
						Couldn't fetch the tracklist from Spotify: {importStatus.data?.message ??
							'the import did not finish'}.
					</p>
					<Button
						variant="outline"
						size="display-sm"
						icon={RefreshCw}
						loading={reimport.isPending}
						onclick={() => void retryImport()}
					>
						Retry
					</Button>
				</div>
			{:else}
				{#if importing}
					<p class="flex items-center gap-2 text-sm text-muted" role="status">
						<LoaderCircle class="size-4 animate-spin motion-keep" aria-hidden="true" />
						Updating from Spotify…
					</p>
				{/if}
				{#if isOwner && !rowsStale && (missingCount > 0 || findingCount > 0)}
					<div
						class="flex flex-wrap items-center gap-x-4 gap-y-3 rounded-box border border-warning/60 bg-warning/8 px-4 py-3"
					>
						<Download class="size-5 shrink-0 text-warning" aria-hidden="true" />
						<p class="min-w-0 flex-1 basis-48 text-sm font-medium">
							{#if missingCount > 0}
								{missingLabel} not in your library
							{:else}
								Not in your library yet
							{/if}
						</p>
						{#if findingCount > 0}
							<Button
								variant="ghost"
								size="display-sm"
								icon={Loader}
								href={withBasePath('/downloads')}
							>
								Finding {findingCount}
								{findingCount === 1 ? 'track' : 'tracks'}
							</Button>
						{/if}
						{#if missingCount > 0}
							<Button
								variant="outline"
								size="display-sm"
								icon={Download}
								loading={requesting}
								onclick={() => void handleRequestMissing()}
							>
								Request {missingLabel}
							</Button>
						{/if}
					</div>
				{/if}

				<PlaylistTrackList
					bind:this={trackList}
					{playlist}
					readonly={!isOwner}
					ontrackchange={() => {}}
					onsourcechange={handleSourceChange}
					onplaytrack={playFromTrack}
					{rowStates}
					onrequesttrack={handleRequestTrack}
				/>
			{/if}
		</div>

		<DeletePlaylistModal
			bind:this={deleteModal}
			playlistName={playlist.name}
			{deleting}
			onconfirm={confirmDelete}
		/>
	{/if}
</div>
