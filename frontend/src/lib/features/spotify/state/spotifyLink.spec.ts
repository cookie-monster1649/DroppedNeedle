import { describe, expect, it } from 'vitest';
import { notFoundMessage, parseSpotifyLink, resolutionPath } from './spotifyLink';

const ID = '4uLU6hMCjMI75M1A2tKUQC';

describe('parseSpotifyLink', () => {
	it.each([
		`https://open.spotify.com/track/${ID}`,
		`https://open.spotify.com/track/${ID}?si=abc123`,
		`https://open.spotify.com/intl-de/track/${ID}?si=abc&utm=x`,
		`http://open.spotify.com/track/${ID}/`,
		`open.spotify.com/track/${ID}`,
		`  https://open.spotify.com/embed/track/${ID}  `,
		`spotify:track:${ID}`
	])('reads a track from %s', (input) => {
		expect(parseSpotifyLink(input)).toEqual({ kind: 'track', id: ID });
	});

	it('reads albums and artists', () => {
		expect(parseSpotifyLink(`https://open.spotify.com/album/${ID}`)?.kind).toBe('album');
		expect(parseSpotifyLink(`spotify:artist:${ID}`)?.kind).toBe('artist');
		expect(parseSpotifyLink(`https://open.spotify.com/intl-pt-br/artist/${ID}#x`)?.kind).toBe(
			'artist'
		);
	});

	it.each([
		'radiohead',
		`https://open.spotify.com/playlist/${ID}`,
		`spotify:playlist:${ID}`,
		'https://open.spotify.com/track/short',
		`https://example.com/track/${ID}`,
		`https://open.spotify.com/track/${ID}/extra`,
		`spotify:track:${ID}:more`,
		''
	])('ignores %s', (input) => {
		expect(parseSpotifyLink(input)).toBeNull();
	});
});

describe('resolutionPath', () => {
	it('opens a song on its album, marked', () => {
		expect(
			resolutionPath({ kind: 'track', found: true, name: 'x', album_id: 'g', recording_id: 'r' })
		).toBe('/album/g?track=r');
	});
	it('opens albums and artists', () => {
		expect(resolutionPath({ kind: 'album', found: true, name: 'x', album_id: 'g' })).toBe(
			'/album/g'
		);
		expect(resolutionPath({ kind: 'artist', found: true, name: 'x', artist_id: 'a' })).toBe(
			'/artist/a'
		);
	});
	it('opens nothing without a match', () => {
		expect(resolutionPath({ kind: 'album', found: false, name: 'x' })).toBeNull();
		expect(notFoundMessage('In Rainbows')).toBe(
			'Couldn\'t find "In Rainbows" on MusicBrainz or Deezer'
		);
	});
});
