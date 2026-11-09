<!--
	Fork: the "In Library · 1/15" chip of a partly held album, as a button.
	It asks "Request album?" and, once confirmed, fetches every track of the
	edition no file holds (the edition fetch, with the files already held
	left as they are).
-->
<script lang="ts">
	import { Check, Download } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { createAcquireEditionMutation } from '$lib/features/library/queries/EditionQueries.svelte';

	interface Props {
		/** Release-group MBID. */
		mbid: string;
		/** The library copy the edition plan counts. */
		copyId?: string;
		kept: number;
		total: number;
	}

	let { mbid, copyId, kept, total }: Props = $props();

	const acquire = createAcquireEditionMutation();
	let open = $state(false);
	const missing = $derived(Math.max(total - kept, 0));

	async function confirm(): Promise<void> {
		try {
			const answer = await acquire.mutateAsync({ mbid, copyId, replaceVersions: false });
			open = false;
			toastStore.show({
				message:
					answer.status === 'started'
						? `Requesting ${answer.requested ?? missing} missing ${(answer.requested ?? missing) === 1 ? 'track' : 'tracks'}.`
						: answer.message,
				type: answer.status === 'started' ? 'success' : 'info'
			});
		} catch (error) {
			toastStore.show({
				message: error instanceof Error ? error.message : "The album couldn't be requested",
				type: 'error'
			});
		}
	}
</script>

<button
	type="button"
	class="inline-flex h-10 shrink-0 cursor-pointer items-center gap-1.5 rounded-field bg-warning/15 px-3.5 text-sm leading-none font-semibold whitespace-nowrap text-warning hover:bg-warning/25 [&_svg]:size-4"
	title="Request the missing tracks"
	onclick={() => (open = true)}
>
	<Check class="size-4" />
	In Library · {kept}/{total}
</button>

<Modal bind:open title="Request album?" icon={Download} size="sm">
	<p class="text-sm">
		Fetch the {missing}
		{missing === 1 ? 'track' : 'tracks'} of this album you don't have yet. The {kept} you have stay as
		they are.
	</p>
	{#snippet actions()}
		<Button variant="ghost" onclick={() => (open = false)} disabled={acquire.isPending}
			>Cancel</Button
		>
		<Button variant="primary" icon={Download} loading={acquire.isPending} onclick={confirm}>
			Request album
		</Button>
	{/snippet}
</Modal>
