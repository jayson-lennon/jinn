// The page's color mode: one attribute the stylesheet reads, one hint the
// next load reads.
//
// The store's theme is the truth — the snapshot seeds it and a `theme`
// frame moves it. `applyTheme` writes it where the stylesheet can see it,
// and `mirrorTheme` copies it into local storage so the inline script in
// the page shell can set the attribute *before* the stylesheet loads. That
// script is the only thing that reads the hint; a light-mode user would
// otherwise see a dark flash on every load, because the bundle's own
// stylesheet ships dark at `:root`.

import type { Theme } from './protocol/protocol';

/** The local-storage key the pre-paint hint lives under. */
const HINT_KEY = 'jinn.web2.theme';

/** The three modes the page accepts; anything else reads as `system`. */
const THEMES: readonly Theme[] = ['light', 'dark', 'system'];

/** Whether a string is one of the three modes. */
export function isTheme(value: unknown): value is Theme {
  return typeof value === 'string' && (THEMES as readonly string[]).includes(value);
}

/** Sets the color mode on the document element, for the stylesheet. */
export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
}

/** Copies the color mode into local storage, for the next load's first paint. */
export function mirrorTheme(theme: Theme): void {
  try {
    window.localStorage.setItem(HINT_KEY, theme);
  } catch {
    // Storage can be refused (private mode, a quota); the hint is an
    // optimization and the store's theme still applies once connected.
  }
}

/** The pre-paint hint's key, shared with the page shell's inline script. */
export const THEME_HINT_KEY = HINT_KEY;
