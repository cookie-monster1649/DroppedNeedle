import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { UnmatchedFile } from './albumEdition';

const h = vi.hoisted(() => ({ removeMut: vi.fn(), matchMut: vi.fn() }));

vi.mock('$lib/features/library/queries/LibraryMutations.svelte', () => ({
	createRemoveLibraryTrackMutation: () => ({ mutate: h.removeMut, isPending: false })
}));

vi.mock('$lib/features/library/queries/EditionQueries.svelte', () => ({
	getEditionPlanQuery: () => ({
		data: {
			identity_revision: 4,
			edition: { disc_count: 1 },
			tracks: [
				{
					disc_number: 1,
					position: 1,
					title: 'Opening',
					length_ms: 60000,
					local_track_id: 'held',
					release_track_mbid: 'rt-1'
				},
				{
					disc_number: 1,
					position: 2,
					title: 'Arrival in Ashford',
					length_ms: 137000,
					local_track_id: null,
					release_track_mbid: 'rt-2'
				},
				{
					disc_number: 1,
					position: 3,
					title: 'Leaving',
					length_ms: 90000,
					local_track_id: null,
					release_track_mbid: 'rt-3'
				}
			]
		}
	}),
	createMatchEditionFileMutation: () => ({ mutateAsync: h.matchMut, isPending: false })
}));

import UnmatchedFilesSection from './UnmatchedFilesSection.svelte';

type RenderOpts = Parameters<typeof render<typeof UnmatchedFilesSection>>[1];
async function renderSection(files: UnmatchedFile[], canRemove = true) {
	return await render(UnmatchedFilesSection, {
		props: { files, albumMbid: 'rg-1', albumId: 'local-1', canRemove }
	} as unknown as RenderOpts);
}

function orphan(overrides: Partial<UnmatchedFile> = {}): UnmatchedFile {
	return {
		id: 'f-wrong',
		rowRevision: 6,
		title: 'Arrival in Ashford',
		artistName: 'Dan Romer',
		format: 'flac',
		lengthMs: 137240,
		label: 'Another version of track 2',
		note: null,
		...overrides
	};
}

describe('UnmatchedFilesSection', () => {
	beforeEach(() => {
		h.removeMut.mockReset();
		h.matchMut.mockReset();
	});

	it('matches a file to the open edition track with its title', async () => {
		h.matchMut.mockResolvedValue({ identity_revision: 5 });
		await renderSection([orphan()]);
		await page.getByRole('button', { name: /Match to track/ }).click();
		const select = page.getByRole('combobox', { name: /Edition track/ });
		await expect.element(select).toHaveValue('rt-2');
		await page.getByRole('button', { name: /^Match$/ }).click();
		expect(h.matchMut).toHaveBeenCalledWith({
			localId: 'local-1',
			trackId: 'f-wrong',
			releaseTrackMbid: 'rt-2',
			identityRevision: 4
		});
	});

	it('removes a file only after a second, confirming click, and backing out keeps it', async () => {
		await renderSection([orphan()]);
		await page.getByRole('button', { name: /^Remove$/ }).click();
		await page.getByRole('button', { name: /Keep this file/ }).click();
		await expect.element(page.getByRole('button', { name: /^Remove$/ })).toBeVisible();
		expect(h.removeMut).not.toHaveBeenCalled();

		await page.getByRole('button', { name: /^Remove$/ }).click();
		await expect.element(page.getByRole('button', { name: /Remove file/ })).toBeVisible();
		expect(h.removeMut).not.toHaveBeenCalled(); // armed, not fired
		await page.getByRole('button', { name: /Remove file/ }).click();
		expect(h.removeMut).toHaveBeenCalledTimes(1);
		expect(h.removeMut.mock.calls[0][0]).toMatchObject({ fileId: 'f-wrong', expectedRevision: 6 });
	});

	it('offers no removal for a file the status does not list', async () => {
		await renderSection([orphan({ rowRevision: null })]);
		await expect.element(page.getByText(/Arrival in Ashford/)).toBeVisible();
		expect(page.getByRole('button', { name: /^Remove$/ }).elements().length).toBe(0);
	});
});
