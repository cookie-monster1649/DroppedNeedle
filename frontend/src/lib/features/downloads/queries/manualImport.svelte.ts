// Fork: import a file by hand onto a still-hunting song: an upload, or
// audio fetched from a YouTube or SoundCloud link.
import type { components } from '$lib/api/v3/openapi';

import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { applyEffects, downloadsChanged } from '$lib/queries/effects';

export type ManualFetch = components['schemas']['ManualFetch'];
export type KnownLink = components['schemas']['KnownLink'];
export type HeldAction = components['schemas']['HeldActionResponse'];

/** The links accepted, checked again on the server. */
const HOSTS = [
	'youtube.com',
	'www.youtube.com',
	'm.youtube.com',
	'music.youtube.com',
	'youtu.be',
	'soundcloud.com',
	'm.soundcloud.com'
];

export function isAllowedLink(raw: string): boolean {
	try {
		// A one-off parse, never rendered: no reactivity wanted.
		// eslint-disable-next-line svelte/prefer-svelte-reactivity
		const url = new URL(raw.trim());
		return (
			(url.protocol === 'https:' || url.protocol === 'http:') &&
			!url.username &&
			!url.password &&
			!url.port &&
			HOSTS.includes(url.hostname.toLowerCase())
		);
	} catch {
		return false;
	}
}

export function refreshDownloads(): Promise<void> {
	return applyEffects(downloadsChanged(authStore.user?.id));
}

export function uploadSongFile(
	taskId: string,
	file: File,
	onProgress: (sent: number, total: number) => void
): Promise<HeldAction> {
	const form = new FormData();
	form.append('file', file, file.name);
	return api.upload<HeldAction>(
		v3('/api/v3/downloads/{task_id}/manual-import/upload', { path: { task_id: taskId } }),
		form,
		{ onProgress, stallMs: 60_000 }
	);
}

export function knownLinks(taskId: string): Promise<{ links: KnownLink[] }> {
	return api.v3.GET(
		v3('/api/v3/downloads/{task_id}/manual-import/links', { path: { task_id: taskId } })
	);
}

export function startLinkFetch(taskId: string, url: string): Promise<ManualFetch> {
	return api.v3.POST(
		v3('/api/v3/downloads/{task_id}/manual-import/link', { path: { task_id: taskId } }),
		{ url: url.trim() }
	);
}

export function readFetch(fetchId: string): Promise<ManualFetch> {
	return api.v3.GET(
		v3('/api/v3/downloads/manual-import/fetches/{fetch_id}', { path: { fetch_id: fetchId } })
	);
}
