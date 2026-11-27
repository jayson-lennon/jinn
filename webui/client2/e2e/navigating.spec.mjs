//! The navigating overlay: what the page shows while a pin's jump streams
//! history in, and the way out of it.
//!
//! A jump shows the overlay only after a beat, so one that lands at once
//! never flashes it — and that is the common case on a quick link, which is
//! what the harness is. So the beat is what this spec can pin down: a pin
//! in the newest page never shows the overlay at all. The overlay's own
//! behaviour — that it blocks the page, that Cancel closes it and stops the
//! stream — is the store's and the server's, and is proved there; here the
//! contract is that a quick jump leaves the page clean.

import {
  awaitConnected,
  entryMenu,
  expect,
  openDash,
  sel,
  sendAndWaitForEcho,
  test,
} from "./fixture.mjs";

/** Longer than the overlay's beat by a comfortable margin: if the overlay
 *  were going to show for a quick jump, it would have by now. */
const PAST_THE_BEAT_MS = 600;

test("a pin in the newest page never flashes the navigating overlay", async ({ page }) => {
  // Given a session with one user message, that message pinned — a jump
  // with no older page to load.
  await sendAndWaitForEcho(page, "a quick jump");
  await entryMenu(page, "user").click();
  await page.locator(sel.ENTRY_MENU_PIN).click();
  const entry = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`).last();
  await expect(entry).toHaveClass(/pinned/, { timeout: 5_000 });
  const pinned = await entry.getAttribute("data-entry-id");

  // When tapping the pin in the dash.
  await openDash(page);
  await page.locator(sel.DASH_PIN_NAMED(pinned)).click();

  // Then the dash closed and the entry is in view, and the overlay was never
  // shown: the jump landed inside the beat.
  await expect(page.locator(sel.DASH)).toBeHidden();
  await expect(page.locator(`${sel.TRANSCRIPT} [data-entry-id="${pinned}"]`)).toBeInViewport();
  await page.waitForTimeout(PAST_THE_BEAT_MS);
  await expect(page.locator(sel.NAVIGATING_OVERLAY)).toHaveCount(0);
});
