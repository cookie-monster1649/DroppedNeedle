/**
 * Operations in the API document that no builder in the web app names, each
 * with the reason it stays. `route-coverage.spec.ts` reads this list.
 */
export const ROUTE_REASONS: readonly { method: string; path: string; reason: string }[] = [
	{ method: 'GET', path: '/health', reason: 'container health check' },
	{ method: 'GET', path: '/openapi.json', reason: 'the served API document' },
	{
		method: 'POST',
		path: '/api/v3/auth/device-sessions',
		reason: 'sign-in for devices and scripts'
	},
	{ method: 'GET', path: '/api/v3/wrapped/server', reason: 'key-gated API for outside tools' },
	{ method: 'GET', path: '/api/v3/wrapped/users', reason: 'key-gated API for outside tools' },
	{
		method: 'GET',
		path: '/api/v3/wrapped/user/{user_id}',
		reason: 'key-gated API for outside tools'
	},
	{
		method: 'GET',
		path: '/api/v3/library/contributions/musicbrainz/callback',
		reason: 'MusicBrainz sends the browser back here'
	},
	{
		method: 'POST',
		path: '/api/v3/auth/app-connect/token',
		reason: 'a native app trades its one-time code here'
	},
	{
		method: 'GET',
		path: '/api/v3/auth/oidc/callback',
		reason: 'the identity provider sends the browser back here'
	},
	{
		method: 'GET',
		path: '/api/v3/acquire/spotify/auth/callback',
		reason: 'Spotify sends the browser back here'
	},
	{ method: 'POST', path: '/api/v3/plugins/ext/{name}/{subpath}', reason: 'plugin routes' },
	{ method: 'DELETE', path: '/api/v3/plugins/ext/{name}/{subpath}', reason: 'plugin routes' },
	{
		method: 'GET',
		path: '/api/v3/stream/{source}/{*key}',
		reason: 'audio URL built by hand: a Plex key keeps its slashes'
	},
	{
		method: 'HEAD',
		path: '/api/v3/stream/{source}/{*key}',
		reason: 'audio URL built by hand: a Plex key keeps its slashes'
	},
	{
		method: 'GET',
		path: '/api/v3/users/{id}/avatar',
		reason: 'the server hands the browser this URL'
	},
	{
		method: 'GET',
		path: '/api/v3/remotes/{source}/images/{id}',
		reason: 'the server hands the browser this URL'
	},
	{
		method: 'GET',
		path: '/api/v3/remotes/{source}/covers/playlists/{id}',
		reason: 'the server hands the browser this URL'
	},
	// Fork: live playlist folders, set by an admin through the API (no UI yet).
	{
		method: 'GET',
		path: '/api/v3/settings/library/playlist-folders',
		reason: 'fork setting read by scripts; no settings page yet'
	},
	{
		method: 'PUT',
		path: '/api/v3/settings/library/playlist-folders',
		reason: 'fork setting saved by scripts; no settings page yet'
	},
	// Fork: the one-off playlist-folder import, run by an admin through the API.
	{
		method: 'POST',
		path: '/api/v3/admin/playlist-import/dry-run',
		reason: 'fork admin tool run by scripts; no page'
	},
	{
		method: 'POST',
		path: '/api/v3/admin/playlist-import/run',
		reason: 'fork admin tool run by scripts; no page'
	},
	{
		method: 'GET',
		path: '/api/v3/admin/playlist-import/status',
		reason: 'fork admin tool read by scripts; no page'
	},
	{
		method: 'POST',
		path: '/api/v3/admin/playlist-import/stop',
		reason: 'fork admin tool run by scripts; no page'
	},
	{
		method: 'POST',
		path: '/api/v3/admin/playlist-import/release-hold',
		reason: 'fork admin tool run by scripts; no page'
	},
	{
		method: 'POST',
		path: '/api/v3/admin/playlist-import/link-ids',
		reason: 'fork admin tool run by scripts; no page'
	},
	{
		method: 'GET',
		path: '/api/v3/admin/bulk-requests',
		reason: 'fork bulk-request tool driven by scripts; no page'
	},
	{
		method: 'GET',
		path: '/api/v3/admin/bulk-requests/{run_id}',
		reason: 'fork bulk-request tool driven by scripts; no page'
	},
	{
		method: 'POST',
		path: '/api/v3/admin/bulk-requests/{run_id}/state',
		reason: 'fork bulk-request tool driven by scripts; no page'
	}
];
