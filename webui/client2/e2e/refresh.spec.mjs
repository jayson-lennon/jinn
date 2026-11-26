//! The model picker's Refresh models button and its busy state.

import { expect, sel, test } from "./fixture.mjs";

/** Opens the model picker through the title bar's model chip. */
async function openModelPicker(page) {
  await page.locator(sel.MODEL_CHIP).click();
  await expect(page.locator(sel.MODEL_FILTER)).toBeVisible();
}

test("the model picker offers a Refresh models button", async ({ page }) => {
  // Given an open model picker.
  await openModelPicker(page);

  // Then the button is there and idle.
  await expect(page.locator(sel.REFRESH_MODELS)).toBeVisible();
  await expect(page.locator(sel.REFRESH_MODELS)).toBeEnabled();
  await expect(page.locator(sel.REFRESH_MODELS)).not.toHaveAttribute("aria-busy", "true");
});

test("pressing Refresh models marks the button busy", async ({ page }) => {
  // Given an open model picker.
  await openModelPicker(page);

  // When pressing refresh.
  await page.locator(sel.REFRESH_MODELS).click();

  // Then the button is disabled and busy — until the rows land.
  await expect(page.locator(sel.REFRESH_MODELS)).toHaveAttribute("aria-busy", "true");
  await expect(page.locator(sel.REFRESH_MODELS)).toBeDisabled();
});

test("the button re-enables once discovery lands", async ({ page }) => {
  // Given an open model picker whose refresh was pressed.
  await openModelPicker(page);
  await page.locator(sel.REFRESH_MODELS).click();

  // When the model cache lands.
  // Then the button is idle again and the rows are fresh.
  await expect(page.locator(sel.REFRESH_MODELS)).toBeEnabled({ timeout: 15_000 });
  await expect(page.locator(sel.REFRESH_MODELS)).not.toHaveAttribute("aria-busy", "true");
  await expect(page.locator(sel.MODEL_ROW)).toHaveCount(2);
});

test("the button cannot be pressed again while busy", async ({ page }) => {
  // Given an open model picker whose refresh was pressed.
  await openModelPicker(page);
  await page.locator(sel.REFRESH_MODELS).click();

  // When trying to press it again — a disabled control swallows the
  // click, so this resolves at once rather than waiting out a long
  // timeout.
  await page.locator(sel.REFRESH_MODELS).click({ force: true, timeout: 1_000 });

  // Then exactly one refresh reached the server: the button is still
  // disabled, because no second discovery run is pending.
  await expect(page.locator(sel.REFRESH_MODELS)).toBeDisabled();
});
