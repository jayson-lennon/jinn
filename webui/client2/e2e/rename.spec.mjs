//! Renaming the active session in place from the title bar.

import { expect, sel, test } from "./fixture.mjs";

test("the title can be renamed in place", async ({ page }) => {
  // When clicking the title, typing a new one, and pressing Enter.
  await page.locator(sel.TITLE).click();
  await page.locator(sel.TITLE_EDIT).fill("renamed from web2");
  await page.locator(sel.TITLE_EDIT).press("Enter");

  // Then the new title is shown.
  await expect(page.locator(sel.TITLE)).toHaveText("renamed from web2");
});

test("a rename updates the session's row in the sidebar", async ({ page }) => {
  // Given a session listed in the sidebar.
  const row = page.locator(sel.ACTIVE_SESSION_ROW);
  await expect(row).toBeVisible();

  // When it is renamed from the title bar.
  await page.locator(sel.TITLE).click();
  await page.locator(sel.TITLE_EDIT).fill("renamed from web2");
  await page.locator(sel.TITLE_EDIT).press("Enter");

  // Then the sidebar row shows the new title, not the old one.
  await expect(row).toHaveText("renamed from web2");
});
