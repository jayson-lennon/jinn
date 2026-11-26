//! The model picker's Refresh models button and its busy state.

import { expect, sel, test } from "./fixture.mjs";

/** Opens the model picker through the title bar's model chip. */
async function openModelPicker(page) {
  await page.locator(sel.MODEL_CHIP).click();
  await expect(page.locator(sel.MODEL_FILTER)).toBeVisible();
}

/** The refresh summaries the transcript holds. */
const summaries = (page) =>
  page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("transient")}`);

/**
 * Waits for the page to be settled after a refresh press: the button idle
 * and that press's summary landed. The sample provider answers discovery
 * almost at once, so a busy state can be gone before an assertion looks —
 * and a summary can still be arriving after the button re-enables. The
 * button going idle is not proof the summary landed, so the count is
 * waited on against the count before the press.
 *
 * @param {import('@playwright/test').Page} page the test's own page.
 * @param {number} before the summary count read before the press.
 */
async function settled(page, before) {
  await expect(page.locator(sel.REFRESH_MODELS)).toBeEnabled({ timeout: 15_000 });
  await expect
    .poll(async () => summaries(page).count(), { timeout: 15_000 })
    .toBe(before + 1);
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

  // When pressing refresh, and reading the button as soon as the framework
  // has flushed the click to the DOM — one microtask later, but long before
  // discovery can answer and re-enable it.
  const busy = await page.locator(sel.REFRESH_MODELS).evaluate(async (button) => {
    button.click();
    await Promise.resolve();
    return {
      busy: button.getAttribute("aria-busy"),
      disabled: button.disabled,
    };
  });

  // Then it was marked busy and disabled by the press itself.
  expect(busy).toEqual({ busy: "true", disabled: true });
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
  // Given a settled page, with its summaries counted.
  await openModelPicker(page);
  const settled_before = await summaries(page).count();
  await page.locator(sel.REFRESH_MODELS).click();
  await settled(page, settled_before);
  const before = await summaries(page).count();

  // When pressing refresh and, without waiting, forcing a second press onto
  // the now-busy button.
  await page.locator(sel.REFRESH_MODELS).evaluate((button) => {
    button.click();
    button.click();
  });
  await settled(page, before);

  // Then one summary was added for the two presses: the busy button took
  // the first and refused the second.
  await expect(summaries(page)).toHaveCount(before + 1);
});

test("the refresh's summary table appears in the transcript", async ({ page }) => {
  // Given an open model picker.
  await openModelPicker(page);

  // When pressing refresh and waiting for discovery to land.
  await page.locator(sel.REFRESH_MODELS).click();
  await expect(page.locator(sel.REFRESH_MODELS)).toBeEnabled({ timeout: 15_000 });

  // Then closing the picker, the transcript holds the summary the terminal
  // shows: one row per provider, with its model count. Closed by its
  // backdrop: the modal's Escape is on its filter, and focus is on the
  // button just pressed.
  await page.locator(".modal-backdrop").click({ position: { x: 2, y: 2 } });
  await expect(page.locator(sel.MODEL_FILTER)).toHaveCount(0);
  // The last one: every refresh — boot-time discovery included — writes its
  // own summary, and this test's is the newest.
  const summaries = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("transient")}`);
  await expect(summaries.last()).toContainText("sample");
  // And it renders as a real table: the summary is markdown, so its pipes
  // are table cells and not literal `|` characters.
  await expect(summaries.last().locator("table")).toHaveCount(1);
  await expect(summaries.last().locator("table")).toContainText("sample");
});
