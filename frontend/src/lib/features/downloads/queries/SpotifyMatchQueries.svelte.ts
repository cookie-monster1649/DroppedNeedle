// Fork: matching held files to a song from the owner's Spotify playlists.
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';

import { heldMutation, type HeldFilesInput } from './HeldQueries.svelte';

export type SpotifyCandidate = components['schemas']['SpotifyCandidate'];

/** Songs of the owner's Spotify playlists; an empty search asks for the held file's title. */
export const getSpotifyCandidatesQuery = (getEntryId: Getter<number>, getSearch: Getter<string>) =>
	createQuery(() => ({
		queryKey: ['held', 'spotify-candidates', getEntryId(), getSearch()],
		queryFn: ({ signal }) =>
			api.v3.GET(
				v3('/api/v3/held/{entry_id}/spotify-candidates', {
					path: { entry_id: getEntryId() },
					query: { q: getSearch() || undefined }
				}),
				{ signal }
			),
		staleTime: 30_000
	}));

export interface HeldSpotifyMatchInput extends HeldFilesInput {
	/** The playlist entry the Spotify song was read from. */
	candidateId: string;
}

/** Import the files as the Spotify song picked, tagged from Spotify. */
export const createMatchSpotifyHeldMutation = () =>
	heldMutation(
		(input: HeldSpotifyMatchInput) =>
			api.v3.POST(
				v3('/api/v3/held/{entry_id}/match-spotify', { path: { entry_id: input.entryId } }),
				{ entry_id: input.candidateId, file_ids: input.fileIds ?? null }
			),
		'Match failed'
	);
