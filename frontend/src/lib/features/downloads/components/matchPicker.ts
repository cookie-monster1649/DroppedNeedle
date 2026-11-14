// Fork: the To Decide match picker's helpers.
import type { Album } from '$lib/features/album/queries/types';
import { sourceBadge } from '$lib/features/search/source/sourceBadge';

/** MusicBrainz albums first, in search order, then the Deezer albums it lacks. */
export function pickerAlbums(albums: Album[]): Album[] {
	const musicbrainz = albums.filter((album) => !sourceBadge(album.musicbrainz_id));
	const others = albums.filter((album) => sourceBadge(album.musicbrainz_id));
	return [...musicbrainz, ...others];
}

/** Whether a match put any file in the library (or found it already there). */
export function matchMoved(result: { imported: number; already_owned: number }): boolean {
	return result.imported + result.already_owned > 0;
}
