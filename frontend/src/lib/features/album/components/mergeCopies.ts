// Fork: duplicate songs across copies of one album, and which file to keep.
import type { TrackView } from '$lib/features/library/queries/types';

export interface DuplicateSong {
	keep: TrackView;
	discard: TrackView[];
}

const LOSSLESS = new Set(['flac', 'alac', 'wav', 'aiff', 'ape', 'wv']);

/** One song: the title folded (case, accents, punctuation and spacing ignored). */
export function songKey(title: string): string {
	return title
		.normalize('NFKD')
		.replace(/\p{M}/gu, '')
		.toLowerCase()
		.replace(/[^\p{L}\p{N}]+/gu, '');
}

/** Higher is better: lossless first, then bit depth, sample rate, bitrate, size. */
function quality(track: TrackView): number[] {
	const format = (track.format ?? '').toLowerCase();
	return [
		LOSSLESS.has(format) ? 1 : 0,
		track.bit_depth ?? 0,
		track.sample_rate ?? 0,
		track.bit_rate ?? 0,
		track.file_size_bytes ?? 0
	];
}

function better(a: TrackView, b: TrackView, keptAlbum: string): number {
	const [left, right] = [quality(a), quality(b)];
	for (let index = 0; index < left.length; index++) {
		if (left[index] !== right[index]) return right[index] - left[index];
	}
	// A tie keeps the file already on the album that stays.
	return Number(b.album_id === keptAlbum) - Number(a.album_id === keptAlbum);
}

/** Songs held more than once, each with its best file first. */
export function pickDuplicates(tracks: TrackView[], keptAlbum: string): DuplicateSong[] {
	const songs = new Map<string, TrackView[]>();
	for (const track of tracks) {
		const key = songKey(track.title) || track.id;
		songs.set(key, [...(songs.get(key) ?? []), track]);
	}
	return [...songs.values()]
		.filter((files) => files.length > 1)
		.map((files) => {
			const [keep, ...discard] = [...files].sort((a, b) => better(a, b, keptAlbum));
			return { keep, discard };
		});
}

/** "FLAC · 24-bit · 96 kHz" or "MP3 · 320 kbps". */
export function qualityLabel(track: TrackView): string {
	const parts = [(track.format ?? 'unknown').toUpperCase()];
	if (track.bit_depth) parts.push(`${track.bit_depth}-bit`);
	if (track.sample_rate) parts.push(`${Math.round(track.sample_rate / 100) / 10} kHz`);
	else if (track.bit_rate) parts.push(`${Math.round(track.bit_rate / 1000)} kbps`);
	return parts.join(' · ');
}
