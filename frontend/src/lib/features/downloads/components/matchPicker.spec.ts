import { describe, expect, it } from 'vitest';
import type { Album } from '$lib/features/album/queries/types';
import { matchMoved, pickerAlbums } from './matchPicker';

const album = (id: string): Album => ({ musicbrainz_id: id, title: id }) as Album;

describe('pickerAlbums', () => {
	it('lists MusicBrainz albums before Deezer ones', () => {
		const deezer = 'd0ab1e00-50c3-8376-a7e9-bc29e846707c';
		const ordered = pickerAlbums([album(deezer), album('mb-1'), album('mb-2')]);
		expect(ordered.map((a) => a.musicbrainz_id)).toEqual(['mb-1', 'mb-2', deezer]);
	});
});

describe('matchMoved', () => {
	it('is false when no file went in', () => {
		expect(matchMoved({ imported: 0, already_owned: 0 })).toBe(false);
		expect(matchMoved({ imported: 0, already_owned: 2 })).toBe(true);
		expect(matchMoved({ imported: 3, already_owned: 0 })).toBe(true);
	});
});
