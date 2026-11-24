//! Chat flow — send, queue, and cancel through the real stack.
//!
//! The `sample` backend answers plain text instantly and streams
//! `!response` word-by-word with 10–50 ms inter-token delays, so the
//! send spec proves the SSE entry lands and the cancel spec has a
//! genuinely in-flight turn to interrupt. The queue specs start a
//! `!response` turn for the same short-but-real busy window and queue
//! a plain-text message through the queue button: its answer dispatches
//! after the stream settles, so the queued entry is visible in the
//! transcript without a reload when it lands in history.

import { expect, test } from "./fixture.mjs";
import { openSession } from "./fixture.mjs";
import * as sel from "./selectors.mjs";

test("send produces a user entry and a streamed assistant reply", async ({
  page,
  jinn,
}) => {
  // Given the boot session open with an empty transcript.
  await openSession(jinn, page);

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
  jinn,
}) => {
  // Given the boot session open.
  await openSession(jinn, page);

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

test("busy composer shows queue before cancel and idle shows send alone", async ({
  page,
  jinn,
}) => {
  // Given the boot session open with an empty transcript (the busy
  // window's settle must not race the idle-fragment assertion).
  await openSession(jinn, page);

  // Then the idle composer shows the send button and neither busy
  // control — the composer is session-invariant markup (all three
  // buttons ship; the $busy signal gates their visibility).
  await expect(page.locator(sel.SEND_BUTTON)).toBeVisible();
  await expect(page.locator(sel.QUEUE_BUTTON)).not.toBeVisible();
  await expect(page.locator(sel.CANCEL_BUTTON)).not.toBeVisible();

  // When a streamed turn starts (the ~1s sample stream holds the busy
  // window).
  await page.locator(sel.INPUT).fill("!response");
  await page.locator(sel.SEND_BUTTON).click();

  // Then the busy state carries both buttons, queue first — the DOM
  // order the queue-then-cancel vocabulary is — and send hidden (the
  // composer ships all three; $busy gates which shows).
  await expect(page.locator(sel.QUEUE_BUTTON)).toBeVisible();
  const buttons = await page
    .locator(`${sel.COMPOSER} button[data-webui-action]`)
    .evaluateAll((nodes) =>
      nodes
        .filter((button) => getComputedStyle(button).display !== "none")
        .map((button) => button.getAttribute("data-webui-action")),
    );
  await expect(buttons).toEqual(["queue", "cancel"]);
  await expect(page.locator(sel.SEND_BUTTON)).not.toBeVisible();

  // And the turn settles naturally and the composer returns to the
  // single send button.
  await expect(page.locator(sel.SEND_BUTTON)).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.locator(sel.QUEUE_BUTTON)).not.toBeVisible();
  await expect(page.locator(sel.CANCEL_BUTTON)).not.toBeVisible();
});

test("queueing while busy lands the message at the turn's end", async ({
  page,
  jinn,
}) => {
  // Given the boot session open and a streamed turn in flight.
  await openSession(jinn, page);
  await page.locator(sel.INPUT).fill("!response");
  await page.locator(sel.SEND_BUTTON).click();
  await expect(page.locator(sel.QUEUE_BUTTON)).toBeVisible();

  // When queueing a plain-text message through the queue button —
  // it posts the send endpoint's form, so the reply arrives as an
  // ordinary turn after the stream settles.
  await page.locator(sel.INPUT).fill("queued follow-up");
  await page.locator(sel.QUEUE_BUTTON).click();

  // Then the optimistic entry cleared the composer's text and the
  // composer stays busy (queue and cancel showing, send hidden by
  // the $busy gate) — the send endpoint's response is unchanged.
  await expect(page.locator(sel.INPUT)).toHaveValue("");
  await expect(page.locator(sel.SEND_BUTTON)).not.toBeVisible();

  // And the queued message's answer lands in the transcript without a
  // reload — after the first turn settles, the drain dispatches the
  // queued entry and the entry-submitted convergence push carries it.
  await expect(
    page
      .locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)
      .filter({ hasText: "queued follow-up" })
  ).toBeVisible({ timeout: 10_000 });
  await expect(
    page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last()
  ).toContainText("sample response", { timeout: 10_000 });

  // And the composer returns to the single send button.
  await expect(page.locator(sel.SEND_BUTTON)).toBeVisible({
    timeout: 10_000,
  });
});

test("pressing Enter in the composer sends nothing and inserts a newline", async ({
  page,
  jinn,
}) => {
  // Given the boot session open, an observer counting POSTs to the
  // send endpoint, and the transcript's current user-entry count —
  // "nothing was sent" is a count that does not grow, not an empty
  // transcript (the boot session carries its greeting entries).
  await openSession(jinn, page);
  const sendPosts = [];
  page.on("request", (request) => {
    if (request.method() === "POST" && request.url().includes("/web/send")) {
      sendPosts.push(request.url());
    }
  });
  const userEntries = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`);
  const before = await userEntries.count();

  // When typing into the composer and pressing Enter with a real
  // key event — `fill()` dispatches no keydown events, which is
  // exactly the blind spot that let Enter-to-send ship (and the
  // blind spot that let it regress unnoticed). Mobile soft
  // keyboards fire Enter to insert newlines, so the press must
  // never send.
  await page.locator(sel.INPUT).fill("hello");
  await page.locator(sel.INPUT).press("Enter");

  // Then no send POST fired.
  expect(sendPosts).toEqual([]);

  // And the text stayed in the composer with the newline inserted
  // — the message was not submitted and not cleared.
  const value = await page.locator(sel.INPUT).inputValue();
  expect(value).toBe("hello\n");

  // And the transcript gained no user entry — nothing was sent.
  await expect(userEntries).toHaveCount(before);
});
