//! The session dash: the card the status bar's context figure opens.
//!
//! It lists the session's statistics, the enabled MCP servers, the task
//! list, and the pinned entries — every row the server's own string. Escape
//! or a tap on the scrim closes it; a tap on the MCP section hands over to
//! the picker; a tap on a pin closes it and the transcript jumps. The pin
//! jump's far-history case is covered in `pin-jump.spec.mjs`.
//!
//! A scratch boot's session starts on a model whose window the catalog may
//! or may not record, so a figure is asserted by its shape, never by a
//! number. The figures land on the tick after the dash opens — the open
//! clears the server's baseline, so the first tick pushes them.

import { createSession, entryMenu, expect, openDash, sel, sendAndWaitForEcho, test } from "./fixture.mjs";

/** The value cell of the one statistics row whose label is `label`. */
function statRow(page, label) {
  return page
    .locator(`${sel.DASH_STATS} .dash-row`)
    .filter({ has: page.locator(".dash-row-label", { hasText: new RegExp(`^${label}$`) }) })
    .locator(".dash-row-value");
}

/** The context figure's shape: a percentage over a budget, or used tokens
 *  over an unknown window. */
const FIGURE = /%\/|\/\?\?\?/;

test("clicking the context figure opens the dash", async ({ page }) => {
  // Given the boot session's bar.
  await expect(page.locator(sel.STATUS_BAR)).toBeVisible();

  // When clicking the context figure.
  await page.locator(sel.STATUS_CONTEXT).click();

  // Then the dash opens: the scrim behind it, the card with its sections.
  await expect(page.locator(sel.DASH_SCRIM)).toBeVisible();
  await expect(page.locator(sel.DASH)).toBeVisible();
  await expect(page.locator(sel.DASH_STATS)).toBeVisible();
  await expect(page.locator(sel.DASH_MCP)).toBeVisible();
  await expect(page.locator(sel.DASH_TASKS)).toBeVisible();
  await expect(page.locator(sel.DASH_PINS)).toBeVisible();
});

test("the dash's context figure is the status bar's", async ({ page }) => {
  // Given a session whose figure the bar already shows.
  await sendAndWaitForEcho(page, "hello");
  await expect(page.locator(sel.STATUS_CONTEXT)).toHaveText(FIGURE);

  // When the dash opens and its figures land.
  await page.locator(sel.STATUS_CONTEXT).click();
  const row = statRow(page, "Context");

  // Then the dash's Context row carries the same figure as the bar.
  await expect(row).toHaveText(FIGURE);
  await expect(row).toHaveText((await page.locator(sel.STATUS_CONTEXT).innerText()).trim());
});

test("the dash lists the session's statistics one per line", async ({ page }) => {
  // Given the dash, open, with its figures landed.
  await page.locator(sel.STATUS_CONTEXT).click();
  await expect(statRow(page, "Model")).not.toHaveText("…", { timeout: 15_000 });

  // Then every figure the terminal status bar shows has its own row.
  for (const label of ["Model", "Context", "Tokens", "Cost", "Turns", "Working time", "Directory"]) {
    await expect(statRow(page, label)).not.toHaveText("…");
  }
  // And the tokens row carries both directions, as the terminal bar does.
  await expect(statRow(page, "Tokens")).toHaveText(/↑.+ ↓.+/);
  // And the working time is a clock.
  await expect(statRow(page, "Working time")).toHaveText(/^\d\d:\d\d:\d\d$/);
});

test("Escape closes the dash", async ({ page }) => {
  // Given an open dash.
  await page.locator(sel.STATUS_CONTEXT).click();
  await expect(page.locator(sel.DASH)).toBeVisible();

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the dash and its scrim are gone.
  await expect(page.locator(sel.DASH)).toBeHidden();
  await expect(page.locator(sel.DASH_SCRIM)).toBeHidden();
});

test("tapping outside the dash closes it", async ({ page }) => {
  // Given an open dash.
  await page.locator(sel.STATUS_CONTEXT).click();
  await expect(page.locator(sel.DASH)).toBeVisible();

  // When tapping the scrim, outside the card.
  await page.locator(sel.DASH_SCRIM).click({ position: { x: 20, y: 400 } });

  // Then the dash and its scrim are gone.
  await expect(page.locator(sel.DASH)).toBeHidden();
  await expect(page.locator(sel.DASH_SCRIM)).toBeHidden();
});

test.with({ mcp: true })("tapping the MCP section opens the picker and closes the dash", async ({
  page,
}) => {
  // Given the dash, open, with a server enabled.
  await page.locator(sel.STATUS_CONTEXT).click();
  await expect(page.locator(sel.DASH_MCP)).toBeVisible();

  // When tapping anywhere in the MCP section.
  await page.locator(sel.DASH_MCP).click();

  // Then the MCP picker opened, and the dash closed first so the picker
  // paints over nothing.
  await expect(page.locator(sel.MCP_MODAL)).toBeVisible();
  await expect(page.locator(sel.DASH)).toBeHidden();
});

test("an empty session's dash says it has no tasks and no pins", async ({ page }) => {
  // Given the boot session's dash, untouched.
  await page.locator(sel.STATUS_CONTEXT).click();

  // Then the tasks and pins sections say so, and are never hidden.
  await expect(page.locator(sel.DASH_TASKS)).toContainText("No tasks yet");
  await expect(page.locator(sel.DASH_PINS)).toContainText("No pinned entries");
});

test("a session switch closes an open dash", async ({ page, browser }) => {
  // Given a second session to switch to, and the dash open on the boot
  // session. The session is made first: the dash's scrim covers the menu.
  const second = await createSession(page);
  await page.locator(`${sel.ROW_WRAP(second)} ${sel.SESSION_ROW}`).click();
  await openDash(page);

  // When another page switches the instance to the second session — the
  // dash's scrim covers this page's own rail, so the switch arrives as a
  // snapshot this page follows.
  const other = await browser.newPage();
  try {
    await other.goto(page.url());
    await other.locator(`${sel.ROW_WRAP(second)} ${sel.SESSION_ROW}`).click();
  } finally {
    await other.close();
  }

  // Then the dash is closed: its facts described a session the page left.
  await expect(page.locator(sel.DASH)).toBeHidden({ timeout: 10_000 });
  await expect(page.locator(sel.DASH_SCRIM)).toBeHidden();
});

test("each pin row carries a trailing arrow its label never crowds", async ({ page }) => {
  // Given an entry pinned whose label is far longer than the dash's row.
  const long = `a pinned entry whose label runs on for so long that it could never fit inside the dash's row without being cut off somewhere past the arrow`;
  await page.locator(sel.DRAFT).fill(long);
  await page.locator(sel.SEND).click();
  const user = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`).last();
  await expect(user).toBeVisible();
  await entryMenu(page, "user").click();
  await page.locator(sel.ENTRY_MENU_PIN).click();
  await openDash(page);

  // Then the row shows the arrow, in the accent, after the label — and the
  // label stays inside its own box rather than running under the arrow.
  // (The server truncates a pin's label before the page sees it, so an
  // entry long enough to force the ellipsis cannot be arranged here; the
  // geometric check below is what the row's flex layout guarantees.)
  const pin = page.locator(sel.DASH_PIN).first();
  await expect(pin.locator(sel.DASH_PIN_ARROW)).toHaveText("❯");
  const arrowBox = await pin.locator(sel.DASH_PIN_ARROW).boundingBox();
  const labelBox = await pin.locator(sel.DASH_PIN_LABEL).boundingBox();
  expect(labelBox.x + labelBox.width).toBeLessThanOrEqual(arrowBox.x);
});

test("a pin row is touch-sized", async ({ page }) => {
  // Given a pinned entry and the dash open over it.
  await sendAndWaitForEcho(page, "hello");
  await entryMenu(page, "assistant").click();
  await page.locator(sel.ENTRY_MENU_PIN).click();
  await openDash(page);

  // Then the pin's box is at least 36px tall.
  const box = await page.locator(sel.DASH_PIN).first().boundingBox();
  expect(box.height).toBeGreaterThanOrEqual(36);
});
