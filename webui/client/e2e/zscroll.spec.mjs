//! Transcript scroll discipline — the page is still while tokens
//! arrive.
//!
//! Sorts after `znew-session.spec.mjs` on purpose: the session-click
//! spec needs a second session row to click, and that world belongs
//! to the z-prefixed run order (the scratch db accumulates sessions;
//! rows are addressed by `data-session-id`, never by index). Sorts
//! before `zspinner.spec.mjs` so the spinner specs' two-page turn
//! sees the same alphabetical world they were written against.
//!
//! The sample backend streams `!response` word-by-word (10–50 ms per
//! token, ~1 s total), which is exactly the window a token-follow
//! autoscroll used to ride: these specs prove the wrap's scrollTop
//! is frozen across it. The wrap's only scrolls are its `data-init`
//! bottom-scroll on insertion (fresh load, row-click swap) and the
//! composer's send-time scroll — never a server event.

import { expect, test } from "@playwright/test";
import { openSession } from "./nav.mjs";
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
 * Selects the scratch tier's one model for the viewed session — a
 * send without a selection errors instantly instead of going busy.
 * The zspinner spec's condensed copy of this flow.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves once the switch landed.
 */
async function selectModel(page) {
  await page.locator(sel.PICKER_BUTTON).click();
  await expect(page.locator(sel.PICKER_MODAL)).toBeVisible();
  await page.locator(sel.PICKER_ROW_MODEL_SPAN(sel.MODEL_ID)).click();
  // The picker button is the model display now (the redundant
  // titlebar label is gone — main's model-btn merge).
  await expect(page.locator(sel.PICKER_BUTTON)).toHaveText(sel.MODEL_ID);
}

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
}) => {
  // Given the boot session open, on the sample model, and its wrap
  // already overflowing — grown by plain prompts so "at the bottom"
  // is a position to hold, not a degenerate zero.
  await openSession(page);
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
}) => {
  // Given two sessions: the boot session holding an overflowing
  // transcript (padded plain prompts' help-text replies), and a
  // second session to click away to and back from. The boot
  // session's id is captured before creating the sibling — the
  // list renders newest-first, so the created row sorts ahead.
  await openSession(page);
  await sendPlainTurn(page, "grow one");
  await sendPlainTurn(page, "grow two");
  const grownId = await page
    .locator(sel.PAGE_ROOT)
    .getAttribute(sel.PAGE_SESSION_ATTR);
  if (!grownId) {
    throw new Error("page root carries no session id");
  }
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();

  // When creating a sibling session through the modal (navigating
  // away from the grown transcript), then clicking back to the
  // grown session — the row-click page swap that re-inserts the
  // wrap.
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  await expect(page.locator(sel.SESSION_NEW_MODAL)).toBeVisible();
  await page.locator(sel.SESSION_NEW_SUBMIT).click();
  const createdId = await page
    .locator(sel.PAGE_ROOT)
    .getAttribute(sel.PAGE_SESSION_ATTR);
  if (!createdId) {
    throw new Error("create did not navigate; page root carries no session id");
  }
  await expect(page.locator(sel.SESSION_ROW_BY_ID(createdId))).toBeVisible();
  await page.locator(sel.SESSION_ROW_BY_ID(grownId)).click();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    grownId,
  );

  // Then the swapped-in wrap sits at the bottom: its data-init
  // bottom-scroll alone lands the reader on the tail (the catch-up
  // park no longer carries any scroll keying, so this is the one
  // carrier left to prove). The wrap must actually overflow here —
  // a degenerate 0 == 0 would prove nothing.
  await expect.poll(() => maxScroll(page), { timeout: 5_000 }).toBeGreaterThan(
    50,
  );
  await expect
    .poll(scrollTop.bind(null, page), { timeout: 5_000 })
    .toBeGreaterThanOrEqual((await maxScroll(page)) - 1);
});
