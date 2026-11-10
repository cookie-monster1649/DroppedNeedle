import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import PlaylistRowStatus from './PlaylistRowStatus.svelte';

describe('PlaylistRowStatus', () => {
	it('shows a missing song with a request button that asks once', async () => {
		const onrequest = vi.fn(() => Promise.resolve());
		render(PlaylistRowStatus, { status: 'missing', title: 'So Long', onrequest });
		await expect.element(page.getByText('Missing')).toBeInTheDocument();
		await page.getByRole('button', { name: 'Request So Long' }).click();
		expect(onrequest).toHaveBeenCalledOnce();
		await expect.element(page.getByRole('button', { name: 'Request So Long' })).toBeDisabled();
	});

	it('shows a fetch on its way without a request button', async () => {
		render(PlaylistRowStatus, { status: 'fetching', title: 'Elevator', onrequest: vi.fn() });
		await expect.element(page.getByText('Fetching')).toBeInTheDocument();
		expect(page.getByRole('button', { name: 'Request Elevator' }).query()).toBeNull();
	});

	it('marks a removed song and lets it be fetched anew', async () => {
		const onrequest = vi.fn(() => Promise.resolve());
		render(PlaylistRowStatus, { status: 'removed', title: 'Wrong', onrequest });
		await expect.element(page.getByText('Removed')).toBeInTheDocument();
		await page.getByRole('button', { name: 'Request Wrong' }).click();
		expect(onrequest).toHaveBeenCalledOnce();
	});

	it('points a held song at To decide', async () => {
		render(PlaylistRowStatus, { status: 'held', title: 'Peace' });
		await expect.element(page.getByRole('link', { name: /held/ })).toBeInTheDocument();
	});
});
