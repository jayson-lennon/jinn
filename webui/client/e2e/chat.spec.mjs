//! Chat flow — send and cancel through the real stack.
//!
//! The `sample` backend answers plain text instantly and streams
//! `!response` word-by-word with 10–50 ms inter-token delays, so the
//! send spec proves the SSE entry lands and the cancel spec has a
//! genuinely in-flight turn to interrupt.

import { expect, test } from "@playwright/test";
import { openSession } from "./nav.mjs";
import * as sel from "./selectors.mjs";

test("send produces a user entry and a streamed assistant reply", async ({
  page,
}) => {
  // Given the boot session open with an empty transcript.
  await openSession(page);

  // When sending a plain prompt through the composer.
  await page.locator(sel.INPUT).fill("hello");
  await page.locator(sel.SEND_BUTTON).click();

  // Then the prompt landed as a user entry.
  const userEntry = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`).last();
  await expect(userEntry).toContainText("hello");

  // And the sample backend's reply arrived as an assistant entry.
  const assistantEntry = page
    .locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`)
    .last();
  await expect(assistantEntry).toContainText("sample response");
});

test("cancel mid-stream ends the turn and restores the send control", async ({
  page,
}) => {
  // Given the boot session open.
  await openSession(page);

  // When sending `!response` — the delayed stream — and cancelling
  // as soon as the busy controls present the cancel button. The
  // button is the turn's in-flight signal: it swaps in with the
  // busy push ~100 ms after the send and stays until the turn
  // settles (~1 s for this stream), so the click has a wide
  // window. Anchoring the click on streamed text instead would
  // race the assertion poll cadence against the stream duration —
  // a systematic loss when the poll lands after the settle.
  // Cancelling before the first token exercises the inline-settle
  // path, the exact shape that used to pin the composer on Cancel;
  // the single-writer rule (SSE push is the controls' only writer)
  // is what this spec pins.
  await page.locator(sel.INPUT).fill("!response");
  await page.locator(sel.SEND_BUTTON).click();
  await page.locator(sel.CANCEL_BUTTON).click();

  // Then the turn ends: the cancel control swaps back to send.
  await expect(page.locator(sel.SEND_BUTTON)).toBeVisible();

  // And the interrupt is on the wire: an error entry naming the
  // cancellation appears in the transcript.
  await expect(
    page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("error")}`)
  ).toContainText("Cancelled");
});
