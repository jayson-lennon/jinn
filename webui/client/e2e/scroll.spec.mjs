//! Transcript scroll discipline — the page is still while tokens
//! arrive. Every test boots its own jinn and grows its own transcript;
//! rows are addressed by `data-session-id`, never by index.
//!
//! The sample backend streams `!response` word-by-word (10–50 ms per
//! token, ~1 s total), which is exactly the window a token-follow
//! autoscroll used to ride: these specs prove the wrap's scrollTop
//! is frozen across it. The wrap's only scrolls are its `data-init`
//! bottom-scroll on insertion (fresh load, row-click swap), the
//! composer's send-time scroll, and the `catchupCursor` effect's
//! landing scroll when the stream's from-zero catch-up completes
//! (which is what puts the row-click swap at the bottom) — never a
//! live token event.

import {
  createSession,
  expect,
  openSession,
  selectModel,
  switchToSession,
  test,
} from "./fixture.mjs";
import * as sel from "./selectors.mjs";

/** Polls the wrap's scrollTop (an integer; 0 before any scroll). */
const scrollTop = (page) =>
  page.evaluate(
    (selector) =>
      Math.round(document.querySelector(selector)?.scrollTop ?? -1),
    sel.TRANSCRIPT_WRAP,
  );

/** The wrap's maximum scrollTop (scrollHeight minus clientHeight). */
const maxScroll = (page) =>
  page.evaluate(
    (selector) => {
      const el = document.querySelector(selector);
      return el ? el.scrollHeight - el.clientHeight : -1;
    },
    sel.TRANSCRIPT_WRAP,
  );

/**
 * Sends one plain prompt and waits for its OWN reply — the shortest
 * full turn. The prompt carries a fenced code block so the reply
 * chain overflows the wrap deterministically: a fenced block renders
 * as `<pre>` (white-space: pre-wrap), preserving every newline, where
 * prose pads collapse (markdown folds single newlines into one
 * paragraph — the first run's lesson). The wait is on the assistant
 * COUNT growing: "last entry contains X" matches the previous turn's
 * reply and races the next send into a busy rejection.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @param {string} prompt the text to send (first line mints the row
 *   title).
 * @returns {Promise<void>} resolves once this turn's reply landed and
 *   the turn settled.
 */
async function sendPlainTurn(page, prompt) {
  const assistants = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`);
  const before = await assistants.count();
  const fence =
    "\n```\n" +
    Array.from({ length: 30 }, (_, i) => `line ${i}`).join("\n") +
    "\n```\n";
  await page.locator(sel.INPUT).fill(`${prompt}${fence}`);
  await page.locator(sel.SEND_BUTTON).click();
  await expect(assistants).toHaveCount(before + 1, { timeout: 15_000 });
  // And the turn settled — the send control returned — so the next
  // send cannot land on a busy session.
  await expect(page.locator(sel.SEND_BUTTON)).toBeVisible({
    timeout: 15_000,
  });
}

test("send scrolls once and the streamed reply arrives without scrolling", async ({
  page,
  jinn,
}) => {
  // Given the boot session open, on the sample model, and its wrap
  // already overflowing — grown by plain prompts so "at the bottom"
  // is a position to hold, not a degenerate zero.
  await openSession(jinn, page);
  await selectModel(page);
  await sendPlainTurn(page, "filler one");
  await sendPlainTurn(page, "filler two");
  await expect.poll(() => maxScroll(page), { timeout: 5_000 }).toBeGreaterThan(
    50,
  );

  // When sending `!response` — the word-by-word stream — and
  // letting the whole turn settle. The floor is measured BEFORE the
  // click: the send's own user entry arrives afterwards and grows
  // the wrap without scrolling it (that arrival-keyed follow is the
  // behavior being removed).
  const floorBeforeSend = await maxScroll(page);
  await page.locator(sel.INPUT).fill("!response");
  await page.locator(sel.SEND_BUTTON).click();

  // Then the send gesture itself scrolled to the bottom: by the
  // time the busy controls arrive, the wrap is at the floor it had
  // when the send fired.
  await expect(page.locator(sel.CANCEL_BUTTON)).toBeVisible();
  const afterSend = await scrollTop(page);
  expect(afterSend).toBeGreaterThan(0);
  expect(afterSend).toBeGreaterThanOrEqual(floorBeforeSend - 1);

  // And the streamed reply lands entry by entry with the wrap
  // frozen: from the cancel button's appearance through the
  // settle, every scrollTop observation is the send-time value.
  const assistant = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`);
  await expect(assistant.last()).toContainText("sample response", {
    timeout: 15_000,
  });
  await expect(page.locator(sel.SEND_BUTTON)).toBeVisible({ timeout: 15_000 });
  expect(await scrollTop(page)).toBe(afterSend);
});

test("a session row click lands the page at the transcript bottom", async ({
  page,
  jinn,
}) => {
  // Given two sessions: the boot session holding an overflowing
  // transcript (padded plain prompts' help-text replies), and a
  // second session to click away to and back from. The boot
  // session's id is captured before creating the sibling — the
  // list renders newest-first, so the created row sorts ahead.
  const grownId = await openSession(jinn, page);
  await sendPlainTurn(page, "grow one");
  await sendPlainTurn(page, "grow two");

  // When creating a sibling session through the modal (navigating
  // away from the grown transcript), then clicking back to the
  // grown session — the row-click page swap that re-inserts the
  // wrap.
  const createdId = await createSession(jinn, page);
  await expect(page.locator(sel.SESSION_ROW_BY_ID(createdId))).toBeVisible();
  await switchToSession(page, grownId);

  // Then the swapped-in wrap sits at the bottom: the stream's
  // from-zero catch-up appended the history into the empty transcript
  // and completed with the `catchupCursor` patch — the wrap's effect
  // answered it with the landing scroll (the data-init scroll fires
  // before any content exists, so this carrier is what lands the
  // reader on the tail). The wrap must actually overflow here —
  // a degenerate 0 == 0 would prove nothing.
  await expect.poll(() => maxScroll(page), { timeout: 5_000 }).toBeGreaterThan(
    50,
  );
  await expect
    .poll(scrollTop.bind(null, page), { timeout: 5_000 })
    .toBeGreaterThanOrEqual((await maxScroll(page)) - 1);
});
