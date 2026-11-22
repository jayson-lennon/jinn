//! Smoke spec — the page loads against the real binary.
//!
//! Proves the whole tier's foundation in one spec: the spawned jinn
//! binds, the page renders its shell, the boot session's row is in
//! the sidebar, clicking it lands on the session page, and the
//! model-picker button is in the titlebar.

import { expect, test } from "@playwright/test";
import { openSession } from "./nav.mjs";
import * as sel from "./selectors.mjs";
const { MODEL_ID } = sel;

test("page root renders the boot session row and opens it", async ({ page }) => {
  // Given a freshly booted scratch jinn.
  // When navigating to the page root and opening the boot session.
  const sessionId = await openSession(page);

  // Then the page root names the session.
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionId,
  );
  // And the session page path carries the id.
  expect(page.url()).toContain(`${sel.SESSION_PAGE_PATH}${sessionId}`);
});

test("session page shows the model picker button in the titlebar", async ({
  page,
}) => {
  // Given the boot session open.
  await openSession(page);

  // When reading the titlebar.
  // Then the model-picker button is visible and carries the boot
  // model's label — the button is the model display (no second
  // label rides beside it).
  await expect(page.locator(sel.PICKER_BUTTON)).toBeVisible();
  await expect(page.locator(sel.PICKER_BUTTON)).toHaveText(MODEL_ID);
});
