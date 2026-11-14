// Fork: whether ListenBrainz answered lately. A list that asked ListenBrainz
// and heard it is unavailable marks it down; one it answered marks it up.
// A mark goes stale after a few minutes, so the source switcher offers
// ListenBrainz again even where nothing asks it in between.
import type { MusicSource } from '$lib/stores/musicSource';

export const DOWN_FOR_MS = 5 * 60 * 1000;

let downSince = $state<number | null>(null);
let now = $state(Date.now());
let timer: ReturnType<typeof setTimeout> | null = null;

function schedule(): void {
	if (timer) clearTimeout(timer);
	timer = null;
	if (downSince === null) return;
	const left = downSince + DOWN_FOR_MS - Date.now();
	timer = setTimeout(
		() => {
			now = Date.now();
			timer = null;
		},
		Math.max(left, 0) + 10
	);
}

export const listenSourceHealth = {
	/** The sources unavailable right now. */
	get unavailable(): MusicSource[] {
		return downSince !== null && now - downSince < DOWN_FOR_MS ? ['listenbrainz'] : [];
	},
	markDown(): void {
		downSince = Date.now();
		now = downSince;
		schedule();
	},
	markUp(): void {
		downSince = null;
		schedule();
	},
	/** Read what an answer says about ListenBrainz. */
	observe(answer: { source: string; unavailable_sources: string[] } | null | undefined): void {
		if (!answer) return;
		if (answer.unavailable_sources.includes('listenbrainz')) this.markDown();
		else if (answer.source === 'listenbrainz' && downSince !== null) this.markUp();
	}
};
