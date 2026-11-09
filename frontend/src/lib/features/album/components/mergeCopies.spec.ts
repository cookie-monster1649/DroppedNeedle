import { describe, expect, it } from 'vitest';
import type { TrackView } from '$lib/features/library/queries/types';
import { pickDuplicates, qualityLabel, songKey } from './mergeCopies';

function track(
	id: string,
	album: string,
	title: string,
	extra: Partial<TrackView> = {}
): TrackView {
	return { id, album_id: album, title, row_revision: 1, ...extra } as TrackView;
}

describe('pickDuplicates', () => {
	it('keeps the lossless file of a song held twice', () => {
		const mp3 = track('a', 'keep', 'Kiara', { format: 'mp3', bit_rate: 320000 });
		const flac = track('b', 'fold', 'kiara', { format: 'flac', bit_depth: 16 });
		const lone = track('c', 'fold', 'Kong');
		const [song, ...rest] = pickDuplicates([mp3, flac, lone], 'keep');
		expect(rest).toEqual([]);
		expect(song.keep.id).toBe('b');
		expect(song.discard.map((t) => t.id)).toEqual(['a']);
	});

	it('keeps the file on the staying album on a tie', () => {
		const a = track('a', 'fold', 'Prelude', { format: 'flac' });
		const b = track('b', 'keep', 'Prelude', { format: 'flac' });
		expect(pickDuplicates([a, b], 'keep')[0].keep.id).toBe('b');
	});

	it('folds case, accents and punctuation', () => {
		expect(songKey('Café — (Live)')).toBe(songKey('cafe live'));
	});

	it('labels quality', () => {
		expect(
			qualityLabel(track('a', 'x', 't', { format: 'flac', bit_depth: 24, sample_rate: 96000 }))
		).toBe('FLAC · 24-bit · 96 kHz');
		expect(qualityLabel(track('a', 'x', 't', { format: 'mp3', bit_rate: 320000 }))).toBe(
			'MP3 · 320 kbps'
		);
	});
});
