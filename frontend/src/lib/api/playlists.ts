import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import type { QueueItem } from '$lib/features/player/playback/types';
import type { components } from '$lib/api/v3/openapi';
import { toPageDetail } from '$lib/features/playlists/queries/playlistV3Adapter';

type Schemas = components['schemas'];

// The contract's rows with dates as ISO strings and every optional field filled.
export type PlaylistTrack = Required<Omit<Schemas['PlaylistTrack'], 'created_at'>> & {
	created_at: string;
};

export type PlaylistSummary = Required<
	Omit<Schemas['PlaylistSummary'], 'created_at' | 'updated_at' | 'is_redacted'>
> & {
	created_at: string;
	updated_at: string;
	is_redacted: false;
};

export type PlaylistDetail = PlaylistSummary & { tracks: PlaylistTrack[] };

/** A private playlist the caller may not read: count + owner only. */
export type RedactedPlaylist = Required<Omit<Schemas['RedactedPlaylist'], 'is_redacted'>> & {
	is_redacted: true;
};

export type PlaylistListItem = PlaylistSummary | RedactedPlaylist;
export type PlaylistDetailItem = PlaylistDetail | RedactedPlaylist;

export function isRedactedPlaylist(
	item: PlaylistListItem | PlaylistDetailItem
): item is RedactedPlaylist {
	return item.is_redacted === true;
}

export type TrackData = Schemas['TrackInput'] & { source_type: string };

export function queueItemToTrackData(item: QueueItem): TrackData {
	return {
		track_name: item.trackName,
		artist_name: item.artistName,
		album_name: item.albumName,
		album_id: item.albumId || null,
		artist_id: item.artistId || null,
		track_source_id: item.trackSourceId || null,
		cover_url: item.coverUrl,
		source_type: item.sourceType,
		available_sources: item.availableSources ?? null,
		format: item.format ?? null,
		track_number: item.trackNumber ?? null,
		disc_number: item.discNumber ?? null,
		duration: item.duration ?? null,
		plex_rating_key: item.plexRatingKey ?? null,
		recording_mbid: item.recordingIds?.[item.sourceType] ?? null
	};
}

// Thin helpers over the v3 contract: every call below hits /api/v3 and adapts
// the wire shape (epoch dates, optional provenance) to the page shapes above.

export async function fetchPlaylist(
	id: string,
	options?: { signal?: AbortSignal }
): Promise<PlaylistDetailItem> {
	const data = await api.v3.GET(
		v3('/api/v3/playlists/{playlist_id}', { path: { playlist_id: id } }),
		{ signal: options?.signal }
	);
	return toPageDetail(data);
}

export interface BatchRequestResult {
	success: boolean;
	message: string;
	requested: number;
	skipped: number;
}

type TrackIntakeBody = Schemas['TrackIntake'];
type BatchItemBody = { musicbrainz_id: string; artist_name: string; album_title: string };

/** What "request missing" would send: per-track where the row names a recording. */
export interface MissingPlan {
	tracks: TrackIntakeBody[];
	albums: BatchItemBody[];
}

function isMissing(track: PlaylistTrack): boolean {
	// library_file_id means owned locally; a streaming source counts as available.
	return (
		!track.library_file_id && (!track.available_sources || track.available_sources.length === 0)
	);
}

/**
 * Fork (#204, brief §9.3): a missing row whose recording is known (the Spotify
 * import proves it by ISRC) becomes a track request, so only that track is
 * wanted. Rows with a release group but no recording fall back to one album
 * request per album, as upstream does.
 */
export function planMissingRequests(tracks: PlaylistTrack[]): MissingPlan {
	const seenRecordings = new Set<string>();
	const seenAlbums = new Set<string>();
	const plan: MissingPlan = { tracks: [], albums: [] };
	for (const track of tracks) {
		if (!isMissing(track)) continue;
		const recording = track.recording_mbid;
		if (recording) {
			if (seenRecordings.has(recording)) continue;
			seenRecordings.add(recording);
			plan.tracks.push({
				recording_mbid: recording,
				artist_name: track.artist_name || 'Unknown',
				track_title: track.track_name || 'Unknown',
				album_title: track.album_name || null,
				duration_seconds: track.duration ? Math.round(track.duration) : null,
				release_group_mbid: track.album_id || null,
				artist_mbid: null,
				release_mbid: null
			});
			continue;
		}
		const mbid = track.album_id;
		if (!mbid || seenAlbums.has(mbid)) continue;
		seenAlbums.add(mbid);
		plan.albums.push({
			musicbrainz_id: mbid,
			artist_name: track.artist_name || 'Unknown',
			album_title: track.album_name || 'Unknown'
		});
	}
	return plan;
}

export async function requestMissingTracks(tracks: PlaylistTrack[]): Promise<BatchRequestResult> {
	const plan = planMissingRequests(tracks);
	if (plan.tracks.length === 0 && plan.albums.length === 0) {
		return {
			success: true,
			message: 'Nothing missing: every track is in your library or has a source',
			requested: 0,
			skipped: 0
		};
	}
	let requested = 0;
	let skipped = 0;
	for (const body of plan.tracks) {
		try {
			await api.v3.POST(v3('/api/v3/requests/tracks'), body);
			requested += 1;
		} catch {
			skipped += 1;
		}
	}
	if (plan.albums.length > 0) {
		const data = await api.v3.POST(v3('/api/v3/requests/batches'), { items: plan.albums });
		requested += data.requested;
		skipped += data.skipped;
	}
	const parts: string[] = [];
	if (plan.tracks.length)
		parts.push(`${plan.tracks.length} ${plan.tracks.length === 1 ? 'track' : 'tracks'}`);
	if (plan.albums.length)
		parts.push(`${plan.albums.length} ${plan.albums.length === 1 ? 'album' : 'albums'}`);
	return {
		success: skipped === 0,
		message: `Requested ${parts.join(' and ')}` + (skipped ? ` (${skipped} skipped)` : ''),
		requested,
		skipped
	};
}
