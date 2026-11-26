//! Loading an archived session from the Archive picker.
//!
//! An archived session is on disk and not in memory, which is the whole
//! point of the bug: activation used to refuse anything not loaded. The
//! arrangement is the user's — archive the session, and come back after
//! a restart — and each step is asserted on its own, so a picker that
//! lists a session it cannot load fails the history test and not the
//! listing one.

import {
  archiveSession,
  createSession,
  expect,
  sel,
  sendAndWaitForEcho,
  test,
} from "./fixture.mjs";
import { restartJinn } from "./harness.mjs";

/** A message only the session that gets archived holds. */
const MARKER = "a message only the archived session holds";

/** The title given to the archived session, so its picker row is findable. */
const ARCHIVED_TITLE = "the archived one";

/**
 * Arranges one archived session and leaves a different, empty session
 * in view.
 *
 * The archived session gets its message and title first, then its row
 * is archived and the instance restarts: startup loads only the
 * unarchived sessions, so afterwards it is on disk and not in memory.
 * The second session keeps an active session on the page throughout.
 *
 * Returns the archived session's picker row.
 */
async function arrangeArchivedSession(page, jinn) {
  await sendAndWaitForEcho(page, MARKER);
  await page.locator(sel.TITLE).click();
  await page.locator(sel.TITLE_EDIT).fill(ARCHIVED_TITLE);
  await page.locator(sel.TITLE_EDIT).press("Enter");
  await expect(page.locator(sel.TITLE)).toHaveText(ARCHIVED_TITLE);
  const archivedId = await page
    .locator(sel.ACTIVE_SESSION_ROW)
    .getAttribute("data-session-id");
  await createSession(page);
  await page.reload();
  await expect(page.locator(sel.SESSION_ROW)).toHaveCount(2);
  await archiveSession(jinn, archivedId);
  await restartJinn(jinn);
  await page.locator(sel.OPEN_PICKER).click();
  const row = page.locator(sel.PICKER_ROW).filter({ hasText: ARCHIVED_TITLE });
  await expect(row).toHaveCount(1, { timeout: 20_000 });
  return row;
}

test("an archived session is not in the sidebar after a restart", async ({
  jinn,
  page,
}) => {
  // Given a session that was archived before the instance restarted.
  await arrangeArchivedSession(page, jinn);
  await page.keyboard.press("Escape");

  // Then its row is gone from the sidebar, which lists only loaded sessions.
  await expect(page.locator(sel.SESSION_ROW)).toHaveCount(1);
  await expect(page.locator(sel.SESSION_ROW)).not.toContainText(ARCHIVED_TITLE);
});

test("choosing an archived session loads its history", async ({
  jinn,
  page,
}) => {
  // Given the archived session's row in the Archive picker, while the
  // page shows a session that does not hold the marker.
  const row = await arrangeArchivedSession(page, jinn);
  await expect(page.locator(sel.TRANSCRIPT)).not.toContainText(MARKER);

  // When choosing it.
  await row.click();

  // Then its history is in view.
  await expect(
    page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`),
  ).toContainText(MARKER, { timeout: 15_000 });
});

test("a loaded archived session returns to the sidebar", async ({
  jinn,
  page,
}) => {
  // Given the archived session's row in the Archive picker.
  const row = await arrangeArchivedSession(page, jinn);

  // When choosing it.
  await row.click();

  // Then it is listed again, and it is the active row.
  const loaded = page.locator(sel.SESSION_ROW).filter({ hasText: ARCHIVED_TITLE });
  await expect(loaded).toHaveCount(1, { timeout: 15_000 });
  await expect(loaded).toHaveClass(/active/);
});
