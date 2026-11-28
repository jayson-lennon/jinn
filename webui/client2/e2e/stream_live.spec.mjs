// A streamed reply grows on the page as it arrives, exactly once.
//
// This is the user-facing proof of the token path, independent of the
// stream trace: the trace (stream_trace.spec.mjs) shows the hops fire, and
// this shows the thing a reader sees.
//
// Two ways for the stream to be wrong are pinned here, both seen during
// the fix. A reply that shows its opening words and then nothing until the
// turn ends means deltas were dropped. A reply with its opening words twice
// means the delta that follows the entry's creating upsert appended text
// the upsert had already delivered. The page must end at the sentence
// itself: no less, and no more.

import { test, expect, sel, sendAndWaitForEcho, SLOW_PROMPT } from "./fixture.mjs";

// The sample provider's canned `!response`, verbatim.
const REPLY =
  "This is a sample response from the sample LLM provider. It streams tokens with a small random delay to simulate a real response.";

test("a streamed reply ends as the sentence itself, once", async ({ page }) => {
  // Given a turn that streamed token by token and has settled.
  await sendAndWaitForEcho(page, SLOW_PROMPT);

  // Then the reply is the sentence exactly: no opening words left behind
  // by a dropped delta, and none repeated by the delta after the creating
  // upsert.
  const reply = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last();
  await expect(reply).toContainText(REPLY);
  // And it holds the sentence's first words only once.
  const text = (await reply.textContent()) ?? "";
  expect(text.split("This is a sample").length - 1).toBe(1);
});
