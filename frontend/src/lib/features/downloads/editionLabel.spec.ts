import { describe, expect, it } from 'vitest';
import { editionLabel } from './editionLabel';

describe('editionLabel', () => {
	it('names what tells the edition apart', () => {
		expect(
			editionLabel({
				release_mbid: 'x',
				title: 'Needle Paw',
				date: '2017-10-27',
				country: 'GB',
				formats: ['CD', 'CD'],
				track_count: 13,
				disambiguation: 'deluxe'
			})
		).toBe('deluxe · 2017 · GB · CD · 13 tracks');
		expect(editionLabel({ release_mbid: 'x', title: 'T', formats: [], track_count: 1 })).toBe(
			'1 track'
		);
	});
});
