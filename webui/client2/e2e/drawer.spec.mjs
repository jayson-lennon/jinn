//! The phone drawer: the `#` button, the off-canvas rail, and how it closes.
//!
//! Every width decision is a CSS media query, so each test sets the
//! viewport itself. Nothing in the client reads the width.

import { createSession, expect, sel, test } from "./fixture.mjs";

/** Narrower than the 768px breakpoint. */
const PHONE = { width: 390, height: 800 };
/** At the breakpoint. */
const DESKTOP = { width: 1280, height: 800 };

test("the sessions button is visible at phone width", async ({ page }) => {
  await page.setViewportSize(PHONE);

  // Then the `#` is shown.
  await expect(page.locator(sel.SESSIONS_BUTTON)).toBeVisible();
});

test("the sessions button is hidden at desktop width", async ({ page }) => {
  await page.setViewportSize(DESKTOP);

  // Then the `#` is not rendered at all.
  await expect(page.locator(sel.SESSIONS_BUTTON)).toBeHidden();
});

test("the rail is out of view at phone width until the sessions button is pressed", async ({
  page,
}) => {
  // Given a phone-width page.
  await page.setViewportSize(PHONE);

  // Then the rail is not visible.
  await expect(page.locator(sel.RAIL)).toBeHidden();
});

test("pressing the sessions button opens the drawer", async ({ page }) => {
  // Given a phone-width page.
  await page.setViewportSize(PHONE);

  // When pressing `#`.
  await page.locator(sel.SESSIONS_BUTTON).click();

  // Then the rail is in view, with its scrim.
  await expect(page.locator(sel.RAIL)).toBeVisible();
  await expect(page.locator(sel.DRAWER_SCRIM)).toBeVisible();
});

test("pressing the scrim closes the drawer", async ({ page }) => {
  // Given an open drawer.
  await page.setViewportSize(PHONE);
  await page.locator(sel.SESSIONS_BUTTON).click();
  await expect(page.locator(sel.RAIL)).toBeVisible();

  // When pressing the scrim.
  await page.locator(sel.DRAWER_SCRIM).click({ position: { x: 370, y: 400 } });

  // Then the rail is out of view again.
  await expect(page.locator(sel.RAIL)).toBeHidden();
});

test("Escape closes the drawer", async ({ page }) => {
  // Given an open drawer.
  await page.setViewportSize(PHONE);
  await page.locator(sel.SESSIONS_BUTTON).click();
  await expect(page.locator(sel.RAIL)).toBeVisible();

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the rail is out of view again.
  await expect(page.locator(sel.RAIL)).toBeHidden();
});

test("picking a session row closes the drawer", async ({ page }) => {
  // Given an open drawer over two sessions.
  await createSession(page);
  await page.setViewportSize(PHONE);
  await page.locator(sel.SESSIONS_BUTTON).click();
  await expect(page.locator(sel.RAIL)).toBeVisible();

  // When picking the first session's row.
  await page.locator(sel.SESSION_ROW).first().click();

  // Then the drawer is closed.
  await expect(page.locator(sel.RAIL)).toBeHidden();
});
