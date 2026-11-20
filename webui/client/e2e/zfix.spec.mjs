import { expect, test } from "@playwright/test";
import { openSession } from "./nav.mjs";
import * as sel from "./selectors.mjs";

test("persona modal closes on backdrop click", async ({ page }) => {
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_PERSONA).click();
  const modal = page.locator(sel.PERSONA_MODAL);
  await expect(modal).toBeVisible();
  const box = await modal.boundingBox();
  await page.mouse.click(box.x + 10, box.y + box.height - 10);
  await expect(modal).toBeHidden();
});

test("new-session modal closes on backdrop click", async ({ page }) => {
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  const modal = page.locator(sel.SESSION_NEW_MODAL);
  await expect(modal).toBeVisible();
  const box = await modal.boundingBox();
  await page.mouse.click(box.x + 10, box.y + box.height - 10);
  await expect(modal).toBeHidden();
});

test("model modal closes on backdrop click", async ({ page }) => {
  await openSession(page);
  await page.locator(sel.PICKER_BUTTON).click();
  const modal = page.locator(sel.PICKER_MODAL);
  await expect(modal).toBeVisible();
  const box = await modal.boundingBox();
  await page.mouse.click(box.x + 10, box.y + box.height - 10);
  await expect(modal).toBeHidden();
});
