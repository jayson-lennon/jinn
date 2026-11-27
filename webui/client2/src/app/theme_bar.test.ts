// @vitest-environment jsdom
// The browser-bar colour: the meta receives whatever the stylesheet resolves
// the page's background to under the active colour mode.
//
// jsdom implements `getComputedStyle` but does not run the page's
// stylesheets into the cascade, so these tests set the body's background
// inline and assert the meta takes the value jsdom resolves — the
// mechanism, not the palette. The palette-to-bar correctness is covered
// through the real page in the e2e tier.

import { describe, it, expect, beforeEach } from 'vitest';
import { applyThemeBar } from './theme_bar';

/** The meta tag the shell ships, as the page's own head would carry it. */
function shellHead(): string {
  return '<meta name="viewport" content="width=device-width"><meta name="theme-color" content="#25272e">';
}

describe('applyThemeBar', () => {
  beforeEach(() => {
    document.head.innerHTML = shellHead();
    document.body.style.background = '';
  });

  it('writes the body\'s resolved background into the theme-color meta', () => {
    // Given a body whose background the stylesheet has resolved.
    document.body.style.background = 'rgb(236, 239, 244)';

    // When the bar colour is applied.
    applyThemeBar();

    // Then the meta carries that colour.
    const meta = document.querySelector<HTMLMetaElement>('meta[name="theme-color"]');
    expect(meta?.content).toBe('rgb(236, 239, 244)');
  });

  it('leaves a page without the meta alone', () => {
    // Given a shell that carries no theme-color meta.
    document.head.innerHTML = '<meta name="viewport" content="width=device-width">';
    document.body.style.background = 'rgb(1, 2, 3)';

    // When the bar colour is applied.
    const applied = (): void => applyThemeBar();

    // Then nothing throws and no meta appears.
    expect(applied).not.toThrow();
    expect(document.querySelector('meta[name="theme-color"]')).toBeNull();
  });
});
