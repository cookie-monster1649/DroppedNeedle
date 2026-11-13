// Fork: results that come from a metadata source other than MusicBrainz.
// The server gives them stand-in ids that all begin with this marker
// (providers `virtual_ids`); MusicBrainz never issues such an id.
const STAND_IN_PREFIX = 'd0ab1e00-50c3-8';

/** The source a result's id names, or null for a MusicBrainz id. */
export function sourceBadge(id: string | null | undefined): string | null {
	return id && id.toLowerCase().startsWith(STAND_IN_PREFIX) ? 'Deezer' : null;
}
