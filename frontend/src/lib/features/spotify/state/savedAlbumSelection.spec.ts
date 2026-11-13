import { describe, expect, it } from 'vitest';
import {
	BATCH_LIMIT,
	initialSelection,
	isSelectable,
	requestBatches,
	selectAll,
	toggle,
	type SavedAlbumLike
} from './savedAlbumSelection';

const album = (id: string, extra: Partial<SavedAlbumLike> = {}): SavedAlbumLike => ({
	spotify_album_id: id,
	title: `Album ${id}`,
	artist_name: 'Artist',
	year: 2020,
	release_group: `rg-${id}`,
	held: false,
	requested: false,
	...extra
});

describe('saved album selection', () => {
	const albums = [
		album('a'),
		album('b'),
		album('missing', { release_group: null }),
		album('held', { held: true }),
		album('asked', { requested: true })
	];

	it('starts with every requestable album selected', () => {
		expect([...initialSelection(albums)].sort()).toEqual(['a', 'b']);
	});

	it('never selects an album not found, held or requested', () => {
		expect(isSelectable(albums[2])).toBe(false);
		expect(isSelectable(albums[3])).toBe(false);
		expect(isSelectable(albums[4])).toBe(false);
		const selection = toggle(initialSelection(albums), albums[2], true);
		expect(selection.has('missing')).toBe(false);
	});

	it('deselects and reselects', () => {
		let selection = toggle(initialSelection(albums), albums[0], false);
		expect([...selection]).toEqual(['b']);
		selection = toggle(selection, albums[0], true);
		expect(selection.has('a')).toBe(true);
		expect(selectAll(albums, false).size).toBe(0);
		expect(selectAll(albums, true).size).toBe(2);
	});

	it('requests only what is still selected, once per album', () => {
		const twin = album('twin', { release_group: 'rg-a' });
		const selection = toggle(initialSelection([...albums, twin]), albums[1], false);
		const batches = requestBatches([...albums, twin], selection);
		expect(batches).toEqual([
			[{ musicbrainz_id: 'rg-a', artist_name: 'Artist', album_title: 'Album a', year: 2020 }]
		]);
	});

	it('splits into batches of at most 500', () => {
		const many = Array.from({ length: BATCH_LIMIT + 3 }, (_, i) => album(String(i)));
		const batches = requestBatches(many, initialSelection(many));
		expect(batches.map((batch) => batch.length)).toEqual([BATCH_LIMIT, 3]);
	});
});
