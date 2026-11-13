import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';

import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import type { HeldEntry, HeldFile } from '$lib/features/downloads/queries/types';

import HeldGroup from './HeldGroup.svelte';
import { groupHeld } from './heldGroups';

function file(id: number): HeldFile {
	return {
		id,
		original_name: `${id}.flac`,
		release_mbid: 'rel',
		release_track_mbid: `t-${id}`,
		recording_mbid: null,
		disc_number: 1,
		track_number: id,
		track_title: `Song ${id}`,
		file_format: 'flac',
		duration_seconds: 180,
		expected_duration_seconds: null,
		evidence_title: null,
		evidence_artist: null,
		evidence_score: null,
		reason: 'format_not_listed',
		reason_text: 'The files are a higher quality format (FLAC 24-bit/192 kHz).',
		reason_action: 'Import them now if you want them.',
		reason_kind: 'hold',
		reason_detail: null,
		source: 'soulseek',
		source_task_id: null,
		created_at: 1,
		actions: ['import']
	};
}

function entry(id: number, files: HeldFile[]): HeldEntry {
	return {
		id,
		origin: 'download',
		state: 'held',
		release_group_mbid: `rg-${id}`,
		recording_mbid: null,
		drop_item_id: null,
		artist_name: 'Radiohead',
		album_title: `Album ${id}`,
		created_at: 1,
		expires_at: null,
		reasons: [],
		files
	};
}

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'admin-1',
			display_name: 'Admin',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'admin',
			username_display: 'Admin'
		})
	);
});

describe('a group of held albums', () => {
	it('names the artist and count, collapses, and imports every file sharing a reason', async () => {
		const api = fakeApi({
			'POST /api/v3/held/{entry_id}/import': {
				imported: 1,
				already_owned: 0,
				discarded: 0,
				still_held: 0,
				message: null
			}
		});
		const [group] = groupHeld([entry(1, [file(1)]), entry(2, [file(2), file(3)])]);
		await renderWithQuery(HeldGroup, { group });

		await expect.element(page.getByText('Radiohead', { exact: true })).toBeVisible();
		await expect.element(page.getByText('2 albums · 3 files')).toBeVisible();
		await expect.element(page.getByText('Album 1')).toBeVisible();

		await page.getByRole('button', { name: 'Import all 3' }).click();
		await expect.poll(() => api.calls('POST /api/v3/held/{entry_id}/import').length).toBe(2);

		await page
			.getByRole('button', { name: /Radiohead/ })
			.first()
			.click();
		await expect.element(page.getByText('Album 1')).not.toBeInTheDocument();
	});
});
