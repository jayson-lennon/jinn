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
  archiveThroughMenu,
  archiveThroughRowMenu,
  createSession,
  expect,
  openMenu,
  sel,
  sendAndWaitForEcho,
  startBusyTurn,
  test,
  viewedSessionId,
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

// ---------------------------------------------------------------------------
// Archiving from /web2: the hamburger menu's Archive session item, and each
// rail row's `⋮`. The row leaves the rail at once, the page lands on another
// session, and a busy session is refused with a toast instead.
// ---------------------------------------------------------------------------

/** Renames the session in view, so its picker row is findable by title. */
async function renameViewedSession(page, title) {
  await page.locator(sel.TITLE).click();
  await page.locator(sel.TITLE_EDIT).fill(title);
  await page.locator(sel.TITLE_EDIT).press("Enter");
  await expect(page.locator(sel.TITLE)).toHaveText(title);
}

test("the menu's Archive session archives the session in view", async ({
  page,
}) => {
  // Given a session in view with a message only it holds.
  await sendAndWaitForEcho(page, MARKER);
  await renameViewedSession(page, ARCHIVED_TITLE);
  const archivedId = await viewedSessionId(page);

  // When archiving it through the menu.
  await archiveThroughMenu(page);

  // Then its row leaves the rail.
  await expect(page.locator(sel.ROW_WRAP(archivedId))).toHaveCount(0);
  // And the Archive picker lists it: the store wrote it as archived.
  await page.locator(sel.OPEN_PICKER).click();
  await expect(
    page.locator(sel.PICKER_ROW).filter({ hasText: ARCHIVED_TITLE }),
  ).toHaveCount(1);
  await page.keyboard.press("Escape");
});

test("archiving the viewed session moves the page to another session", async ({
  page,
}) => {
  // Given two sessions, the first in view.
  const first = await viewedSessionId(page);
  await createSession(page);
  await page.locator(sel.ROW_WRAP(first)).locator(sel.SESSION_ROW).click();
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    first,
  );

  // When the viewed session is archived through the menu.
  await archiveThroughMenu(page);

  // Then its row is gone, and the page is on the other session.
  await expect(page.locator(sel.ROW_WRAP(first))).toHaveCount(0);
  await expect(page.locator(sel.SESSION_ROW)).toHaveCount(1);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).not.toHaveAttribute(
    "data-session-id",
    first,
  );
});

test("archiving the last session leaves a fresh one active", async ({
  page,
}) => {
  // Given the instance's only session in view, holding a message.
  await sendAndWaitForEcho(page, MARKER);
  const only = await viewedSessionId(page);

  // When it is archived through the menu.
  await archiveThroughMenu(page);

  // Then a fresh, empty session is active and its transcript is blank.
  await expect(page.locator(sel.ROW_WRAP(only))).toHaveCount(0);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveCount(1);
  await expect(page.locator(sel.TRANSCRIPT)).not.toContainText(MARKER);
  await expect(page.locator(sel.TITLE)).not.toHaveText(ARCHIVED_TITLE);
});

test("a row's archive item archives that row and leaves the viewed session", async ({
  page,
}) => {
  // Given two sessions, the second in view.
  const first = await viewedSessionId(page);
  await createSession(page);
  await sendAndWaitForEcho(page, MARKER);
  const viewed = await viewedSessionId(page);
  expect(viewed).not.toBe(first);

  // When the first row's `⋮` archives it.
  await archiveThroughRowMenu(page, first);

  // Then that row is gone, and the viewed session is untouched.
  await expect(page.locator(sel.ROW_WRAP(first))).toHaveCount(0);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    viewed,
  );
  await expect(page.locator(sel.TRANSCRIPT)).toContainText(MARKER);
});

test("archiving a busy session is refused with a toast and the row remains", async ({
  page,
}) => {
  // Given a session running a turn.
  const busy = await viewedSessionId(page);
  await startBusyTurn(page);

  // When it is archived through the menu.
  await archiveThroughMenu(page);

  // Then the page is told the session is busy, and the row is still there.
  const toast = page.locator(sel.TOAST).filter({ hasText: "session is busy" });
  await expect(toast).toHaveCount(1);
  await expect(page.locator(sel.ROW_WRAP(busy))).toHaveCount(1);
});

test("a toast dismisses on tap", async ({ page }) => {
  // Given a refusal that raises a toast.
  await startBusyTurn(page);
  await archiveThroughMenu(page);
  const toast = page.locator(sel.TOAST).filter({ hasText: "session is busy" });
  await expect(toast).toHaveCount(1);

  // When it is tapped.
  await toast.click();

  // Then it is gone.
  await expect(page.locator(sel.TOAST)).toHaveCount(0);
});

test("a toast dismisses on its own", async ({ page }) => {
  // Given a refusal that raises a toast.
  await startBusyTurn(page);
  await archiveThroughMenu(page);
  await expect(
    page.locator(sel.TOAST).filter({ hasText: "session is busy" }),
  ).toHaveCount(1);

  // Then it goes away by itself.
  await expect(page.locator(sel.TOAST)).toHaveCount(0, { timeout: 10_000 });
});

test("no refusal renders under the composer", async ({ page }) => {
  // Given a refusal, raised through a busy archive.
  await startBusyTurn(page);
  await archiveThroughMenu(page);
  await expect(page.locator(sel.TOAST)).toHaveCount(1);

  // Then nothing is rendered under the composer: the toast is the one
  // refusal surface.
  await expect(page.locator('[data-testid="composer-error"]')).toHaveCount(0);
});

test("the menu offers Archive session while a session is active", async ({
  page,
}) => {
  // Given the page is viewing a session.
  await openMenu(page);

  // Then the item is offered.
  await expect(page.locator(sel.MENU_ITEM_ARCHIVE)).toHaveCount(1);
});
