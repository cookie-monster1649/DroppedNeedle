// Fork: the saved-album review's selection. Every album that can be asked for
// starts selected; one not found, already held or already requested cannot be
// selected. Confirming requests every album still selected, in batches the
// batch endpoint takes (at most 500 each).

export interface SavedAlbumLike {
	spotify_album_id: string;
	title: string;
	artist_name: string;
	year?: number | null;
	release_group?: string | null;
	held: boolean;
	requested: boolean;
}

export const BATCH_LIMIT = 500;

/** Whether the album can be selected: matched, not held, not requested. */
export function isSelectable(album: SavedAlbumLike): boolean {
	return Boolean(album.release_group) && !album.held && !album.requested;
}

/** Every selectable album, selected. */
export function initialSelection(albums: SavedAlbumLike[]): Set<string> {
	return new Set(albums.filter(isSelectable).map((album) => album.spotify_album_id));
}

/** Select or deselect one album; a disabled one never changes. */
export function toggle(
	selection: Set<string>,
	album: SavedAlbumLike,
	selected: boolean
): Set<string> {
	const next = new Set(selection);
	if (!isSelectable(album)) return next;
	if (selected) next.add(album.spotify_album_id);
	else next.delete(album.spotify_album_id);
	return next;
}

/** Select every selectable album, or none. */
export function selectAll(albums: SavedAlbumLike[], selected: boolean): Set<string> {
	return selected ? initialSelection(albums) : new Set();
}

export interface AlbumRequestItem {
	musicbrainz_id: string;
	artist_name: string;
	album_title: string;
	year?: number;
}

/** The selected albums as request batches (one album request each). */
export function requestBatches(
	albums: SavedAlbumLike[],
	selection: Set<string>
): AlbumRequestItem[][] {
	const seen = new Set<string>();
	const items: AlbumRequestItem[] = [];
	for (const album of albums) {
		if (!selection.has(album.spotify_album_id) || !isSelectable(album)) continue;
		const group = album.release_group as string;
		if (seen.has(group)) continue;
		seen.add(group);
		items.push({
			musicbrainz_id: group,
			artist_name: album.artist_name,
			album_title: album.title,
			year: album.year ?? undefined
		});
	}
	const batches: AlbumRequestItem[][] = [];
	for (let start = 0; start < items.length; start += BATCH_LIMIT) {
		batches.push(items.slice(start, start + BATCH_LIMIT));
	}
	return batches;
}
