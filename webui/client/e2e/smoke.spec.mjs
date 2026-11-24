//! Smoke spec — the page loads against the real binary.
//!
//! Proves the whole tier's foundation in one spec: each test's own
//! jinn binds, the page renders its shell, the boot session's row is in
//! the sidebar, clicking it lands on the session page, and the
//! model-picker button is in the titlebar.

import { expect, test } from "./fixture.mjs";
import { createSession, openSession } from "./fixture.mjs";
import * as sel from "./selectors.mjs";
const { MODEL_ID } = sel;

test("page root renders the boot session row and opens it", async ({ page, jinn }) => {
  // Given a freshly booted scratch jinn.
  // When navigating to the page root and opening the boot session.
  const sessionId = await openSession(jinn, page);

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
  jinn,
}) => {
  // Given the boot session open.
  await openSession(jinn, page);

  // When reading the titlebar.
  // Then the model-picker button is visible and carries the boot
  // model's label — the button is the model display (no second
  // label rides beside it).
  await expect(page.locator(sel.PICKER_BUTTON)).toBeVisible();
  await expect(page.locator(sel.PICKER_BUTTON)).toHaveText(MODEL_ID);
});

test("navigating sessions never blanks the sidebar", async ({ page, jinn }) => {
  // Given a landing page whose sidebar carries two rows — a fresh
  // scratch db boots with one, so a second is created through the
  // modal.
  await openSession(jinn, page);
  await createSession(jinn, page);
  await page.goto(`${jinn.baseUrl}/web`);
  const rowCount = await page.locator(sel.SESSION_ROW).count();
  expect(rowCount).toBe(2);
  const firstId = await page
    .locator(sel.SESSION_ROW)
    .first()
    .getAttribute(sel.SESSION_ID_ATTR);

  // When clicking into the first row's session (a real navigation).
  await page.locator(sel.SESSION_ROW_BY_ID(firstId)).click();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    firstId,
  );

  // Then the sidebar never lost a row: the navigation's targeted
  // patches leave the list's DOM untouched (the blank-flash bug this
  // pins), and the row count still matches the boot render.
  await expect(page.locator(sel.SESSION_ROW)).toHaveCount(rowCount);
});
