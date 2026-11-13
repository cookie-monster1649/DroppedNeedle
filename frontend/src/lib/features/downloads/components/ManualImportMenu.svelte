<!--
	Fork: import a file by hand onto a still-hunting song. A small menu:
	upload one audio file, or fetch the song from a YouTube or SoundCloud
	link (pasted, or one the app already knows for it). The file goes in at
	the song's place and the hunt ends; one the library would not take
	waits in To decide.
-->
<script lang="ts">
	import { FileUp, Link, Upload } from 'lucide-svelte';

	import {
		isAllowedLink,
		knownLinks,
		readFetch,
		refreshDownloads,
		startLinkFetch,
		uploadSongFile,
		type KnownLink,
		type ManualFetch
	} from '$lib/features/downloads/queries/manualImport.svelte';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	let { taskId, songTitle }: { taskId: string; songTitle: string } = $props();

	let open = $state(false);
	let mode = $state<'menu' | 'upload' | 'link'>('menu');
	let busy = $state(false);
	let percent = $state<number | null>(null);
	let status = $state<string | null>(null);
	let error = $state<string | null>(null);
	let url = $state('');
	let links = $state<KnownLink[]>([]);
	let fileInput: HTMLInputElement | undefined = $state();

	function reset() {
		mode = 'menu';
		busy = false;
		percent = null;
		status = null;
		error = null;
		url = '';
	}

	function openMenu() {
		reset();
		open = true;
	}

	function failure(err: unknown, fallback: string): string {
		return err instanceof Error && err.message ? err.message : fallback;
	}

	function done(message: string) {
		toastStore.show({ message, type: 'success' });
		open = false;
		void refreshDownloads();
	}

	async function onFile(event: Event) {
		const file = (event.currentTarget as HTMLInputElement).files?.[0];
		if (!file) return;
		mode = 'upload';
		busy = true;
		error = null;
		status = `Uploading ${file.name}`;
		percent = 0;
		try {
			const acted = await uploadSongFile(taskId, file, (sent, total) => {
				percent = total > 0 ? Math.round((sent / total) * 100) : null;
				if (total > 0 && sent >= total) status = 'Importing';
			});
			if (acted.imported > 0) done(`${songTitle} is in your library.`);
			else {
				error = acted.message ?? 'The file was kept in To decide.';
				void refreshDownloads();
			}
		} catch (err) {
			error = failure(err, 'The upload failed.');
		} finally {
			busy = false;
			if (fileInput) fileInput.value = '';
		}
	}

	async function showLinks() {
		mode = 'link';
		error = null;
		try {
			links = (await knownLinks(taskId)).links;
		} catch {
			links = [];
		}
	}

	const linkOk = $derived(isAllowedLink(url));

	async function follow(job: ManualFetch) {
		let current = job;
		while (current.state === 'fetching' || current.state === 'importing') {
			percent = current.percent ?? null;
			status = current.state === 'fetching' ? 'Downloading the audio' : 'Importing';
			await new Promise((resolve) => setTimeout(resolve, 1000));
			current = await readFetch(current.id);
		}
		if (current.state === 'done')
			done(`${songTitle} is in your library (low quality, upgradable).`);
		else {
			error = current.message ?? 'The download failed.';
			void refreshDownloads();
		}
	}

	async function fetchLink(link: string) {
		if (!isAllowedLink(link)) {
			error = 'Paste a youtube.com, youtu.be, music.youtube.com or soundcloud.com link.';
			return;
		}
		busy = true;
		error = null;
		status = 'Starting the download';
		percent = 0;
		try {
			await follow(await startLinkFetch(taskId, link));
		} catch (err) {
			error = failure(err, 'The download failed.');
		} finally {
			busy = false;
		}
	}
</script>

<Button
	variant="outline"
	size="display-sm"
	icon={FileUp}
	onclick={openMenu}
	title="Import a file you have for this song"
>
	Import a file
</Button>

<Modal
	bind:open
	title="Import {songTitle}"
	description="The file goes in at this song's place and the hunt stops."
	size="md"
	dismissible={!busy}
	closeButton
>
	<input
		bind:this={fileInput}
		type="file"
		accept="audio/*,.flac,.mp3,.m4a,.ogg,.opus,.wav"
		class="hidden"
		onchange={onFile}
		data-testid="manual-import-file"
	/>
	{#if mode === 'menu'}
		<div class="flex flex-col gap-2" role="menu">
			<Button variant="outline" icon={Upload} onclick={() => fileInput?.click()} role="menuitem">
				Upload file
			</Button>
			<Button variant="outline" icon={Link} onclick={showLinks} role="menuitem">
				From YouTube / SoundCloud
			</Button>
		</div>
	{:else if mode === 'link'}
		<form
			class="flex flex-col gap-3"
			onsubmit={(event) => {
				event.preventDefault();
				void fetchLink(url);
			}}
		>
			<TextField
				label="Link"
				type="url"
				placeholder="https://www.youtube.com/watch?v=..."
				bind:value={url}
				disabled={busy}
				help="The audio is kept as it comes (Opus, AAC or MP3) and marked low quality, so a better copy replaces it later."
			/>
			<Button type="submit" disabled={busy || !linkOk} loading={busy}>Fetch and import</Button>
			{#if links.length > 0}
				<p class="text-sm font-medium">Links the app knows for this song</p>
				<ul class="flex flex-col gap-1">
					{#each links as link (link.video_id)}
						<li>
							<button
								type="button"
								class="link text-left text-sm"
								disabled={busy}
								onclick={() => {
									url = link.url;
									void fetchLink(link.url);
								}}
							>
								{link.artist_name} - {link.title}
							</button>
						</li>
					{/each}
				</ul>
			{/if}
		</form>
	{/if}

	{#if status && busy}
		<div class="mt-3" aria-live="polite">
			<p class="text-sm">{status}{percent != null ? ` · ${Math.round(percent)}%` : ''}</p>
			<progress class="progress w-full" value={percent ?? undefined} max="100"></progress>
		</div>
	{/if}
	{#if error}
		<p class="text-error mt-3 text-sm" role="alert">{error}</p>
	{/if}
</Modal>
