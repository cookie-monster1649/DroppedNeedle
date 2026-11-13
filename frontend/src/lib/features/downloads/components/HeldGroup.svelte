<script lang="ts">
	// Fork: several albums held for one artist or from one download, as one
	// collapsible block. Group actions take every file sharing a reason
	// across the albums; each album and file keeps its own decisions below.
	import { Check, ChevronDown, Trash2 } from 'lucide-svelte';

	import {
		createDiscardHeldMutation,
		createImportHeldMutation
	} from '$lib/features/downloads/queries/HeldQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';

	import HeldEntryCard from './HeldEntryCard.svelte';
	import { sharedReasons, type HeldGroup, type SharedReason } from './heldGroups';

	interface Props {
		group: HeldGroup;
		/** The task that can hunt again, per entry. */
		retryTaskOf?: (entry: HeldGroup['entries'][number]) => string | null;
	}

	let { group, retryTaskOf = () => null }: Props = $props();

	const importMut = createImportHeldMutation();
	const discardMut = createDiscardHeldMutation();

	let open = $state(true);
	let pending = $state(false);
	let discarding = $state<SharedReason | null>(null);
	let discardOpen = $state(false);

	const fileCount = $derived(group.entries.reduce((sum, entry) => sum + entry.files.length, 0));
	const title = $derived(group.artist ?? 'Same download');
	const shared = $derived(sharedReasons(group));

	async function importAll(reason: SharedReason): Promise<void> {
		pending = true;
		try {
			for (const target of reason.importable) await importMut.mutateAsync(target);
		} finally {
			pending = false;
		}
	}

	async function discardAll(): Promise<void> {
		if (!discarding) return;
		pending = true;
		try {
			for (const target of discarding.all) await discardMut.mutateAsync(target);
		} finally {
			pending = false;
		}
	}
</script>

<section
	class="rounded-md border border-warning/40 p-2 sm:p-3"
	aria-label="{title}: {group.entries.length} albums"
>
	<button
		type="button"
		class="flex w-full items-center justify-between gap-3 text-left"
		onclick={() => (open = !open)}
		aria-expanded={open}
	>
		<span class="min-w-0">
			<span class="block truncate text-base font-bold">{title}</span>
			<span class="font-mono text-xs tracking-wide text-muted uppercase">
				{group.entries.length} albums · {fileCount}
				{fileCount === 1 ? 'file' : 'files'}
			</span>
		</span>
		<ChevronDown
			class="size-5 shrink-0 transition-transform motion-reduce:transition-none {open
				? 'rotate-180'
				: ''}"
			aria-hidden="true"
		/>
	</button>

	{#if shared.length > 0}
		<ul class="mt-2 space-y-2">
			{#each shared as reason (reason.reason)}
				<li class="flex flex-wrap items-center gap-2 text-sm">
					<span class="min-w-0 flex-1 text-base-content/75">
						{reason.text}
						<span class="text-muted">({reason.fileCount} files in {reason.albums} albums)</span>
					</span>
					{#if reason.importCount > 0}
						<Button
							variant="primary"
							size="xs"
							icon={Check}
							disabled={pending}
							onclick={() => importAll(reason)}
						>
							Import all {reason.importCount}
						</Button>
					{/if}
					<Button
						variant="ghost"
						size="xs"
						icon={Trash2}
						disabled={pending}
						onclick={() => {
							discarding = reason;
							discardOpen = true;
						}}
					>
						Discard all {reason.fileCount}
					</Button>
				</li>
			{/each}
		</ul>
	{/if}

	{#if open}
		<div class="mt-3 space-y-3">
			{#each group.entries as entry (entry.id)}
				<HeldEntryCard {entry} retryTaskId={retryTaskOf(entry)} />
			{/each}
		</div>
	{/if}
</section>

<ConfirmDialog
	bind:open={discardOpen}
	title="Discard {discarding?.fileCount ?? 0} files from {discarding?.albums ?? 0} albums?"
	message="They are deleted. Nothing in your library changes."
	confirmLabel="Discard"
	tone="danger"
	{pending}
	onconfirm={discardAll}
/>
