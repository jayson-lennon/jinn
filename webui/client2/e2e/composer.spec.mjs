//! The composer row: the draft, the `+`, and the buttons on one line.

import { TINY_PNG, attachImage, expect, sel, startBusyTurn, test } from "./fixture.mjs";

/** Narrower than the 768px breakpoint: a phone. */
const PHONE = { width: 390, height: 800 };
/** The narrowest phone the row must still hold together on. */
const NARROW = { width: 320, height: 800 };

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

/* ---- the phone composer ----
 * The row is the same on a phone as at desktop: the draft, `+` and the
 * buttons on one line, the chips above them. It used to wrap — the draft
 * took a whole line and the buttons dropped below — and, while busy, the
 * two buttons made the column wider than the phone and pushed the draft
 * off the left edge. */

test("on a phone the draft, the attach control, and Send share one line", async ({
  page,
}) => {
  // Given an idle composer at phone width.
  await page.setViewportSize(PHONE);

  // Then the three controls' vertical centres agree.
  const draft = await centreY(page, sel.DRAFT);
  expect(Math.abs(draft - (await centreY(page, sel.ATTACH)))).toBeLessThan(2);
  expect(Math.abs(draft - (await centreY(page, sel.SEND)))).toBeLessThan(2);
});

test("on a phone the busy composer keeps its row", async ({ page }) => {
  // Given a busy composer at phone width, with Q and Cancel shown.
  await page.setViewportSize(PHONE);
  await startBusyTurn(page);

  // Then the draft, `+`, Q and Cancel all share one line.
  const draft = await centreY(page, sel.DRAFT);
  expect(Math.abs(draft - (await centreY(page, sel.ATTACH)))).toBeLessThan(2);
  expect(Math.abs(draft - (await centreY(page, sel.QUEUE)))).toBeLessThan(2);
  expect(Math.abs(draft - (await centreY(page, sel.CANCEL)))).toBeLessThan(2);
});

test("on a narrow phone the busy composer does not overflow the viewport", async ({
  page,
}) => {
  // Given the narrowest phone, busy.
  await page.setViewportSize(NARROW);
  await startBusyTurn(page);

  // Then the draft starts at the composer's padding, not off the left
  // edge, and every control's right edge is inside the viewport.
  const draft = await page.locator(sel.DRAFT).boundingBox();
  expect(draft.x).toBeGreaterThanOrEqual(0);
  for (const selector of [sel.ATTACH, sel.QUEUE, sel.CANCEL]) {
    const box = await page.locator(selector).boundingBox();
    expect(box.x + box.width).toBeLessThanOrEqual(NARROW.width);
  }
});

test("on a phone the chips sit above the composer row", async ({ page }) => {
  // Given a pending image at phone width.
  await page.setViewportSize(PHONE);
  await attachImage(page, TINY_PNG);

  // Then the chip's bottom edge is at or above the draft's top edge.
  const chip = await page.locator(sel.CHIP).boundingBox();
  const draft = await page.locator(sel.DRAFT).boundingBox();
  expect(chip.y + chip.height).toBeLessThanOrEqual(draft.y);
});

test("on a phone the composer row is as tall as it is at desktop", async ({
  page,
}) => {
  // Given the desktop composer's height.
  await page.setViewportSize({ width: 1280, height: 800 });
  const desktop = (await page.locator(sel.DRAFT).boundingBox()).height;

  // When the viewport is a phone.
  await page.setViewportSize(PHONE);

  // Then the draft, `+` and Send are all that same height — the row does
  // not shrink on a phone.
  expect((await page.locator(sel.DRAFT).boundingBox()).height).toBe(desktop);
  expect((await page.locator(sel.ATTACH).boundingBox()).height).toBe(desktop);
  expect((await page.locator(sel.SEND).boundingBox()).height).toBe(desktop);
});

