//! The composer row: the draft, the `+`, and the buttons on one line.

import { expect, sel, startBusyTurn, test } from "./fixture.mjs";

/** The vertical centre of an element, in page pixels. */
async function centreY(page, selector) {
  const box = await page.locator(selector).boundingBox();
  if (!box) throw new Error(`no box for ${selector}`);
  return box.y + box.height / 2;
}

test("the draft, the attach control, and Send share one line", async ({ page }) => {
  // Given an idle composer.

  // Then the three controls' vertical centres agree.
  const draft = await centreY(page, sel.DRAFT);
  const attach = await centreY(page, sel.ATTACH);
  const send = await centreY(page, sel.SEND);
  expect(Math.abs(draft - attach)).toBeLessThan(2);
  expect(Math.abs(draft - send)).toBeLessThan(2);
});

test("the attach control and Send are as tall as the draft", async ({ page }) => {
  // Given an idle composer.

  // Then no control is shorter than the draft.
  const draft = (await page.locator(sel.DRAFT).boundingBox()).height;
  const attach = (await page.locator(sel.ATTACH).boundingBox()).height;
  const send = (await page.locator(sel.SEND).boundingBox()).height;
  expect(attach).toBeGreaterThanOrEqual(draft - 1);
  expect(send).toBeGreaterThanOrEqual(draft - 1);
});

test("the busy-phase queue button reads Q", async ({ page }) => {
  // Given a running turn.
  await startBusyTurn(page);

  // Then the queue button's text is exactly Q.
  await expect(page.locator(sel.QUEUE)).toHaveText("Q");
});
