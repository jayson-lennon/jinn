//! Switching the model from the chip in the title bar.

import { expect, MODEL_LABEL, OTHER_MODEL_LABEL, OTHER_MODEL_NAME, sel, test } from "./fixture.mjs";

test("the model chip switches to another selectable row", async ({ page }) => {
  // Given the sample model selected (the page fixture's starting state).
  await expect(page.locator(sel.MODEL_CHIP)).toHaveText(MODEL_LABEL);

  // When picking the other provider.
  await page.locator(sel.MODEL_CHIP).click();
  const other = page.locator(sel.MODEL_ROW).filter({ hasText: OTHER_MODEL_NAME });
  await expect(other).toBeVisible();
  await other.click();

  // Then the chip names the new model.
  await expect(page.locator(sel.MODEL_CHIP)).toHaveText(OTHER_MODEL_LABEL);
});

test("the model filter narrows the picker to matching rows", async ({ page }) => {
  // Given the open picker lists both staged models.
  await page.locator(sel.MODEL_CHIP).click();
  await expect(page.locator(sel.MODEL_ROW)).toHaveCount(2);

  // When filtering for the second provider.
  await page.locator(sel.MODEL_FILTER).fill(OTHER_MODEL_NAME);

  // Then only its model is listed.
  const rows = page.locator(sel.MODEL_ROW);
  await expect(rows).toHaveCount(1);
  await expect(rows).toContainText(OTHER_MODEL_NAME);
});

test("the model filter matches a provider's name", async ({ page }) => {
  // Given the open picker lists both staged models.
  await page.locator(sel.MODEL_CHIP).click();
  await expect(page.locator(sel.MODEL_ROW)).toHaveCount(2);

  // When filtering by the second provider's name alone, which its model
  // does not contain.
  await page.locator(sel.MODEL_FILTER).fill("second");

  // Then that provider's model is the one listed.
  const rows = page.locator(sel.MODEL_ROW);
  await expect(rows).toHaveCount(1);
  await expect(rows).toContainText(OTHER_MODEL_NAME);
});

test("the model filter reads a space as two terms", async ({ page }) => {
  // Given the open picker lists both staged models.
  await page.locator(sel.MODEL_CHIP).click();
  await expect(page.locator(sel.MODEL_ROW)).toHaveCount(2);

  // When filtering for the second provider's name and its model's first
  // word, separated by a space — the way the terminal picker reads terms.
  await page.locator(sel.MODEL_FILTER).fill("second model");

  // Then only that row is listed.
  const rows = page.locator(sel.MODEL_ROW);
  await expect(rows).toHaveCount(1);
  await expect(rows).toContainText(OTHER_MODEL_NAME);
});

test("the model filter highlights the matched letters", async ({ page }) => {
  // Given the open picker lists both staged models.
  await page.locator(sel.MODEL_CHIP).click();
  await expect(page.locator(sel.MODEL_ROW)).toHaveCount(2);

  // When filtering by the second provider's name.
  await page.locator(sel.MODEL_FILTER).fill("second");

  // Then the provider line marks exactly that word.
  const row = page.locator(sel.MODEL_ROW);
  await expect(row).toHaveCount(1);
  await expect(row.locator("mark")).toHaveText("second");
});
