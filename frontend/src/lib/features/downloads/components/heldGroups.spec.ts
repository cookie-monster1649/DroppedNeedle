import { describe, expect, it } from 'vitest';

import type { HeldEntry, HeldFile } from '$lib/features/downloads/queries/types';

import { foldName, groupHeld, sharedReasons } from './heldGroups';

function file(id: number, over: Partial<HeldFile> = {}): HeldFile {
	return {
		id,
		original_name: `${id}.flac`,
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: null,
		disc_number: 1,
		track_number: null,
		track_title: null,
		file_format: 'flac',
		duration_seconds: 180,
		expected_duration_seconds: null,
		evidence_title: null,
		evidence_artist: null,
		evidence_score: null,
		reason: 'format_not_listed',
		reason_text: 'The files are FLAC.',
		reason_action: 'Import them.',
		reason_kind: 'hold',
		reason_detail: null,
		source: 'soulseek',
		source_task_id: null,
		created_at: 1,
		actions: ['import'],
		...over
	};
}

function entry(id: number, artist: string | null, files: HeldFile[]): HeldEntry {
	return {
		id,
		origin: 'download',
		state: 'held',
		release_group_mbid: `rg-${id}`,
		recording_mbid: null,
		drop_item_id: null,
		artist_name: artist,
		album_title: `Album ${id}`,
		created_at: 1,
		expires_at: null,
		reasons: [],
		files
	};
}

describe('groupHeld', () => {
	it('joins albums of one artist, folded, and keeps the list order', () => {
		const groups = groupHeld([
			entry(1, 'Beyoncé', [file(1)]),
			entry(2, 'Jack Johnson', [file(2)]),
			entry(3, 'beyonce', [file(3)])
		]);
		expect(groups.map((g) => g.entries.map((e) => e.id))).toEqual([[1, 3], [2]]);
		expect(groups[0].artist).toBe('Beyoncé');
	});

	it('joins entries of one download even across artists, and never groups various artists', () => {
		const groups = groupHeld([
			entry(1, 'A', [file(1, { source_task_id: 't1' })]),
			entry(2, 'B', [file(2, { source_task_id: 't1' })]),
			entry(3, 'Various Artists', [file(3)]),
			entry(4, 'Various Artists', [file(4)])
		]);
		expect(groups.map((g) => g.entries.map((e) => e.id))).toEqual([[1, 2], [3], [4]]);
		expect(groups[0].artist).toBeNull();
	});

	it('folds names', () => {
		expect(foldName('  Sigur Rós! ')).toBe('sigur ros');
	});
});

describe('sharedReasons', () => {
	it('offers group actions only for reasons two or more albums share', () => {
		const [group] = groupHeld([
			entry(1, 'X', [file(1), file(2, { reason: 'weak_match', actions: [] })]),
			entry(2, 'X', [file(3), file(4, { actions: [] })])
		]);
		const shared = sharedReasons(group);
		expect(shared).toHaveLength(1);
		expect(shared[0].reason).toBe('format_not_listed');
		expect(shared[0].importCount).toBe(2);
		expect(shared[0].fileCount).toBe(3);
		expect(shared[0].albums).toBe(2);
		expect(shared[0].importable).toEqual([
			{ entryId: 1, releaseGroupMbid: 'rg-1', fileIds: [1] },
			{ entryId: 2, releaseGroupMbid: 'rg-2', fileIds: [3] }
		]);
		expect(shared[0].all.map((t) => t.fileIds)).toEqual([[1], [3, 4]]);
	});
});
