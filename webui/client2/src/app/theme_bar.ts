// The phone's browser-bar colour: the page's own background, read back from
// the stylesheet under the active colour mode.
//
// There is no colour table here. The palettes own every colour as a token,
// and `--bg` under the active theme is the bar's colour — read from the
// stylesheet after `applyTheme` has set the attribute, so light, dark, and
// system (through the browser's own media query) are each right without a
// second copy of the palette that could drift.
//
// The meta is parsed as a CSS `<color>`, so the `rgb(r, g, b)` form
// `getComputedStyle` returns is accepted as it is; no conversion is needed.

/** The meta tag whose content is the browser bar's colour. */
const THEME_COLOR_NAME = 'theme-color';

/** Writes the browser-bar colour for the colour mode now on the document.
 *
 *  Must run after `applyTheme`: it resolves the colour the attribute
 *  selects. A page whose shell carries no such meta is left alone. */
export function applyThemeBar(): void {
  const meta = document.querySelector<HTMLMetaElement>(`meta[name="${THEME_COLOR_NAME}"]`);
  if (meta === null) return;
  const color = getComputedStyle(document.body).backgroundColor;
  if (color.length > 0) meta.content = color;
}
