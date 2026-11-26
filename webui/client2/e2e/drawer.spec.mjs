//! The phone drawer: the `#` button, the off-canvas rail, and how it closes.
//!
//! Every width decision is a CSS media query, so each test sets the
//! viewport itself. Nothing in the client reads the width.

import { createSession, expect, sel, test } from "./fixture.mjs";

/** The rail's distance from the viewport's left edge, in pixels. */
async function railInset(page) {
  return page.locator(sel.RAIL).evaluate((node) => node.getBoundingClientRect().left);
}

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

/* ---- where the `#` sits, and the menu's Sessions item ----
 * The `#` used to be a child of the page's grid, not of the title bar: at
 * phone width the grid is one column, so it took a whole row across the
 * top. These pin it to the title bar, left of the hamburger, sized like
 * the hamburger — and pin the menu item that opens the same drawer. */

test("the sessions button is as tall as the hamburger beside it", async ({
  page,
}) => {
  // Given a phone-width page.
  await page.setViewportSize(PHONE);

  // Then the `#` and the `☰` have the same height — one row, one size.
  const hash = await page.locator(sel.SESSIONS_BUTTON).boundingBox();
  const menu = await page.locator(sel.MENU_BUTTON).boundingBox();
  expect(hash.height).toBe(menu.height);
});

test("the sessions button is no wider than the title bar's tenth", async ({
  page,
}) => {
  // Given a phone-width page.
  await page.setViewportSize(PHONE);

  // Then the `#` is a small control — it does not stretch across the bar.
  const box = await page.locator(sel.SESSIONS_BUTTON).boundingBox();
  expect(box.width).toBeLessThan(PHONE.width / 10);
});

test("the sessions button sits left of the hamburger button", async ({ page }) => {
  // Given a phone-width page.
  await page.setViewportSize(PHONE);

  // Then the `#`'s right edge is at or left of the hamburger's left edge,
  // and they share the title bar's row.
  const hash = await page.locator(sel.SESSIONS_BUTTON).boundingBox();
  const menu = await page.locator(sel.MENU_BUTTON).boundingBox();
  expect(hash.x + hash.width).toBeLessThanOrEqual(menu.x);
  expect(Math.abs(hash.y + hash.height / 2 - (menu.y + menu.height / 2))).toBeLessThan(6);
});

test("the sessions button reports whether the drawer is open", async ({
  page,
}) => {
  // Given a phone-width page with the drawer closed.
  await page.setViewportSize(PHONE);
  const button = page.locator(sel.SESSIONS_BUTTON);
  await expect(button).toHaveAttribute('aria-expanded', 'false');

  // When pressing it.
  await button.click();

  // Then it reports the drawer open.
  await expect(button).toHaveAttribute('aria-expanded', 'true');
});

test("the menu's Sessions item sits just above the color-mode group", async ({
  page,
}) => {
  // Given the menu, open, at desktop width.
  await page.setViewportSize(DESKTOP);
  await page.locator(sel.MENU_BUTTON).click();

  // Then the group is the panel's last row, and Sessions is the row
  // directly above it — nothing between them.
  const children = page.locator(sel.MENU_PANEL).locator(':scope > *');
  await expect(children.last()).toHaveAttribute('data-testid', 'theme-group');
  await expect(children.nth(-2)).toHaveAttribute(
    'data-testid',
    'menu-item-sessions',
  );
  const sessions = await children.nth(-2).boundingBox();
  const group = await children.last().boundingBox();
  expect(group.y).toBeGreaterThanOrEqual(sessions.y + sessions.height);
});

test("the menu's Sessions item opens the drawer at phone width", async ({
  page,
}) => {
  // Given a phone-width page with the menu open.
  await page.setViewportSize(PHONE);
  await page.locator(sel.MENU_BUTTON).click();

  // When picking Sessions.
  await page.locator(sel.MENU_ITEM_SESSIONS).click();

  // Then the menu closed and the drawer slid fully in — its left edge is
  // at the viewport's, not merely entering.
  await expect(page.locator(sel.MENU_BUTTON)).toHaveAttribute('aria-expanded', 'false');
  await expect.poll(() => railInset(page)).toBe(0);
});

test("the menu's Sessions item does nothing at desktop width", async ({
  page,
}) => {
  // Given a desktop-width page, where the rail is pinned in place.
  await page.setViewportSize(DESKTOP);
  const before = await railInset(page);

  // When picking Sessions.
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_SESSIONS).click();

  // Then the rail has not moved and no drawer scrim is shown.
  expect(await railInset(page)).toBe(before);
  await expect(page.locator(sel.DRAWER_SCRIM)).toBeHidden();
});
