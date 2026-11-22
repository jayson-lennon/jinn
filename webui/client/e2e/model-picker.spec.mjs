//! Model-picker flow — open, filter, switch, inert unavailable rows,
//! and the refresh spinner cycle.
//!
//! With a sample-only scratch providers.toml the picker shows one
//! model, `sample`, so the "current promoted" and "switch" specs
//! pivot on it: switching to the only model is a no-op switch, which
//! is exactly the right exercise — the request round-trips the server,
//! and the client must re-render the button even when the model does
//! not change.

import { expect, test } from "@playwright/test";
import { openSession } from "./nav.mjs";
import * as sel from "./selectors.mjs";
const { MODEL_ID } = sel;

test("opening the picker shows the current model promoted as active", async ({
  page,
}) => {
  // Given the boot session open.
  await openSession(page);

  // When opening the model picker.
  await page.locator(sel.PICKER_BUTTON).click();

  // Then the modal renders with the sample model's row, promoted as
  // the active one.
  const modal = page.locator(sel.PICKER_MODAL);
  await expect(modal).toBeVisible();
  const row = page.locator(sel.PICKER_ROW(MODEL_ID));
  await expect(row).toBeVisible();
  await expect(row).toHaveClass(/model-row-active/);
});

test("fuzzy filter highlights matches with a mark", async ({ page }) => {
  // Given the picker open.
  await openSession(page);
  await page.locator(sel.PICKER_BUTTON).click();

  // When typing a fuzzy-only query (`spl` matches `sample`).
  const filter = page.locator(sel.PICKER_FILTER);
  await filter.fill("spl");

  // Then the row survives and its model name carries `<mark>` runs
  // over the matched characters (fuzzy subsequence matches split the
  // highlight into several runs — `spl` on `sample` gives two).
  const modelSpan = page.locator(sel.PICKER_ROW_MODEL_SPAN(MODEL_ID));
  await expect(page.locator(sel.PICKER_ROW(MODEL_ID))).toBeVisible();
  const marks = modelSpan.locator("mark");
  const markCount = await marks.count();
  if (markCount === 0) {
    throw new Error("filter matched but no <mark> highlight rendered");
  }
  await expect(modelSpan).toContainText("sample");
});

test("switching from the inner model-name span re-renders the button", async ({
  page,
}) => {
  // Given the picker open on the boot session.
  await openSession(page);
  await page.locator(sel.PICKER_BUTTON).click();
  const modal = page.locator(sel.PICKER_MODAL);
  await expect(modal).toBeVisible();

  // When clicking the row's inner `.model-row-model` span — the
  // delegation target, not the row itself.
  await page.locator(sel.PICKER_ROW_MODEL_SPAN(MODEL_ID)).click();

  // Then the modal closes.
  await expect(modal).not.toBeVisible();
  // And the button carries the switched model immediately (the
  // sync-write fix's pin — the button is the model display now).
  await expect(page.locator(sel.PICKER_BUTTON)).toHaveText(MODEL_ID);
});

test("clicking the backdrop closes the picker", async ({ page }) => {
  // Given the model picker open.
  await openSession(page);
  await page.locator(sel.PICKER_BUTTON).click();
  const modal = page.locator(sel.PICKER_MODAL);
  await expect(modal).toBeVisible();

  // When clicking the dark backdrop outside the card.
  const box = await modal.boundingBox();
  await page.mouse.click(box.x + 10, box.y + box.height - 10);

  // Then the picker hides — the click-away handler lives on the
  // overlay, not the card.
  await expect(modal).toBeHidden();
});

test("unavailable rows are inert: no switch action, modal stays open", async ({
  page,
}) => {
  // Given the picker open.
  await openSession(page);
  await page.locator(sel.PICKER_BUTTON).click();
  const modal = page.locator(sel.PICKER_MODAL);

  // When finding an unavailable row (any provider whose models are
  // not configured — with a sample-only config, every other row).
  const unavailable = modal.locator(`[${sel.MODEL_UNAVAILABLE_ATTR}]`).first();
  await expect(unavailable).toBeVisible();

  // Then the row carries no switch action attribute.
  await expect(unavailable).not.toHaveAttribute("data-webui-action", /.+/);

  // And clicking it leaves the modal open and the model unchanged.
  await unavailable.click();
  await expect(modal).toBeVisible();
  await expect(page.locator(sel.PICKER_BUTTON)).toHaveText(MODEL_ID);
});

test("refresh shows the busy state and clears it when the cache lands", async ({
  page,
}) => {
  // Given the picker open with a filter typed — and the refresh POST
  // held, so the busy window is deterministic (the scratch backend
  // answers in milliseconds; an unheld assertion races the clear).
  await openSession(page);
  await page.locator(sel.PICKER_BUTTON).click();
  const refresh = page.locator(sel.PICKER_REFRESH);
  await page.locator(sel.PICKER_FILTER).fill("sample");
  let releaseRefresh;
  const gate = new Promise((resolve) => {
    releaseRefresh = resolve;
  });
  await page.route("**/models/refresh", async (route) => {
    await gate;
    await route.continue();
  });

  // When clicking refresh.
  await refresh.click();

  // Then the button entered its busy state (disabled + aria-busy)…
  await expect(refresh).toHaveAttribute("aria-busy", "true");

  // When the held response lands.
  releaseRefresh();

  // …and the `models` SSE event cleared it, rows intact, filter kept.
  await expect(refresh).not.toHaveAttribute("aria-busy", /.+/);
  await expect(page.locator(sel.PICKER_ROW(MODEL_ID))).toBeVisible();
  await expect(page.locator(sel.PICKER_FILTER)).toHaveValue("sample");
});

test("typing into the filter keeps the modal open, Escape closes it", async ({
  page,
}) => {
  // Given the picker open.
  await openSession(page);
  await page.locator(sel.PICKER_BUTTON).click();
  const modal = page.locator(sel.PICKER_MODAL);
  await expect(modal).toBeVisible();

  // When typing a fuzzy query with real keypresses — `fill()`
  // dispatches no keydown events, which is exactly the blind spot
  // that let an every-keystroke dismissal ship unnoticed.
  await page.locator(sel.PICKER_FILTER).click();
  await page.keyboard.type("spl");

  // Then the modal is still open with the row filtered in — a
  // keystroke must never dismiss the picker.
  await expect(modal).toBeVisible();
  await expect(page.locator(sel.PICKER_ROW(MODEL_ID))).toBeVisible();

  // And Escape dismisses it.
  await page.keyboard.press("Escape");
  await expect(modal).toBeHidden();
});
