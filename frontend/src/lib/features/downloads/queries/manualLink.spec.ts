import { describe, expect, it } from 'vitest';

import { isAllowedLink } from './manualImport.svelte';

describe('isAllowedLink', () => {
	it('takes the four sites only', () => {
		for (const good of [
			'https://www.youtube.com/watch?v=abc',
			'https://youtu.be/abc',
			'https://music.youtube.com/watch?v=abc',
			'https://soundcloud.com/a/b'
		]) {
			expect(isAllowedLink(good), good).toBe(true);
		}
		for (const bad of [
			'https://youtube.com.evil.com/x',
			'https://evil.com/?u=youtube.com',
			'https://a:b@youtube.com/x',
			'https://youtube.com:444/x',
			'javascript:alert(1)',
			'not a link'
		]) {
			expect(isAllowedLink(bad), bad).toBe(false);
		}
	});
});
