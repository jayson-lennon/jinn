//! The OpenRouter model market in the web UI: the chip's chooser, the
//! two-density table, the header sort, the detail page and its popout,
//! "Use this model", and the saved preferences.
//!
//! Every test here stages the market's disk cache (`market: true`), so the
//! actor serves the catalog with no network. The failed-refresh path is not
//! here: the OpenRouter base URL is hardcoded, so a refresh from this tier
//! reaches the real API or fails by environment, never deterministically.
//! That path is proven where it is deterministic — the stale push in the
//! Rust tests, the toast and the standing rows in the store's unit tests.

import fs from "node:fs";
import path from "node:path";

import {
  expect,
  MARKET_CHEAP_ID,
  MARKET_CHEAP_NAME,
  MARKET_DEAR_NAME,
  MARKET_ENDPOINT_NAME,
  MARKET_ENDPOINT_TAG,
  MODEL_LABEL,
  openMarket,
  sel,
  test,
} from "./fixture.mjs";

/** The filter that matches the cheap listing alone. */
const CHEAP_FILTER = "e2e-alpha";

/** Closes the market through its own close button. */
async function closeMarket(page) {
  await page.locator(sel.MARKET_CLOSE).click();
  await expect(page.locator(sel.MARKET_MODAL)).toHaveCount(0);
}

/** The id of the first row on screen, polled until it is `id`. */
async function expectFirstRow(page, id) {
  await expect
    .poll(() => page.locator(sel.MARKET_ROW).first().getAttribute("data-model-id"))
    .toBe(id);
}

/** The right edges of a row's/header's cells, by class suffix. */
async function rightEdges(page, selector, classes) {
  return page.locator(selector).first().evaluate((el, names) => {
    const out = {};
    for (const name of names) {
      const cell = el.querySelector(`.${name}`);
      out[name] = cell === null ? null : Math.round(cell.getBoundingClientRect().right);
    }
    return out;
  }, classes);
}

const CELLS = ["m-name", "m-proj", "m-in", "m-out", "m-cache", "m-eci", "m-vals"];

test.with({ market: true })("the market lists the staged models cheapest first", async ({ page }) => {
  // Given the staged catalog.
  // When opening the market.
  await openMarket(page);

  // Then both listings are listed, the cheap one first under the default
  // PROJ ascending sort.
  await expect(page.locator(sel.MARKET_ROW)).toHaveCount(2);
  await expectFirstRow(page, MARKET_CHEAP_ID);
});

test.with({ market: true })("the status line names the mix and the count", async ({ page }) => {
  // Given the staged catalog.
  // When opening the market.
  await openMarket(page);

  // Then the status line carries the fixed mix and the catalog's size.
  const status = page.locator('[data-testid="market-status"]');
  await expect(status).toContainText("mix: fixed 10M·98%·50k");
  await expect(status).toContainText("2 models");
});

test.with({ market: true })("the filter narrows the rows to the match and highlights it", async ({ page }) => {
  // Given the open market.
  await openMarket(page);

  // When filtering for the cheap listing's author.
  await page.locator(sel.MARKET_FILTER).fill(CHEAP_FILTER);

  // Then only that listing remains, with its match marked.
  await expect(page.locator(sel.MARKET_ROW)).toHaveCount(1);
  await expect(page.locator(sel.MARKET_ROW)).toHaveAttribute("data-model-id", MARKET_CHEAP_ID);
  await expect(page.locator(`${sel.MARKET_ROW} mark`).first()).toHaveText(CHEAP_FILTER);
});

test.with({ market: true })("the filter is empty on every open", async ({ page }) => {
  // Given a market closed while filtered.
  await openMarket(page);
  await page.locator(sel.MARKET_FILTER).fill(CHEAP_FILTER);
  await expect(page.locator(sel.MARKET_ROW)).toHaveCount(1);
  await closeMarket(page);

  // When reopening it.
  await openMarket(page);

  // Then the filter is empty and both rows are back.
  await expect(page.locator(sel.MARKET_FILTER)).toHaveValue("");
  await expect(page.locator(sel.MARKET_ROW)).toHaveCount(2);
});

test.with({ market: true })("the default sort is PROJ ascending", async ({ page }) => {
  // Given the open market.
  await openMarket(page);

  // Then the PROJ header is the active sort, ascending.
  await expect(page.locator('[data-testid="market-sort-proj"]')).toHaveAttribute("aria-sort", "ascending");
  await expect(page.locator('[data-testid="market-sort-proj"]')).toContainText("↑");
});

test.with({ market: true })("tapping a header sorts by it in its natural direction", async ({ page }) => {
  // Given the open market, where the dear listing has the VALS score.
  await openMarket(page);

  // When tapping the VALS header.
  await page.locator('[data-testid="market-sort-vals"]').click();

  // Then VALS is the active sort descending, and the scored row leads.
  await expect(page.locator('[data-testid="market-sort-vals"]')).toHaveAttribute("aria-sort", "descending");
  await expectFirstRow(page, "openrouter/e2e-beta/e2e-dear:free");
});

test.with({ market: true })("a second tap on the active header reverses it", async ({ page }) => {
  // Given the open market, where the dear listing is dear on OUT and
  // carries no cache price, so it projects last under PROJ ascending.
  await openMarket(page);
  await expectFirstRow(page, MARKET_CHEAP_ID);

  // When tapping the PROJ header once — it is already the active sort,
  // ascending — the tap reverses it.
  await page.locator('[data-testid="market-sort-proj"]').click();

  // Then the direction is descending, and the dear listing leads.
  await expect(page.locator('[data-testid="market-sort-proj"]')).toHaveAttribute("aria-sort", "descending");
  await expectFirstRow(page, "openrouter/e2e-beta/e2e-dear:free");
});

test.with({ market: true })("the sort and direction survive a close and reopen", async ({ page }) => {
  // Given a market closed while sorted by VALS ascending.
  await openMarket(page);
  await page.locator('[data-testid="market-sort-vals"]').click();
  await page.locator('[data-testid="market-sort-vals"]').click();
  await closeMarket(page);

  // When reopening it.
  await openMarket(page);

  // Then VALS is still the active sort, ascending.
  await expect(page.locator('[data-testid="market-sort-vals"]')).toHaveAttribute("aria-sort", "ascending");
});

test.with({ market: true })("the mix toggle flips to the tree mix and survives a reopen", async ({ page }) => {
  // Given the open market on the fixed mix.
  await openMarket(page);
  await expect(page.locator(sel.MARKET_STATUS)).toContainText("mix: fixed");

  // When toggling the mix to the session tree's usage.
  await page.locator('[data-testid="market-mix"]').click();

  // Then the toggle reads pressed. A fresh session has no measured usage,
  // so the resolved mix degrades to the fixed label — the toggle, not the
  // label, is what the page remembers.
  await expect(page.locator('[data-testid="market-mix"]')).toHaveAttribute("aria-pressed", "true");
  await closeMarket(page);

  // When reopening it.
  await openMarket(page);

  // Then the mix is still the tree's.
  await expect(page.locator('[data-testid="market-mix"]')).toHaveAttribute("aria-pressed", "true");
});

test.with({ market: true })("tapping a row opens its detail with its endpoints", async ({ page }) => {
  // Given the open market.
  await openMarket(page);

  // When tapping the cheap listing.
  await page.locator(sel.MARKET_ROW).filter({ hasText: MARKET_CHEAP_NAME }).click();

  // Then the detail page names the model and lists the sentinel, then its
  // one provider endpoint.
  await expect(page.locator('[data-testid="market-detail-name"]')).toContainText(MARKET_CHEAP_NAME);
  const endpoints = page.locator(sel.MARKET_ENDPOINT);
  await expect(endpoints).toHaveCount(2);
  await expect(endpoints.nth(0)).toContainText("Default");
  await expect(endpoints.nth(1)).toContainText(MARKET_ENDPOINT_NAME);
});

test.with({ market: true })("the detail's facts line carries the context length", async ({ page }) => {
  // Given the cheap listing's detail page.
  await openMarket(page);
  await page.locator(sel.MARKET_ROW).filter({ hasText: MARKET_CHEAP_NAME }).click();

  // Then its facts line names the context window.
  await expect(page.locator('[data-testid="market-facts"]')).toContainText("200k context");
});

test.with({ market: true })("the popout opens the model's openrouter.ai page in a new tab", async ({ page }) => {
  // Given the cheap listing's detail page.
  await openMarket(page);
  await page.locator(sel.MARKET_ROW).filter({ hasText: MARKET_CHEAP_NAME }).click();

  // Then the popout is a new-tab link to the model's page.
  const popout = page.locator('[data-testid="market-popout"]');
  await expect(popout).toHaveAttribute("href", `https://openrouter.ai/${MARKET_CHEAP_NAME}`);
  await expect(popout).toHaveAttribute("target", "_blank");
});

test.with({ market: true })("the popout keeps a variant suffix in the URL", async ({ page }) => {
  // Given the dear listing's detail page.
  await openMarket(page);
  await page.locator(sel.MARKET_ROW).filter({ hasText: MARKET_DEAR_NAME }).click();

  // Then the popout's URL carries the `:free` suffix.
  await expect(page.locator('[data-testid="market-popout"]')).toHaveAttribute(
    "href",
    `https://openrouter.ai/${MARKET_DEAR_NAME}`,
  );
});

test.with({ market: true })("Use this model switches the session's model", async ({ page }) => {
  // Given the cheap listing's detail page, on the sentinel.
  await openMarket(page);
  await page.locator(sel.MARKET_ROW).filter({ hasText: MARKET_CHEAP_NAME }).click();

  // When using the model.
  await page.locator('[data-testid="market-use"]').click();

  // Then the market closes and the chip names the new model.
  await expect(page.locator(sel.MARKET_MODAL)).toHaveCount(0);
  await expect(page.locator(sel.MODEL_CHIP)).not.toHaveText(MODEL_LABEL);
});

test.with({ market: true })("Use this model on an endpoint row pins that endpoint", async ({ page, jinn }) => {
  // Given the cheap listing's detail page.
  await openMarket(page);
  await page.locator(sel.MARKET_ROW).filter({ hasText: MARKET_CHEAP_NAME }).click();

  // When highlighting its one provider endpoint and using the model.
  await page.locator(`${sel.MARKET_ENDPOINT}[data-tag="${MARKET_ENDPOINT_TAG}"]`).click();
  await page.locator('[data-testid="market-use"]').click();
  await expect(page.locator(sel.MARKET_MODAL)).toHaveCount(0);

  // Then the pin is a per-model default in providers.toml, named for
  // that endpoint.
  await expect
    .poll(() => fs.readFileSync(path.join(jinn.root, "config", "jinn", "providers.toml"), "utf8"))
    .toContain(`model = "${MARKET_CHEAP_ID}"`);
  const toml = fs.readFileSync(path.join(jinn.root, "config", "jinn", "providers.toml"), "utf8");
  expect(toml).toContain(`tag = "${MARKET_ENDPOINT_TAG}"`);
});

test.with({ market: true })("Use this model on the Default row clears the pin", async ({ page, jinn }) => {
  // Given the cheap listing pinned to its provider endpoint.
  await openMarket(page);
  await page.locator(sel.MARKET_ROW).filter({ hasText: MARKET_CHEAP_NAME }).click();
  await page.locator(`${sel.MARKET_ENDPOINT}[data-tag="${MARKET_ENDPOINT_TAG}"]`).click();
  await page.locator('[data-testid="market-use"]').click();
  await expect(page.locator(sel.MARKET_MODAL)).toHaveCount(0);
  await expect
    .poll(() => fs.readFileSync(path.join(jinn.root, "config", "jinn", "providers.toml"), "utf8"))
    .toContain(`tag = "${MARKET_ENDPOINT_TAG}"`);

  // When reopening the detail and using the Default (auto-route) row.
  await openMarket(page);
  await page.locator(sel.MARKET_ROW).filter({ hasText: MARKET_CHEAP_NAME }).click();
  await page.locator(sel.MARKET_ENDPOINT).first().click();
  await page.locator('[data-testid="market-use"]').click();
  await expect(page.locator(sel.MARKET_MODAL)).toHaveCount(0);

  // Then the pin is gone from providers.toml.
  await expect
    .poll(() => fs.readFileSync(path.join(jinn.root, "config", "jinn", "providers.toml"), "utf8"))
    .not.toContain(`tag = "${MARKET_ENDPOINT_TAG}"`);
});

test.with({ market: true })("the current model is marked and its Use button is disabled", async ({ page }) => {
  // Given the cheap listing in use.
  await openMarket(page);
  await page.locator(sel.MARKET_ROW).filter({ hasText: MARKET_CHEAP_NAME }).click();
  await page.locator('[data-testid="market-use"]').click();
  await expect(page.locator(sel.MARKET_MODAL)).toHaveCount(0);

  // When reopening the market.
  await openMarket(page);

  // Then its row is marked current.
  const row = page.locator(sel.MARKET_ROW).filter({ hasText: MARKET_CHEAP_NAME });
  await expect(row.locator('[data-testid="market-current"]')).toBeVisible();

  // And on its detail page the Use button is disabled.
  await row.click();
  await expect(page.locator('[data-testid="market-use"]')).toBeDisabled();
});

test.with({ market: true })("Escape steps back from the detail page, then closes the market", async ({ page }) => {
  // Given the cheap listing's detail page.
  await openMarket(page);
  await page.locator(sel.MARKET_ROW).filter({ hasText: MARKET_CHEAP_NAME }).click();

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the list is back, the market still open.
  await expect(page.locator(sel.MARKET_ROW)).toHaveCount(2);

  // When pressing Escape again.
  await page.keyboard.press("Escape");

  // Then the market closes.
  await expect(page.locator(sel.MARKET_MODAL)).toHaveCount(0);
});

test.onPhone({ market: true })("on a phone the rows are two lines with the header over their columns", async ({ page }) => {
  // Given the open market at 390px.
  await openMarket(page);

  // Then the name and PROJ share the first line while the strip sits
  // below: the strip's top is under the name's bottom.
  const first = page.locator(sel.MARKET_ROW).first();
  const nameBox = await first.locator(".m-name").boundingBox();
  const inBox = await first.locator(".m-in").boundingBox();
  expect(inBox.y).toBeGreaterThanOrEqual(nameBox.y + nameBox.height - 1);

  // And the header's cells land over the row's.
  const head = await rightEdges(page, ".market-head", CELLS);
  const row = await rightEdges(page, sel.MARKET_ROW, CELLS);
  for (const cell of CELLS) expect(head[cell], cell).toBe(row[cell]);

  // And the list does not scroll sideways.
  const list = page.locator('[data-testid="market-list"]');
  const widths = await list.evaluate((el) => [el.scrollWidth, el.clientWidth]);
  expect(widths[0]).toBeLessThanOrEqual(widths[1]);
});

test.with({ market: true })("on a wide screen the rows are one line with every column beside the name", async ({ page }) => {
  // Given the open market at the default desktop viewport.
  await openMarket(page);

  // Then the name and the strip share one line: every cell's vertical
  // centre sits within the row's single line.
  const first = page.locator(sel.MARKET_ROW).first();
  const boxes = await first.evaluate((el) =>
    ["m-name", "m-proj", "m-in", "m-out", "m-cache", "m-eci", "m-vals"].map((name) => {
      const r = el.querySelector(`.${name}`).getBoundingClientRect();
      return r.top + r.height / 2;
    }),
  );
  const mid = boxes[0];
  for (const y of boxes) expect(Math.abs(y - mid)).toBeLessThan(4);

  // And the header's cells land over the row's.
  const head = await rightEdges(page, ".market-head", CELLS);
  const row = await rightEdges(page, sel.MARKET_ROW, CELLS);
  for (const cell of CELLS) expect(head[cell], cell).toBe(row[cell]);

  // And the modal is a centred dialog, not full-bleed.
  const modal = await page.locator(sel.MARKET_MODAL).boundingBox();
  expect(modal.width).toBeLessThanOrEqual(860);
});
