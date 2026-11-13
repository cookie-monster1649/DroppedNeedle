// Fork: the Spotify library sync (Profile, Spotify card) and pasted links.

import { createQuery, useQueryClient } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { SpotifyQueryKeyFactory } from '$lib/queries/featureKeys';
import {
	invalidateQueriesWithPersister,
	setQueryDataWithPersister
} from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';

export type SpotifyLibraryState = components['schemas']['SpotifyLibraryState'];
export type SpotifySavedAlbum = components['schemas']['SpotifySavedAlbum'];
export type SpotifySavedAlbumList = components['schemas']['SpotifySavedAlbumList'];
export type SpotifyResolution = components['schemas']['SpotifyResolution'];

const POLL_MS = 3000;

/** The card's state, read again every few seconds while a sync runs. */
export const getSpotifyLibraryQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		refetchOnWindowFocus: false,
		retry: false,
		queryKey: SpotifyQueryKeyFactory.library(authStore.user?.id),
		queryFn: ({ signal }: { signal: AbortSignal }) =>
			api.v3.GET(v3('/api/v3/acquire/spotify/library'), { signal }),
		refetchInterval: (query: { state: { data?: SpotifyLibraryState } }) =>
			query.state.data?.artists_syncing || query.state.data?.albums_scanning ? POLL_MS : false
	}));

/** The saved albums for the review, read again while a scan runs. */
export const getSavedAlbumsQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		refetchOnWindowFocus: false,
		retry: false,
		queryKey: SpotifyQueryKeyFactory.savedAlbums(authStore.user?.id),
		queryFn: ({ signal }: { signal: AbortSignal }) =>
			api.v3.GET(v3('/api/v3/acquire/spotify/library/albums'), { signal }),
		refetchInterval: (query: { state: { data?: SpotifySavedAlbumList } }) =>
			query.state.data?.scanning ? POLL_MS : false
	}));

/** Actions on the library, each reading the card again after. */
export function spotifyLibraryActions() {
	const client = useQueryClient();
	const refresh = () =>
		invalidateQueriesWithPersister(
			{ queryKey: SpotifyQueryKeyFactory.library(authStore.user?.id) },
			undefined,
			{ client }
		);
	const refreshAlbums = () =>
		invalidateQueriesWithPersister(
			{ queryKey: SpotifyQueryKeyFactory.savedAlbums(authStore.user?.id) },
			undefined,
			{ client }
		);
	return {
		async set(followSync: boolean, albumSync: boolean): Promise<SpotifyLibraryState> {
			const state = await api.v3.PUT(v3('/api/v3/acquire/spotify/library'), {
				follow_sync: followSync,
				album_sync: albumSync
			});
			await setQueryDataWithPersister(
				SpotifyQueryKeyFactory.library(authStore.user?.id),
				state,
				undefined,
				client
			);
			return state;
		},
		async syncArtists(): Promise<void> {
			await api.v3.POST(v3('/api/v3/acquire/spotify/library/artists/sync'));
			await refresh();
		},
		async scanAlbums(): Promise<void> {
			await api.v3.POST(v3('/api/v3/acquire/spotify/library/albums/scan'));
			await refresh();
			await refreshAlbums();
		},
		async markReviewed(): Promise<void> {
			await api.v3.POST(v3('/api/v3/acquire/spotify/library/albums/reviewed'));
			await refresh();
			await refreshAlbums();
		}
	};
}

/** What a pasted Spotify link names here. */
export function resolveSpotifyLink(url: string, signal?: AbortSignal): Promise<SpotifyResolution> {
	return api.v3.GET(v3('/api/v3/resolve/spotify', { query: { url } }), { signal });
}
