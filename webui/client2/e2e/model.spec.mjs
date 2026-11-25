//! Switching the model from the chip in the title bar.

import { expect, MODEL_LABEL, sel, test } from "./fixture.mjs";

test("the model chip switches to another selectable row", async ({ page }) => {
  // Given the sample model selected (the page fixture's starting state).
  await expect(page.locator(sel.MODEL_CHIP)).toHaveText(MODEL_LABEL);

  // When picking the other provider.
  await page.locator(sel.MODEL_CHIP).click();
  const other = page.locator(sel.MODEL_ROW).filter({ hasText: "second-model" });
  await expect(other).toBeVisible();
  await other.click();

  // Then the chip names the new model.
  await expect(page.locator(sel.MODEL_CHIP)).toHaveText("second-model");
});
