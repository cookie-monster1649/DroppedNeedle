// Fork: "Tidy up" for a group on the Duplicates page. The server names the
// album to keep and the lesser copies; tidying merges the other albums into
// the kept one (preview, then apply its token), then sends the lesser
// copies to the recycle bin with the revisions the merge left them. Both
// go through the routes the album page uses, so both undo the usual ways.
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { LibraryQueryKeyFactory } from '$lib/features/library/queries/LibraryQueryKeyFactory';
import {
	albumRemovalsChanged,
	catalogChanged,
	createAppMutation,
	libraryCountsChanged,
	localChanged
} from '$lib/queries/effects';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { createUuid } from '$lib/utils/uuid';

export type TidyPlan = components['schemas']['TidyGroupView'];

/** What tidying these albums does, or null when there is nothing to tidy. */
export async function fetchTidyPlan(albumIds: string[]): Promise<TidyPlan | null> {
	const found = await api.v3.POST(v3('/api/v3/library/duplicates/tidy-plan'), {
		album_ids: albumIds
	});
	return found.plan ?? null;
}

export function createTidyDuplicateGroupMutation() {
	return createAppMutation(() => ({
		mutationFn: async (plan: TidyPlan): Promise<void> => {
			const keep = plan.albums[0];
			const others = plan.albums.slice(1);
			if (others.length) {
				const request = {
					track_ids: others.flatMap((album) => album.tracks.map((track) => track.track_id)),
					expected_album_revisions: Object.fromEntries(
						plan.albums.map((album) => [album.album_id, album.row_revision])
					),
					target_album_id: keep.album_id,
					identity_choice: 'detach' as const
				};
				const preview = await api.v3.POST(v3('/api/v3/library/albums/merge-preview'), request);
				await api.v3.POST(v3('/api/v3/library/albums/merge'), {
					...request,
					preview_token: preview.preview_token,
					idempotency_key: createUuid()
				});
			}
			if (!plan.surplus_track_ids.length) return;
			const tracks = await api.v3.GET(
				v3('/api/v3/library/albums/{id}/tracks', {
					path: { id: keep.album_id },
					query: { limit: 500 }
				})
			);
			for (const id of plan.surplus_track_ids) {
				const track = tracks.items.find((item) => item.id === id);
				if (!track) continue;
				await api.v3.DELETE(
					v3('/api/v3/library/tracks/{id}', {
						path: { id },
						query: { expected_revision: track.row_revision }
					})
				);
			}
		},
		onSuccess: async () => {
			await invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.catalog.duplicates(authStore.user?.id)
			});
		},
		effects: () => {
			const userId = authStore.user?.id;
			return [
				catalogChanged(userId),
				albumRemovalsChanged(userId),
				libraryCountsChanged(userId),
				localChanged()
			];
		}
	}));
}
