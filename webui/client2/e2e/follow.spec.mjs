//! Following the instance's active session, whatever the page holds.
//!
//! A change is made by a second page on the same instance, so the page
//! under test never asked for it: that is what makes the arrival a push
//! rather than a reply. Nothing the user has typed holds the page back; a
//! draft is theirs, not the session's, and it survives the switch.

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
 * Arranges two sessions with the first in view. Returns both ids: the
 * first, then the second (titled by its first message).
 */
async function arrangeTwoSessions(page) {
  const firstId = await viewedSessionId(page);
  await sendAndWaitForEcho(page, MARKER);
  await createSession(page);
  await sendAndWaitForEcho(page, "the second session's marker");
  const secondId = await viewedSessionId(page);
  await page.locator(sel.SESSION_ROW).filter({ hasText: MARKER }).click();
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    firstId,
  );
  return { firstId, secondId };
}

/** Switches the instance to the second session from another page. */
async function switchAwayFromOtherPage(page, browser) {
  const other = await browser.newPage();
  try {
    await other.goto(page.url());
    await other.locator(sel.SESSION_ROW)
      .filter({ hasText: "the second session's marker" })
      .click();
  } finally {
    await other.close();
  }
}

test("a page with no draft follows when the active session changes", async ({
  page,
  browser,
}) => {
  // Given two sessions, the first in view, and no draft.
  const { secondId } = await arrangeTwoSessions(page);

  // When another page switches the instance to the second.
  await switchAwayFromOtherPage(page, browser);

  // Then this page follows.
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    secondId,
    { timeout: 10_000 },
  );
});

test("a page holding a draft follows when the active session changes", async ({
  page,
  browser,
}) => {
  // Given the first session in view, with a draft typed.
  const { firstId, secondId } = await arrangeTwoSessions(page);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    firstId,
  );
  await page.locator(sel.DRAFT).fill("unsent text");

  // When another page switches the instance to the second.
  await switchAwayFromOtherPage(page, browser);

  // Then this page follows — the draft does not hold it back.
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    secondId,
    { timeout: 10_000 },
  );
});

test("a draft survives being switched to another session", async ({
  page,
  browser,
}) => {
  // Given the first session in view, with a draft typed.
  const { secondId } = await arrangeTwoSessions(page);
  await page.locator(sel.DRAFT).fill("unsent text");

  // When another page switches the instance to the second.
  await switchAwayFromOtherPage(page, browser);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    secondId,
    { timeout: 10_000 },
  );

  // Then the draft is exactly as it was typed.
  await expect(page.locator(sel.DRAFT)).toHaveValue("unsent text");
});

test("a row click switches session while a draft is held", async ({ page }) => {
  // Given two sessions, the first in view, with a draft typed.
  await arrangeTwoSessions(page);
  await page.locator(sel.DRAFT).fill("unsent text");
  const second = page.locator(sel.SESSION_ROW).filter({
    hasText: "the second session's marker",
  });

  // When clicking the other session's row on this page.
  await second.click();

  // Then the page switches — its own request is never declined.
  await expect(second).toHaveClass(/active/);
  // And the draft is still there.
  await expect(page.locator(sel.DRAFT)).toHaveValue("unsent text");
});
