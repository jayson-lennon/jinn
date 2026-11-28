//! The install set: the manifest the page links, the icons it names, and the
//! browser-bar colour that follows the chosen colour mode.
//!
//! Headless Chromium never shows an install prompt, so this spec asserts
//! what a browser installs *from* — the shell's tags and the manifest route
//! — rather than the install UI itself.

import { expect, pickTheme, test } from "./fixture.mjs";

/** The dark palette's background (`--bg` under `:root`), as the browser
 *  resolves it — the `rgb()` form `theme-color` receives. */
const DARK_BAR = "rgb(71, 74, 80)";
/** The light palette's background (`--bg` under `[data-theme="light"]`). */
const LIGHT_BAR = "rgb(236, 239, 244)";

test("the shell links a manifest that serves and names resolvable icons", async ({
  page,
  jinn,
}) => {
  // Given the page's shell, read from the served document.
  const manifestHref = await page
    .locator('link[rel="manifest"]')
    .getAttribute("href");
  expect(manifestHref).toBe("/manifest.webmanifest");

  // When the manifest is fetched, and each icon it names after it. The
  // request context takes a URL string, so each is resolved against the
  // instance's base and handed over as `.href`.
  const manifestResponse = await page.request.get(
    new URL(manifestHref, jinn.baseUrl).href,
  );
  expect(manifestResponse.status()).toBe(200);
  expect(manifestResponse.headers()["content-type"]).toContain(
    "application/manifest+json",
  );
  const manifest = await manifestResponse.json();
  const iconStatuses = await Promise.all(
    manifest.icons.map((icon) =>
      page.request
        .get(new URL(icon.src, jinn.baseUrl).href)
        .then(async (response) => ({
          status: response.status(),
          type: response.headers()["content-type"],
        })),
    ),
  );

  // Then the manifest installs as a standalone app at the root, and every
  // icon resolves as a PNG.
  expect(manifest.display).toBe("standalone");
  expect(manifest.start_url).toBe("/");
  expect(manifest.scope).toBe("/");
  expect(iconStatuses).toHaveLength(3);
  for (const icon of iconStatuses) {
    expect(icon.status).toBe(200);
    expect(icon.type).toContain("image/png");
  }
});

test("the shell carries the install tags beside the manifest", async ({ page }) => {
  // Given the served page shell.

  // When its head is read.
  const viewport = await page
    .locator('meta[name="viewport"]')
    .getAttribute("content");
  const themeColor = await page
    .locator('meta[name="theme-color"]')
    .getAttribute("content");
  const favicon = await page
    .locator('link[rel="icon"][type="image/png"]')
    .getAttribute("href");
  const touchIcon = await page
    .locator('link[rel="apple-touch-icon"]')
    .getAttribute("href");

  // Then it lets the layout run under the phone's notch and home bar, names
  // a browser-bar colour, and links the two icons the manifest does not.
  expect(viewport).toContain("viewport-fit=cover");
  expect(themeColor).toBeTruthy();
  expect(favicon).toMatch(/^\/vendor\/icon-favicon-32\.[0-9a-f]{64}\.png$/);
  expect(touchIcon).toMatch(/^\/vendor\/icon-apple-touch-180\.[0-9a-f]{64}\.png$/);
});

/** The browser-bar colour the page writes for the colour mode now on it. */
function barColour(page) {
  return page.locator('meta[name="theme-color"]').getAttribute("content");
}

test("the browser bar is the dark palette's background in dark mode", async ({
  page,
}) => {
  // Given a connected page.

  // When Dark is picked.
  await pickTheme(page, "dark");

  // Then the bar is the dark palette's background.
  await expect.poll(() => barColour(page)).toBe(DARK_BAR);
});

test("the browser bar is the light palette's background in light mode", async ({
  page,
}) => {
  // Given a connected page.

  // When Light is picked.
  await pickTheme(page, "light");

  // Then the bar is the light palette's background.
  await expect.poll(() => barColour(page)).toBe(LIGHT_BAR);
});
