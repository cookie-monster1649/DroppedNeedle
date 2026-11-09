// Fork: the reads and the removal "Merge albums" makes before it merges.
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { TrackView } from '$lib/features/library/queries/types';

/** Every track of one library album. */
export async function fetchAlbumTracks(albumId: string): Promise<TrackView[]> {
	const page = await api.v3.GET(
		v3('/api/v3/library/albums/{id}/tracks', { path: { id: albumId }, query: { limit: 500 } })
	);
	return page.items;
}

/** One library album's current revision. */
export async function fetchAlbumRevision(albumId: string): Promise<[string, number]> {
	const detail = await api.v3.GET(v3('/api/v3/library/albums/{id}', { path: { id: albumId } }));
	return [detail.id, detail.row_revision];
}

/** Send one track's file to the recycle bin. */
export async function removeTrack(track: TrackView): Promise<void> {
	await api.v3.DELETE(
		v3('/api/v3/library/tracks/{id}', {
			path: { id: track.id },
			query: { expected_revision: track.row_revision }
		})
	);
}
