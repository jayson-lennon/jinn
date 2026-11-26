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

/* ---- the vision gate's blocked path ----
 * A model not confirmed image-capable makes the gate commit the user's
 * entry and an explanatory error, then stop: no dispatch, no phase
 * change, no `ChatEntrySubmitted`. Those two entries used to reach the
 * page only with the next unrelated send, so the error showed up a
 * message late. One jinn whose sample model declares text only. */
const textOnly = test.with({ textOnly: true });

textOnly(
  "an image to a text-only model shows the gate's error at once",
  async ({ page }) => {
    // Given an image attached, and a model that declares text only.
    await attachImage(page, TINY_PNG);

    // When sending it. Nothing follows this send: no later message can
    // be what carries the error to the page.
    await page.locator(sel.SEND).click();

    // Then the gate's error is in the transcript now — it names the
    // model and points at the lever that marks a model image-capable.
    const error = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("error")}`);
    await expect(error).toBeVisible({ timeout: 10_000 });
    await expect(error).toContainText("not confirmed to support image input");

    // And the user's entry with its image is there too, not lost.
    await expect(
      page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")} ${sel.ATTACHMENT}`),
    ).toHaveCount(1);
  },
);

/* ---- one entry, once ----
 * A send with text adds an optimistic placeholder. The real entry then
 * arrives more than once — the history append and the submission each
 * carry it — and the first of those drops the placeholder. A second copy
 * arriving before the transcript's index had caught up was appended
 * beside the first, so one user entry with its image showed twice. */
test("a sent image with text shows its user entry exactly once", async ({
  page,
}) => {
  // Given one chip and text in the draft.
  await attachImage(page, TINY_PNG);

  // When sending both together, and the turn settles.
  await sendImageAndWait(page, "look at this");

  // Then there is exactly one user entry, carrying exactly one image.
  await expect(page.locator(sel.ENTRY("user"))).toHaveCount(1);
  await expect(page.locator(`${sel.ENTRY("user")} ${sel.ATTACHMENT}`)).toHaveCount(1);
});
