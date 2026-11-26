//! Image attachments: the three sources, the chips, and the sent image.

import {
  TINY_PNG,
  attachImage,
  expect,
  pasteImage,
  sel,
  sendAndWaitForEcho,
  test,
} from "./fixture.mjs";

/**
 * Sends `text` with whatever chips the composer holds, and waits for the
 * sample model's reply to settle — the moment the real user entry has
 * replaced the optimistic placeholder. An image send is slower than a
 * text one (the bytes are ingested first), so a fixed wait is not enough.
 */
async function sendImageAndWait(page, text) {
  await sendAndWaitForEcho(page, text);
}

test("picking a file through the + shows a chip", async ({ page }) => {
  // Given an idle composer.

  // When picking an image.
  await attachImage(page, TINY_PNG);

  // Then one chip names it.
  await expect(page.locator(sel.CHIP)).toHaveCount(1);
  await expect(page.locator(sel.CHIP)).toContainText(TINY_PNG.name);
});

test("pasting an image shows a chip", async ({ page }) => {
  // Given an idle composer.

  // When pasting an image into the draft.
  await pasteImage(page, TINY_PNG);

  // Then one chip is shown.
  await expect(page.locator(sel.CHIP)).toHaveCount(1);
});

test("a chip's ✕ removes it", async ({ page }) => {
  // Given one chip.
  await attachImage(page, TINY_PNG);

  // When pressing its remove.
  await page.locator(sel.CHIP_REMOVE).click();

  // Then no chip is left.
  await expect(page.locator(sel.CHIP)).toHaveCount(0);
});

test("a successful send clears every chip", async ({ page }) => {
  // Given one chip.
  await attachImage(page, TINY_PNG);

  // When sending text with it.
  await sendImageAndWait(page, "look");

  // Then no chip is left.
  await expect(page.locator(sel.CHIP)).toHaveCount(0);
});

test("a sent image renders on its user entry", async ({ page }) => {
  // Given one chip.
  await attachImage(page, TINY_PNG);

  // When sending text with it.
  await sendImageAndWait(page, "look at this");

  // Then the user entry shows one image.
  await expect(page.locator(`${sel.ENTRY("user")} ${sel.ATTACHMENT}`)).toHaveCount(1);
});

test("the rendered image loads real bytes from the attachment route", async ({ page }) => {
  // Given a sent image.
  await attachImage(page, TINY_PNG);
  await sendImageAndWait(page, "look at this");

  // Then the `<img>` decoded: it has non-zero natural size.
  const size = await page
    .locator(sel.ATTACHMENT)
    .evaluate((img) => ({ w: img.naturalWidth, h: img.naturalHeight }));
  expect(size.w).toBeGreaterThan(0);
  expect(size.h).toBeGreaterThan(0);
});

test("an image with no text can be sent", async ({ page }) => {
  // Given one chip and an empty draft.
  await attachImage(page, TINY_PNG);

  // When sending.
  await expect(page.locator(sel.SEND)).toBeEnabled();
  await page.locator(sel.SEND).click();

  // Then a real user entry lands, carrying its image.
  await expect(
    page.locator(`${sel.ENTRY("user")} ${sel.ATTACHMENT}`),
  ).toHaveCount(1, { timeout: 15_000 });
  await expect(page.locator(sel.ENTRY("error"))).toHaveCount(0);
});
