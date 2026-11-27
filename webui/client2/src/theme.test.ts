// The page's color mode guard: which strings name one of the three modes.

import { describe, it, expect } from 'vitest';
import { isTheme } from './theme';

describe('isTheme', () => {
  it.each(['light', 'dark', 'system'])('%s is a mode', (mode) => {
    // Given one of the three mode names.
    // When asking whether it names a mode.
    const yes = isTheme(mode);

    // Then it does.
    expect(yes).toBe(true);
  });

  it.each([
    ['an unknown string', 'neon'],
    ['an empty string', ''],
    ['null', null],
    ['a number', 3],
  ])('%s is not a mode', (_label, value) => {
    // Given a value that is not one of the three names.
    // When asking whether it names a mode.
    const yes = isTheme(value);

    // Then it does not.
    expect(yes).toBe(false);
  });
});
