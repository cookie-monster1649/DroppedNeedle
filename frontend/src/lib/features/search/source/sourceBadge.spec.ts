import { describe, expect, it } from 'vitest';
import { sourceBadge } from './sourceBadge';

describe('sourceBadge', () => {
	it('names Deezer for stand-in ids only', () => {
		expect(sourceBadge('d0ab1e00-50c3-8376-a7e9-bc29e846707c')).toBe('Deezer');
		expect(sourceBadge('D0AB1E00-50C3-8376-A7E9-BC29E846707C')).toBe('Deezer');
		expect(sourceBadge('b1392450-e666-3926-a536-22c65f834433')).toBeNull();
		expect(sourceBadge(null)).toBeNull();
	});
});
