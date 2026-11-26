//! The hamburger menu's shell: where the button sits, how the panel
//! opens, and the three ways it closes. The items themselves belong to
//! the features that supply them; here the panel's content is not under
//! test, only the chrome.

import { expect, openMenu, sel, test } from "./fixture.mjs";

test("the hamburger sits in the title bar, left of the title", async ({ page }) => {
  // Given a connected page.
  // Then the menu button is the title bar's first control, and the
  // title sits to its right.
  const bar = page.locator(".title-bar");
  const button = page.locator(sel.MENU_BUTTON);
  await expect(button).toBeVisible();
  await expect(bar.locator("> *").first()).toHaveAttribute(
    "class",
    new RegExp("menu"),
  );
  const [buttonBox, titleBox] = await Promise.all([
    button.boundingBox(),
    page.locator(sel.TITLE).boundingBox(),
  ]);
  expect(buttonBox.x).toBeLessThan(titleBox.x);
});

test("the menu is closed on a fresh page", async ({ page }) => {
  // Given a page that has just connected.
  // Then the panel is not visible and the button says it is collapsed.
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
  await expect(page.locator(sel.MENU_BUTTON)).toHaveAttribute(
    "aria-expanded",
    "false",
  );
});

test("clicking the hamburger opens the panel", async ({ page }) => {
  // Given a closed menu.
  // When clicking the hamburger.
  await page.locator(sel.MENU_BUTTON).click();

  // Then the panel is visible, the scrim is behind it, and the button
  // says the menu is open.
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();
  await expect(page.locator(sel.MENU_SCRIM)).toBeVisible();
  await expect(page.locator(sel.MENU_BUTTON)).toHaveAttribute(
    "aria-expanded",
    "true",
  );
});

test("clicking the scrim closes the menu", async ({ page }) => {
  // Given an open menu.
  await openMenu(page);

  // When clicking the scrim, well away from the panel.
  await page.locator(sel.MENU_SCRIM).click({ position: { x: 900, y: 400 } });

  // Then the panel is gone.
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
});

test("Escape closes the menu", async ({ page }) => {
  // Given an open menu.
  await openMenu(page);

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the panel is gone.
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
});
