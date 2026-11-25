//! The busy state: queueing a message for after the running turn, and
//! cancelling the turn.

import { expect, sel, sendAndWaitForEcho, startBusyTurn, test } from "./fixture.mjs";

test("a busy session shows Queue and Cancel instead of Send", async ({ page }) => {
  await startBusyTurn(page);

  // Then the busy controls are on screen and Send is gone.
  await expect(page.locator(sel.QUEUE)).toBeVisible();
  await expect(page.locator(sel.SEND)).toHaveCount(0);
});

test("a queued message is delivered after the running turn", async ({ page }) => {
  await startBusyTurn(page);

  // When queueing a message mid-turn (the queued turn also streams, so
  // the settle below waits on a real second reply).
  await page.locator(sel.DRAFT).fill("!response");
  await page.locator(sel.QUEUE).click();

  // Then the queued turn runs to completion and the composer is idle.
  await expect(page.locator(sel.SEND)).toBeVisible({ timeout: 30_000 });
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`)).toHaveCount(2);
});

test("cancel stops the running turn", async ({ page }) => {
  await startBusyTurn(page);

  // When cancelling.
  await page.locator(sel.CANCEL).click();

  // Then the composer leaves its busy state.
  await expect(page.locator(sel.SEND)).toBeVisible({ timeout: 15_000 });
});

test("a settled turn leaves the composer idle", async ({ page }) => {
  await sendAndWaitForEcho(page, "settles on its own");

  // Then Send is back.
  await expect(page.locator(sel.SEND)).toBeVisible();
});
