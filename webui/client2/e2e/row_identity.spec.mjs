// A streaming entry keeps its row.
//
// The transcript's list is keyed by entry id. A delta replaces the entry's
// envelope with a new object under the same id, and the keyed list updates
// the existing row. A list keyed by the object instead would tear the row
// down and mount a new one on every delta — every 150 ms for the length of
// the turn.
//
// Two things that costs a reader are why this is pinned. Each rebuild
// re-renders the entry's markdown, which grows with the turn. And a click
// is a press and a release on the same element: a row replaced between the
// two dispatches no click at all, so a thinking header could not be opened
// while it streamed.

import { test, expect, sel, startBusyTurn } from "./fixture.mjs";

// Several flush ticks, so the entry has certainly been re-upserted.
const SETTLE_MS = 400;

// The streaming reply's row: the page's own selector for an assistant entry.
const SELECTOR = `${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`;

test("a streaming entry keeps its row across deltas", async ({ page }) => {
  // Given a turn that is still streaming.
  await startBusyTurn(page);
  const row = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last();
  await expect(row).toBeVisible();

  // When several deltas have re-upserted the entry.
  // The live node is tagged with a property the DOM cannot copy to a
  // replacement, and kept by reference so identity itself is compared.
  await page.evaluate((selector) => {
    const node = document.querySelector(selector);
    node.dataset.identityProbe = "tagged";
    window.__identityProbe = node;
  }, SELECTOR);
  await page.waitForTimeout(SETTLE_MS);

  // Then the row is the very same node: the list updated it in place
  // rather than mounting a new one.
  const identity = await page.evaluate(
    (selector) =>
      document.querySelector(selector) === window.__identityProbe &&
      window.__identityProbe.dataset.identityProbe === "tagged",
    SELECTOR,
  );
  expect(identity).toBe(true);
});
