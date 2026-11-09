// Fork: one edition of an album in a line: "Deluxe edition · 2017 · GB · CD · 16 tracks".
import type { components } from '$lib/api/v3/openapi';

export type OtherEdition = components['schemas']['OtherEdition'];

export function editionLabel(edition: OtherEdition): string {
	const parts: string[] = [];
	if (edition.disambiguation) parts.push(edition.disambiguation);
	const year = edition.date?.slice(0, 4);
	if (year) parts.push(year);
	if (edition.country) parts.push(edition.country);
	const formats = [...new Set(edition.formats)].join(' + ');
	if (formats) parts.push(formats);
	parts.push(`${edition.track_count} ${edition.track_count === 1 ? 'track' : 'tracks'}`);
	return parts.join(' · ');
}
