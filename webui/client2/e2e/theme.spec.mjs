//! The color mode: the menu group, the attribute the stylesheet reads, and
//! that the choice outlives a reload.

import { expect, openMenu, pickTheme, sel, test } from "./fixture.mjs";

test("the menu lists Light, Dark, and System", async ({ page }) => {
  // Given a fresh page.

  // When opening the menu.
  await openMenu(page);

  // Then all three modes are offered.
  await expect(page.locator(sel.THEME_LIGHT)).toBeVisible();
  await expect(page.locator(sel.THEME_DARK)).toBeVisible();
  await expect(page.locator(sel.THEME_SYSTEM)).toBeVisible();
});

test("picking Light applies the light palette at once", async ({ page }) => {
  // Given a page in system mode.

  // When picking Light.
  await pickTheme(page, "light");

  // Then the document carries the light attribute.
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
});

test("picking Dark applies the dark palette", async ({ page }) => {
  // Given a page in system mode.

  // When picking Dark.
  await pickTheme(page, "dark");

  // Then the document carries the dark attribute.
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
});

test("the chosen mode marks its button active", async ({ page }) => {
  // Given a page in system mode.

  // When picking Dark.
  await pickTheme(page, "dark");

  // Then Dark is pressed and the others are not. Picking a mode keeps the
  // menu open, so the group is still on screen.
  await expect(page.locator(sel.THEME_DARK)).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(sel.THEME_LIGHT)).toHaveAttribute("aria-pressed", "false");
});

test("the chosen mode survives a reload", async ({ page }) => {
  // Given the page was put in dark mode.
  await pickTheme(page, "dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  // When the page reloads.
  await page.reload();

  // Then it comes back in dark mode.
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await openMenu(page);
  await expect(page.locator(sel.THEME_DARK)).toHaveAttribute("aria-pressed", "true");
});
