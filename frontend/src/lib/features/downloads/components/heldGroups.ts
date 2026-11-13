// Fork: To Decide groups albums held for the same artist or from the same
// download, so a discography batch reads as one block with group actions.
import type { HeldEntry, HeldFile } from '$lib/features/downloads/queries/types';

export interface HeldGroup {
	/** Stable key: the first entry's id. */
	key: string;
	/** The artist the group's entries share, when they name one. */
	artist: string | null;
	entries: HeldEntry[];
}

/** Lowercased, accents and punctuation folded away, so "Beyoncé" meets "beyonce". */
export function foldName(name: string | null | undefined): string {
	return (name ?? '')
		.normalize('NFKD')
		.replace(/\p{M}/gu, '')
		.toLowerCase()
		.replace(/[^\p{L}\p{N}]+/gu, ' ')
		.trim();
}

/**
 * Entries joined when they share an artist (folded) or a download task. The
 * list keeps its order: a group sits where its first entry was.
 */
export function groupHeld(entries: HeldEntry[]): HeldGroup[] {
	const parent = entries.map((_, index) => index);
	const find = (index: number): number => {
		while (parent[index] !== index) {
			parent[index] = parent[parent[index]];
			index = parent[index];
		}
		return index;
	};
	const join = (a: number, b: number) => {
		const [ra, rb] = [find(a), find(b)];
		if (ra !== rb) parent[Math.max(ra, rb)] = Math.min(ra, rb);
	};
	const seen = new Map<string, number>();
	entries.forEach((entry, index) => {
		const keys: string[] = [];
		const artist = foldName(entry.artist_name);
		if (artist && artist !== 'various artists') keys.push(`artist:${artist}`);
		for (const file of entry.files) {
			if (file.source_task_id) keys.push(`task:${file.source_task_id}`);
		}
		for (const key of keys) {
			const other = seen.get(key);
			if (other === undefined) seen.set(key, index);
			else join(index, other);
		}
	});
	const groups = new Map<number, HeldGroup>();
	entries.forEach((entry, index) => {
		const root = find(index);
		let group = groups.get(root);
		if (!group) {
			group = { key: String(entry.id), artist: null, entries: [] };
			groups.set(root, group);
		}
		group.entries.push(entry);
	});
	for (const group of groups.values()) {
		const names = new Set(group.entries.map((entry) => foldName(entry.artist_name)));
		group.artist =
			names.size === 1 ? (group.entries.find((e) => e.artist_name)?.artist_name ?? null) : null;
	}
	return [...groups.values()];
}

export interface SharedReason {
	reason: HeldFile['reason'];
	/** The reason in words, from the first entry that names it. */
	text: string;
	/** Files of this reason, by entry, that can be imported as they are. */
	importable: { entryId: number; fileIds: number[]; releaseGroupMbid: string | null }[];
	/** Every file of this reason, by entry. */
	all: { entryId: number; fileIds: number[]; releaseGroupMbid: string | null }[];
	importCount: number;
	fileCount: number;
	/** How many of the group's albums hold files for it. */
	albums: number;
}

/** The reasons two or more of the group's albums hold files for. */
export function sharedReasons(group: HeldGroup): SharedReason[] {
	const byReason = new Map<string, SharedReason>();
	for (const entry of group.entries) {
		if (entry.state !== 'held') continue;
		const summary = new Map((entry.reasons ?? []).map((item) => [item.reason, item.text]));
		const files = new Map<string, HeldFile[]>();
		for (const file of entry.files) {
			files.set(file.reason, [...(files.get(file.reason) ?? []), file]);
		}
		for (const [reason, held] of files) {
			let shared = byReason.get(reason);
			if (!shared) {
				shared = {
					reason: held[0].reason,
					text: summary.get(held[0].reason) ?? held[0].reason_text,
					importable: [],
					all: [],
					importCount: 0,
					fileCount: 0,
					albums: 0
				};
				byReason.set(reason, shared);
			}
			const importIds = held.filter((f) => f.actions.includes('import')).map((f) => f.id);
			const target = { entryId: entry.id, releaseGroupMbid: entry.release_group_mbid ?? null };
			if (importIds.length) shared.importable.push({ ...target, fileIds: importIds });
			shared.all.push({ ...target, fileIds: held.map((f) => f.id) });
			shared.importCount += importIds.length;
			shared.fileCount += held.length;
			shared.albums += 1;
		}
	}
	return [...byReason.values()].filter((shared) => shared.albums >= 2);
}
