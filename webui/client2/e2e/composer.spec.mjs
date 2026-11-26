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

  // Then the queue button's label is Q. (A queued-message count may sit
  // beside it; the label is what the key is named after.)
  await expect(page.locator(sel.QUEUE)).toHaveText(/^Q/);
});

test("a second message can be queued behind a running turn", async ({ page }) => {
  // Given a running turn.
  await startBusyTurn(page);

  // When queueing one message, then another.
  await page.locator(sel.DRAFT).fill("first behind the turn");
  await page.locator(sel.QUEUE).click();
  await expect(page.locator(sel.QUEUE_DEPTH)).toHaveText("1", { timeout: 10_000 });
  await page.locator(sel.DRAFT).fill("second behind the turn");
  await page.locator(sel.QUEUE).click();

  // Then the depth reads two: both were taken.
  await expect(page.locator(sel.QUEUE_DEPTH)).toHaveText("2", { timeout: 10_000 });
  // And the button is still offered, ready for a third: it did not go away
  // or lock after the first. (Empty draft, so disabled is correct — what
  // matters is that text enables it again, which the next test asserts.)
  await page.locator(sel.DRAFT).fill("third");
  await expect(page.locator(sel.QUEUE)).toBeEnabled();
});

test("the queue button stays enabled after a message is queued", async ({ page }) => {
  // Given a running turn, with one message already queued.
  await startBusyTurn(page);
  await page.locator(sel.DRAFT).fill("already waiting");
  await page.locator(sel.QUEUE).click();
  await expect(page.locator(sel.QUEUE_DEPTH)).toHaveText("1", { timeout: 10_000 });

  // When typing another message.
  await page.locator(sel.DRAFT).fill("another");

  // Then the button accepts it.
  await expect(page.locator(sel.QUEUE)).toBeEnabled();
});

test("the placeholder names how many messages are already queued", async ({ page }) => {
  // Given a running turn, with one message already queued.
  await startBusyTurn(page);
  await page.locator(sel.DRAFT).fill("already waiting");
  await page.locator(sel.QUEUE).click();
  await expect(page.locator(sel.QUEUE_DEPTH)).toHaveText("1", { timeout: 10_000 });

  // Then the draft's placeholder says so.
  await expect(page.locator(sel.DRAFT)).toHaveAttribute(
    "placeholder",
    "Queue a message — 1 already queued",
  );
});
