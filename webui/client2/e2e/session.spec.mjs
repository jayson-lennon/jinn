//! Switching the active session from the rail.

import { createSession, expect, sel, test } from "./fixture.mjs";

test("a second session can be activated from the rail", async ({ browser, jinn, page }) => {
  const second = await createSession(jinn, browser);
  await page.reload();
  await expect(page.locator(sel.SESSION_ROW).filter({ hasText: "Untitled" })).toHaveCount(2, {
    timeout: 10_000,
  });

  // When clicking the row of the second session.
  await page.locator(sel.SESSION_ROW).nth(1).click();

  // Then that row is the active one.
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toBeVisible();
});
