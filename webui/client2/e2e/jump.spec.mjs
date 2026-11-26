//! The transcript's two jump buttons — jump to the start, jump to the end.
//!
//! Both float over the transcript's right edge and each is shown only while
//! the view is off that end, by the same band the follow rule uses. Every
//! test boots its own jinn and grows its own transcript; every assertion
//! reads real geometry from the scroll box, never a fixed sleep.

import { expect, send, sendAndWaitForEcho, sel, test } from "./fixture.mjs";

const TRANSCRIPT = '[data-testid="transcript"]';
const JUMP_START = '[data-testid="jump-start"]';
const JUMP_END = '[data-testid="jump-end"]';
const DESKTOP = { width: 1280, height: 800 };
const PHONE = { width: 390, height: 844 };

/** The band, in pixels, within which a reader still counts as at an end. */
const EDGE_BAND_PX = 80;

/**
 * Grows the transcript until it overflows its box several times over, and
 * waits until it actually has — the geometry, not a timer.
 */
async function growTranscript(page) {
  for (let at = 0; at < 12; at += 1) {
    await sendAndWaitForEcho(page, `message ${at}`);
  }
  await expect
    .poll(async () => (await readScroll(page)).max, { timeout: 5_000 })
    .toBeGreaterThan(400);
}

/** `{ top, max, client }` of the scroll box; `max` is scrollHeight − clientHeight. */
function readScroll(page) {
  return page.locator(TRANSCRIPT).evaluate((node) => ({
    top: node.scrollTop,
    max: node.scrollHeight - node.clientHeight,
    client: node.clientHeight,
  }));
}

/** Scrolls the box up by `pixels` with a real scroll event, so the page measures. */
function scrollUpBy(page, pixels) {
  return page.locator(TRANSCRIPT).evaluate((node, by) => {
    node.scrollTop -= by;
  }, pixels);
}

/** Scrolls the box to the middle of its content, with a real scroll event. */
function scrollToMiddle(page) {
  return page.locator(TRANSCRIPT).evaluate((node) => {
    node.scrollTop = node.scrollHeight / 2;
  });
}

test("a short transcript shows no jump buttons", async ({ page }) => {
  // Given the boot session's transcript, short enough to fit its box.
  await page.setViewportSize(DESKTOP);
  await sendAndWaitForEcho(page, "a single message");

  // Then neither jump button is rendered: the reader is at both ends.
  await expect(page.locator(JUMP_START)).toHaveCount(0);
  await expect(page.locator(JUMP_END)).toHaveCount(0);
});

test("a reader at the end is offered the start and not the end", async ({
  page,
}) => {
  // Given a long transcript, followed to the bottom.
  await page.setViewportSize(DESKTOP);
  await growTranscript(page);

  // Then the start button is there, and the end button is not.
  await expect(page.locator(JUMP_START)).toBeVisible();
  await expect(page.locator(JUMP_END)).toHaveCount(0);
});

test("a reader scrolled up is offered the end", async ({ page }) => {
  // Given a long transcript, followed to the bottom.
  await page.setViewportSize(DESKTOP);
  await growTranscript(page);

  // When scrolling up, well clear of the end band.
  await scrollUpBy(page, 300);

  // Then the end button appears.
  await expect(page.locator(JUMP_END)).toBeVisible();
});

test("a reader away from the top is offered the start", async ({ page }) => {
  // Given a long transcript, followed to the bottom — far below the top.
  await page.setViewportSize(DESKTOP);
  await growTranscript(page);
  const before = await readScroll(page);
  expect(before.top).toBeGreaterThan(EDGE_BAND_PX);

  // Then the start button is there.
  await expect(page.locator(JUMP_START)).toBeVisible();
});

test("jump to end lands at the bottom and hides itself", async ({ page }) => {
  // Given a long transcript, scrolled well up from the bottom.
  await page.setViewportSize(DESKTOP);
  await growTranscript(page);
  await scrollUpBy(page, 300);
  await expect(page.locator(JUMP_END)).toBeVisible();

  // When clicking jump-to-end.
  await page.locator(JUMP_END).click();

  // Then the box is at its bottom, and the button is gone.
  const after = await readScroll(page);
  expect(Math.abs(after.top - after.max)).toBeLessThanOrEqual(1);
  await expect(page.locator(JUMP_END)).toHaveCount(0);
});

test("jump to start lands at the top and hides itself", async ({ page }) => {
  // Given a long transcript, at the bottom.
  await page.setViewportSize(DESKTOP);
  await growTranscript(page);
  await expect(page.locator(JUMP_START)).toBeVisible();

  // When clicking jump-to-start.
  await page.locator(JUMP_START).click();

  // Then the box is at its top, and the button is gone.
  expect((await readScroll(page)).top).toBe(0);
  await expect(page.locator(JUMP_START)).toHaveCount(0);
});

test("the jump buttons sit inside the transcript's right edge, top and bottom", async ({
  page,
}) => {
  // Given a long transcript, read in the middle — both buttons shown. The
  // composer's box is taken with none shown, and again with both.
  await page.setViewportSize(DESKTOP);
  await growTranscript(page);
  const composerBefore = await page
    .locator('[data-testid="composer"]')
    .boundingBox();
  await scrollToMiddle(page);
  await expect(page.locator(JUMP_START)).toBeVisible();
  await expect(page.locator(JUMP_END)).toBeVisible();

  // Then each button lies inside the transcript's box, at its right edge.
  const box = await page.locator(TRANSCRIPT).boundingBox();
  const start = await page.locator(JUMP_START).boundingBox();
  const end = await page.locator(JUMP_END).boundingBox();
  for (const button of [start, end]) {
    expect(button.x).toBeGreaterThanOrEqual(box.x);
    expect(button.y).toBeGreaterThanOrEqual(box.y);
    expect(button.x + button.width).toBeLessThanOrEqual(box.x + box.width);
    expect(button.y + button.height).toBeLessThanOrEqual(box.y + box.height);
    // Right-aligned: its right edge is within 20px of the box's.
    expect(box.x + box.width - (button.x + button.width)).toBeLessThanOrEqual(20);
  }

  // And the start button is in the top quarter, the end button in the bottom.
  const quarter = box.height / 4;
  expect(start.y).toBeLessThan(box.y + quarter);
  expect(end.y + end.height).toBeGreaterThan(box.y + box.height - quarter);

  // And nothing moved when they appeared: the composer is where it was.
  const composerAfter = await page
    .locator('[data-testid="composer"]')
    .boundingBox();
  expect(composerAfter).toEqual(composerBefore);
});

test("a stream arriving at the bottom keeps the reader there and the end button hidden", async ({
  page,
}) => {
  // Given a long transcript, followed to the bottom — the end button hidden.
  await page.setViewportSize(DESKTOP);
  await growTranscript(page);
  await expect(page.locator(JUMP_END)).toHaveCount(0);

  // When a reply streams in token by token, and the turn settles.
  await send(page, "!response");
  await expect(page.locator(sel.SEND)).toBeVisible({ timeout: 15_000 });

  // Then the reader is still at the bottom, and the button still hidden.
  const after = await readScroll(page);
  expect(Math.abs(after.top - after.max)).toBeLessThanOrEqual(1);
  await expect(page.locator(JUMP_END)).toHaveCount(0);
});

test("the jump buttons are touch-sized and on screen on a phone", async ({
  page,
}) => {
  // Given a phone viewport, and a long transcript read in the middle.
  await page.setViewportSize(PHONE);
  await growTranscript(page);
  await scrollToMiddle(page);
  await expect(page.locator(JUMP_START)).toBeVisible();
  await expect(page.locator(JUMP_END)).toBeVisible();

  // Then each button is at least 40px tall, and inside the viewport.
  const viewport = page.viewportSize();
  for (const selector of [JUMP_START, JUMP_END]) {
    const box = await page.locator(selector).boundingBox();
    expect(box.height).toBeGreaterThanOrEqual(40);
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.y).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(viewport.width);
    expect(box.y + box.height).toBeLessThanOrEqual(viewport.height);
  }
});
