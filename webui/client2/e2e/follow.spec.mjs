//! Following the instance's active session — and declining to while the
//! page holds unsent text.
//!
//! A change is made by a second page on the same instance, so the page
//! under test never asked for it: that is what makes the arrival a push
//! rather than a reply.

import {
  createSession,
  expect,
  sel,
  sendAndWaitForEcho,
  test,
  viewedSessionId,
} from "./fixture.mjs";

/** What only the first session holds, to find its row by. */
const MARKER = "only the first session holds this";

/**
 * Arranges two sessions with the first in view and a marker typed into
 * its draft. Returns the first session's id.
 */
async function arrangeFirstWithDraft(page) {
  const firstId = await viewedSessionId(page);
  await sendAndWaitForEcho(page, MARKER);
  await createSession(page);
  await page.locator(sel.SESSION_ROW).filter({ hasText: MARKER }).click();
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute("data-session-id", firstId);
  await page.locator(sel.DRAFT).fill("unsent text");
  return firstId;
}

/** Switches the instance to the second (untitled) session from another page. */
async function switchAwayFromOtherPage(page, browser) {
  const other = await browser.newPage();
  try {
    await other.goto(page.url());
    await other.locator(sel.SESSION_ROW).filter({ hasText: "Untitled Session" }).click();
    await expect(other.locator(sel.ACTIVE_SESSION_ROW)).not.toHaveAttribute(
      "data-session-id",
      await viewedSessionId(page),
      { timeout: 10_000 },
    );
  } finally {
    await other.close();
  }
}

test("a page with no draft follows when the active session changes", async ({
  page,
  browser,
}) => {
  // Given two sessions, the first in view, and no draft.
  const firstId = await viewedSessionId(page);
  await sendAndWaitForEcho(page, MARKER);
  await createSession(page);
  await page.locator(sel.SESSION_ROW).filter({ hasText: MARKER }).click();
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute("data-session-id", firstId);

  // When another page switches the instance to the second session.
  await switchAwayFromOtherPage(page, browser);

  // Then this page follows to it, with an empty transcript.
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).not.toHaveAttribute("data-session-id", firstId);
  await expect(page.locator(sel.ENTRY("user"))).toHaveCount(0);
});

test("a page holding a draft stays put when the active session changes", async ({
  page,
  browser,
}) => {
  // Given the first session in view, with a draft typed.
  const firstId = await arrangeFirstWithDraft(page);

  // When another page switches the instance to the second session.
  await switchAwayFromOtherPage(page, browser);

  // Then this page stays on the first, and the draft is intact.
  await page.waitForTimeout(600);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute("data-session-id", firstId);
  await expect(page.locator(sel.DRAFT)).toHaveValue("unsent text");
});

test("clearing the draft does not jump to the session the page declined", async ({
  page,
  browser,
}) => {
  // Given a page that declined a switch to the second session, holding a draft.
  const firstId = await arrangeFirstWithDraft(page);
  await switchAwayFromOtherPage(page, browser);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute("data-session-id", firstId);

  // When the draft is cleared, with no further change to the active session.
  await page.locator(sel.DRAFT).fill("");
  await page.waitForTimeout(800);

  // Then the page is still on the first session: the switch it declined
  // is not replayed.
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute("data-session-id", firstId);
});

test("after clearing the draft the page follows the next change", async ({
  page,
  browser,
}) => {
  // Given a page that declined a switch, whose draft is now cleared.
  const firstId = await arrangeFirstWithDraft(page);
  await switchAwayFromOtherPage(page, browser);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute("data-session-id", firstId);
  await page.locator(sel.DRAFT).fill("");
  await page.waitForTimeout(600);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute("data-session-id", firstId);

  // When the instance's active session changes again — away and back,
  // then away once more, each from the other page.
  await page.locator(sel.SESSION_ROW).filter({ hasText: MARKER }).click();
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute("data-session-id", firstId);
  await switchAwayFromOtherPage(page, browser);

  // Then this page follows that change.
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).not.toHaveAttribute("data-session-id", firstId);
});
