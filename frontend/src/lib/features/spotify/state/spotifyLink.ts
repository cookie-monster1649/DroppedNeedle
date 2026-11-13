// Fork: a Spotify track, album or artist link pasted into search.
// Mirrors the server's parser (acquire/src/imports/spotify_resolve.rs).

export type SpotifyLinkKind = 'track' | 'album' | 'artist';

export interface SpotifyLink {
	kind: SpotifyLinkKind;
	id: string;
}

const KINDS = new Set<string>(['track', 'album', 'artist']);
const ID = /^[A-Za-z0-9]{22}$/;

/**
 * The item a pasted Spotify link names: `open.spotify.com/{track,album,artist}/<id>`
 * with or without the scheme, an `/intl-xx/` prefix, `/embed/` or a `?si=`
 * query, and `spotify:<type>:<id>`. Anything else is `null`.
 */
export function parseSpotifyLink(input: string): SpotifyLink | null {
	const text = input.trim();
	if (text.startsWith('spotify:')) {
		const parts = text.slice('spotify:'.length).split(':');
		if (parts.length !== 2 || !KINDS.has(parts[0]) || !ID.test(parts[1])) return null;
		return { kind: parts[0] as SpotifyLinkKind, id: parts[1] };
	}
	const rest = text.replace(/^https?:\/\//i, '');
	const slash = rest.indexOf('/');
	if (slash < 0) return null;
	const host = rest.slice(0, slash).toLowerCase();
	if (host !== 'open.spotify.com' && host !== 'play.spotify.com') return null;
	const path = rest.slice(slash + 1).split(/[?#]/)[0];
	const segments = path.split('/').filter(Boolean);
	if (segments[0]?.startsWith('intl-')) segments.shift();
	if (segments[0] === 'embed') segments.shift();
	if (segments.length !== 2 || !KINDS.has(segments[0]) || !ID.test(segments[1])) return null;
	return { kind: segments[0] as SpotifyLinkKind, id: segments[1] };
}

export interface SpotifyResolutionLike {
	kind: string;
	found: boolean;
	name: string;
	album_id?: string | null;
	artist_id?: string | null;
	recording_id?: string | null;
}

/** Where a resolved link opens (a song opens its album with the song marked), or null. */
export function resolutionPath(resolution: SpotifyResolutionLike): string | null {
	if (!resolution.found) return null;
	if (resolution.kind === 'artist') {
		return resolution.artist_id ? `/artist/${encodeURIComponent(resolution.artist_id)}` : null;
	}
	if (!resolution.album_id) return null;
	const album = `/album/${encodeURIComponent(resolution.album_id)}`;
	if (resolution.kind === 'track' && resolution.recording_id) {
		return `${album}?track=${encodeURIComponent(resolution.recording_id)}`;
	}
	return album;
}

/** The message for a link with no confident match. */
export function notFoundMessage(name: string): string {
	const what = name.trim() ? `"${name.trim()}"` : 'this';
	return `Couldn't find ${what} on MusicBrainz or Deezer`;
}
