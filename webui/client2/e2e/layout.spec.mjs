//! The page's layout invariants: what stays in view no matter how long the
//! transcript or a title gets.
//!
//! Each test measures real geometry. A rule that exists in the stylesheet
//! but does not actually clamp the layout fails here, where a screenshot
//! review would miss it.

import { createSession, expect, sel, sendAndWaitForEcho, test, openProviderPicker } from "./fixture.mjs";

const DESKTOP = { width: 1280, height: 800 };

/** A title far longer than the rail's 260px column. */
const LONG_TITLE =
  "a session title so long that it could never fit inside the rail's column without being cut off somewhere";

test("the composer stays in the viewport below a long transcript", async ({ page }) => {
  // Given a transcript long enough to scroll many times over.
  await page.setViewportSize(DESKTOP);
  for (let at = 0; at < 40; at += 1) {
    await sendAndWaitForEcho(page, `message ${at}`);
  }

  // Then the composer is entirely inside the viewport.
  const box = await page.locator('[data-testid="composer"]').boundingBox();
  const viewport = page.viewportSize();
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.y + box.height).toBeLessThanOrEqual(viewport.height);
});

test("a long transcript scrolls inside the transcript, not the page", async ({ page }) => {
  // Given a transcript long enough to overflow.
  await page.setViewportSize(DESKTOP);
  for (let at = 0; at < 40; at += 1) {
    await sendAndWaitForEcho(page, `message ${at}`);
  }

  // Then the document does not scroll: its height is the viewport's.
  const height = await page.evaluate(
    () => document.documentElement.scrollHeight,
  );
  expect(height).toBe(page.viewportSize().height);
});

test("a long session title is cut with an ellipsis inside the rail", async ({
  page,
}) => {
  // Given a session whose title overflows the rail's column.
  await page.setViewportSize(DESKTOP);
  const id = await createSession(page);

  // When renaming the session just created to a long title. Its own row is
  // the one that changes — the list's first row is a different session.
  await page.locator(sel.TITLE).click();
  await page.locator('[data-testid="title-edit"]').fill(LONG_TITLE);
  await page.keyboard.press("Enter");

  // Then the row does not overflow the rail: the rail does not scroll sideways.
  const row = page.locator(`[data-testid="session-row"][data-session-id="${id}"]`);
  await expect(row).toContainText("a session title so long");
  const rail = await page.locator(sel.RAIL).evaluate((node) => ({
    client: node.clientWidth,
    scroll: node.scrollWidth,
  }));
  expect(rail.scroll).toBeLessThanOrEqual(rail.client);
});

test("the row's kebab sits entirely inside the rail", async ({ page }) => {
  // Given a session with a long title.
  await page.setViewportSize(DESKTOP);
  const id = await createSession(page);
  await page.locator(sel.TITLE).click();
  await page.locator('[data-testid="title-edit"]').fill(LONG_TITLE);
  await page.keyboard.press("Enter");

  // Then the kebab's box is inside the rail's box. The row is the renamed
  // session's own, so the long title is the one being measured.
  const wrap = page.locator(
    `[data-testid="session-row-wrap"][data-session-id="${id}"]`,
  );
  await expect(wrap).toContainText("a session title so long");
  const kebab = await wrap.locator('[data-testid="row-menu"]').boundingBox();
  const rail = await page.locator(sel.RAIL).boundingBox();
  expect(kebab.x).toBeGreaterThanOrEqual(rail.x);
  expect(kebab.x + kebab.width).toBeLessThanOrEqual(rail.x + rail.width);
});

test("the Archive label is centred in its button", async ({ page }) => {
  // Given the sidebar's Archive button.
  await page.setViewportSize(DESKTOP);
  const button = page.locator('[data-testid="open-picker"]');

  // Then the label's centre is the button's centre, left to right.
  const centres = await button.evaluate((node) => {
    const range = document.createRange();
    range.selectNodeContents(node);
    const text = range.getBoundingClientRect();
    const box = node.getBoundingClientRect();
    return { text: text.x + text.width / 2, box: box.x + box.width / 2 };
  });
  expect(Math.abs(centres.text - centres.box)).toBeLessThan(2);
});

/* ---- the pickers' rows ----
 * A picker row stacks its title above its meta line (a column). The title
 * class is shared with the sidebar's row (a single line), where a zero
 * flex-basis is the truncation the ellipsis needs. In a column that same
 * zero is a zero HEIGHT: the title collapsed and a row showed only its
 * project or provider. These pin the height in both places, so scoping the
 * truncation to the sidebar can never regress either. */

/** The height of a row's `.session-title`, in page pixels. */
async function titleHeight(page, rowSelector) {
  const row = page.locator(rowSelector).first();
  await expect(row).toBeVisible({ timeout: 10_000 });
  return row
    .locator(".session-title")
    .evaluate((node) => node.getBoundingClientRect().height);
}

test("the Archive picker shows each session's title at full height", async ({
  page,
}) => {
  // Given a session that has been persisted, so the picker has a row for it.
  await page.setViewportSize(DESKTOP);
  await sendAndWaitForEcho(page, "a title the picker must show");
  await page.locator(sel.OPEN_PICKER).click();

  // Then the row's title has real height — it is not collapsed onto the
  // project line beneath it.
  const height = await titleHeight(page, sel.PICKER_ROW);
  expect(height).toBeGreaterThan(14);
});

test("the model picker shows each model's name at full height", async ({
  page,
}) => {
  // Given the model picker, open.
  await page.setViewportSize(DESKTOP);
  await openProviderPicker(page);

  // Then the row's title has real height.
  const height = await titleHeight(page, sel.MODEL_ROW);
  expect(height).toBeGreaterThan(14);
});

test("the sidebar's row still gives its title a zero width basis", async ({
  page,
}) => {
  // Given a session in the sidebar.
  await page.setViewportSize(DESKTOP);

  // Then its title's flex-basis is the absolute zero a single-line row
  // needs to truncate — not the shared class's percentage.
  const basis = await page
    .locator(sel.SESSION_ROW)
    .first()
    .locator(".session-title")
    .evaluate((node) => getComputedStyle(node).flex);
  expect(basis).toBe("1 1 0px");
});
