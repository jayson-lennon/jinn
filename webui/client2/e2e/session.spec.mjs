//! Switching the active session from the rail.
//!
//! A click must do three things at once, each asserted on its own: move
//! the highlight, swap the title, and swap the transcript. A click that
//! only reaches the server (the message goes to the clicked session, the
//! page still shows the old one) passes none of these.
//!
//! Rows are found by what they show, never by position: the sidebar
//! follows the server's map order, which is not stable between boots.

import { createSession, expect, sel, sendAndWaitForEcho, test } from "./fixture.mjs";

/** The first session's marker: a message only it holds. */
const MARKER = "a message only the first session holds";

/**
 * Arranges two sessions and leaves the *new* one in view.
 *
 * Returns the two rows, found by title: the first session is auto-titled
 * with its message, and the new one reads "Untitled Session".
 */
async function arrangeTwoSessions(page, browser, jinn) {
  await sendAndWaitForEcho(page, MARKER);
  await createSession(jinn, browser);
  await page.reload();
  const first = page.locator(sel.SESSION_ROW).filter({ hasText: MARKER });
  const second = page.locator(sel.SESSION_ROW).filter({ hasText: "Untitled Session" });
  await expect(first).toHaveCount(1, { timeout: 10_000 });
  await expect(second).toHaveCount(1);
  return { first, second };
}

test("clicking a session row moves the highlight to that row", async ({
  browser,
  jinn,
  page,
}) => {
  // Given the new session is in view, so the first row is not highlighted.
  const { first } = await arrangeTwoSessions(page, browser, jinn);
  await expect(first).not.toHaveClass(/active/);

  // When clicking the first session's row.
  await first.click();

  // Then that row is highlighted, and it is the only one that is.
  await expect(first).toHaveClass(/active/);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveCount(1);
});

test("clicking a session row brings that session's history into view", async ({
  browser,
  jinn,
  page,
}) => {
  // Given the new session is in view, which does not hold the marker.
  const { first } = await arrangeTwoSessions(page, browser, jinn);
  await expect(page.locator(sel.TRANSCRIPT)).not.toContainText(MARKER);

  // When clicking the first session's row.
  await first.click();

  // Then its history is on the page.
  await expect(page.locator(sel.TRANSCRIPT)).toContainText(MARKER);
});

test("clicking a session row swaps the title to that session's", async ({
  browser,
  jinn,
  page,
}) => {
  // Given the new session is in view.
  const { first } = await arrangeTwoSessions(page, browser, jinn);
  await expect(page.locator(sel.TITLE)).toHaveText("Untitled Session");

  // When clicking the first session's row.
  await first.click();

  // Then the title bar names that session.
  await expect(page.locator(sel.TITLE)).toHaveText(MARKER);
});

test("clicking away from a session clears its history from view", async ({
  browser,
  jinn,
  page,
}) => {
  // Given the first session, with its marker, is in view.
  const { first, second } = await arrangeTwoSessions(page, browser, jinn);
  await first.click();
  await expect(page.locator(sel.TRANSCRIPT)).toContainText(MARKER);

  // When clicking the new session's row.
  await second.click();

  // Then the marker is gone: the page shows the other session's history.
  await expect(page.locator(sel.TRANSCRIPT)).not.toContainText(MARKER);
});
