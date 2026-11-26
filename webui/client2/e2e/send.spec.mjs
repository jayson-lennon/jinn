//! Sending a message from the composer: the optimistic entry, the
//! streamed reply, and the composer returning to its idle state.

import { expect, sel, send, sendAndWaitForEcho, test } from "./fixture.mjs";

test("a sent message echoes back as a settled assistant entry", async ({ page }) => {
  await sendAndWaitForEcho(page, "hello from web2");

  // Then the transcript holds the user's message and the model's reply.
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)).toContainText(
    "hello from web2",
  );
});

test("the composer clears the moment a message is sent", async ({ page }) => {
  await send(page, "gone from the box");

  // Then the draft is empty without waiting for any reply.
  await expect(page.locator(sel.DRAFT)).toHaveValue("");
});

test("an idle composer offers Send and no Cancel", async ({ page }) => {
  await expect(page.locator(sel.SEND)).toBeVisible();

  // Then the busy-state controls are absent.
  await expect(page.locator(sel.CANCEL)).toHaveCount(0);
});
