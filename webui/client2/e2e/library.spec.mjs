//! The sidebar's Sessions | Library control and the row `⋮` that moves a
//! session between them.
//!
//! Rows are addressed by session id: the wrapper and the row button
//! inside it both carry the id, so the fixture's `ROW_WRAP` names the
//! wrapper alone.

import {
  createSession,
  expect,
  openLibraryView,
  sel,
  test,
  toggleLibraryThroughMenu,
  viewedSessionId,
} from "./fixture.mjs";

/** Arranges two sessions and returns the id of the first, which stays in view. */
async function arrangeTwo(page) {
  const firstId = await viewedSessionId(page);
  await createSession(page);
  await page.reload();
  await expect(page.locator(sel.ROW_WRAP(firstId))).toHaveCount(1, { timeout: 10_000 });
  return firstId;
}

test("the rail shows the Sessions list and not the Library list", async ({ page }) => {
  // Given a fresh page.

  // Then the Sessions control is selected and the Library list is not showing.
  await expect(page.locator(sel.SESSIONS_VIEW)).toHaveAttribute("aria-selected", "true");
  await expect(page.locator(sel.LIBRARY_VIEW)).toHaveAttribute("aria-selected", "false");
});

test("the Archive button is labelled Archive", async ({ page }) => {
  // Given a fresh page.

  // Then the stored-session picker's button reads Archive.
  await expect(page.locator(sel.OPEN_PICKER)).toHaveText("Archive");
});

test("adding a session to the library removes it from the Sessions list", async ({
  page,
}) => {
  // Given two sessions, the first listed under Sessions.
  const firstId = await arrangeTwo(page);
  const first = page.locator(sel.ROW_WRAP(firstId));
  await expect(first).toHaveCount(1);

  // When adding the first to the library through its ⋮.
  await toggleLibraryThroughMenu(page, firstId);

  // Then it is gone from the Sessions list.
  await expect(first).toHaveCount(0);
});

test("a library member is listed under Library", async ({ page }) => {
  // Given the first of two sessions added to the library.
  const firstId = await arrangeTwo(page);
  await toggleLibraryThroughMenu(page, firstId);
  await expect(page.locator(sel.ROW_WRAP(firstId))).toHaveCount(0);

  // When opening the Library view.
  await openLibraryView(page);

  // Then that session, alone, is listed there.
  await expect(page.locator(`${sel.RAIL} ${sel.SESSION_ROW}`)).toHaveCount(1);
  await expect(page.locator(sel.ROW_WRAP(firstId))).toHaveCount(1);
});

test("removing a session from the library returns it to the Sessions list", async ({
  page,
}) => {
  // Given the first of two sessions is a library member, under Library.
  const firstId = await arrangeTwo(page);
  await toggleLibraryThroughMenu(page, firstId);
  await openLibraryView(page);
  await expect(page.locator(sel.ROW_WRAP(firstId))).toHaveCount(1);

  // When removing it through its ⋮.
  await toggleLibraryThroughMenu(page, firstId);

  // Then it leaves the Library list...
  await expect(page.locator(sel.ROW_WRAP(firstId))).toHaveCount(0);
  // ...and is back under Sessions.
  await page.locator(sel.SESSIONS_VIEW).click();
  await expect(page.locator(sel.ROW_WRAP(firstId))).toHaveCount(1);
});

test("a library row's ⋮ offers remove from library", async ({ page }) => {
  // Given a library member, under Library.
  const firstId = await arrangeTwo(page);
  await toggleLibraryThroughMenu(page, firstId);
  await openLibraryView(page);
  await expect(page.locator(sel.ROW_WRAP(firstId))).toHaveCount(1);

  // When opening its ⋮.
  await page.locator(`${sel.ROW_WRAP(firstId)} ${sel.ROW_MENU}`).click();

  // Then the item reads remove from library.
  await expect(page.locator(sel.ROW_MENU_LIBRARY)).toHaveText("remove from library");
});

test("a sessions row's ⋮ offers add to library", async ({ page }) => {
  // Given a session listed under Sessions.
  const firstId = await viewedSessionId(page);

  // When opening its ⋮.
  await page.locator(`${sel.ROW_WRAP(firstId)} ${sel.ROW_MENU}`).click();

  // Then the item reads add to library.
  await expect(page.locator(sel.ROW_MENU_LIBRARY)).toHaveText("add to library");
});
