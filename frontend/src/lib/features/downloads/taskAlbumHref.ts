// Fork: where a download's album link points. A want (a song asked for by
// name, e.g. from a Spotify playlist) has no release group, so it links to
// the library album its file landed in, the same page the playlist row opens.
import { albumHref } from '$lib/utils/entityRoutes';

export function taskAlbumHref(task: {
	release_group_mbid: string;
	library_album_id?: string | null;
}): string | undefined {
	if (task.release_group_mbid) return albumHref(task.release_group_mbid);
	return task.library_album_id ? albumHref(task.library_album_id) : undefined;
}
