//! Session archive — the sidebar's Archive button opens the modal over
//! every stored session, a row click activates its session, and the
//! filter narrows the rows without re-rendering the input.
//!
//! The web has no archive gesture (archiving is a TUI command that also
//! evicts the session from memory), so this spec drives the modal over
//! the instance's own loaded sessions and pins the browser wiring; the
//! loaded-vs-archived merge and the archived-only row shape are pinned
//! by the Rust region tests.

import {
  archiveSession,
  createSession,
  expect,
  openArchive,
  openSession,
  test,
  viewSession,
} from "./fixture.mjs";
import * as sel from "./selectors.mjs";

test("the Archive button opens the modal into its host", async ({
  page,
  jinn,
}) => {
  // Given the landing page.
  await page.goto(`${jinn.baseUrl}/web`);
  await expect(page.locator(sel.SESSION_ROW).first()).toBeVisible();

  // When clicking the sidebar's Archive button.
  const overlay = await openArchive(page);

  // Then the modal renders inside its own host span, with its filter
  // input and rows container.
  await expect(page.locator(sel.ARCHIVE_MODAL_HOST).locator(sel.ARCHIVE_MODAL)).toHaveCount(1);
  await expect(page.locator(sel.ARCHIVE_FILTER)).toBeVisible();
  await expect(page.locator(sel.ARCHIVE_ROWS)).toBeVisible();

  // And Escape closes it (the other modals' exact mechanism).
  await page.keyboard.press("Escape");
  await expect(overlay).not.toBeVisible();
});

test("every loaded session renders a row carrying the loaded tag", async ({
  page,
  jinn,
}) => {
  // Given two sessions — the boot session and one created through the
  // modal — and the archive modal open on the boot session's page.
  const boot = await openSession(jinn, page);
  const other = await createSession(jinn, page);
  await viewSession(jinn, page, boot);
  await openArchive(page);

  // Then each session has a row, tagged loaded.
  for (const id of [boot, other]) {
    const row = page.locator(sel.ARCHIVE_ROW_BY_ID(id));
    await expect(row).toHaveCount(1);
    await expect(row).toContainText("loaded");
  }
});

test("a row click activates its session and closes the modal", async ({
  page,
  jinn,
}) => {
  // Given two sessions — A viewed, B left behind — and the archive
  // modal open.
  const sessionA = await openSession(jinn, page);
  const sessionB = await createSession(jinn, page);
  await viewSession(jinn, page, sessionA);
  const overlay = await openArchive(page);

  // When clicking B's archive row.
  await page.locator(sel.ARCHIVE_ROW_BY_ID(sessionB)).click();

  // Then the page swaps to B and the modal is closed with the
  // navigation (the open flag is re-seeded — a modal left open would
  // survive the switch stuck over the new page).
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionB,
  );
  await expect(overlay).not.toBeVisible();
});

test("the filter narrows the rows without re-rendering the input", async ({
  page,
  jinn,
}) => {
  // Given two sessions and the archive modal open, with the filter
  // focused.
  const boot = await openSession(jinn, page);
  const other = await createSession(jinn, page);
  await viewSession(jinn, page, boot);
  await openArchive(page);
  const filter = page.locator(sel.ARCHIVE_FILTER);
  await filter.focus();

  // When typing a string no title contains.
  await filter.fill("zzz-no-match");

  // Then no row renders, and the input still holds what was typed and
  // still has focus — the rows-only re-pull never re-rendered it.
  await expect(page.locator(sel.ARCHIVE_ROW_BY_ID(boot))).toHaveCount(0);
  await expect(page.locator(sel.ARCHIVE_ROW_BY_ID(other))).toHaveCount(0);
  await expect(filter).toHaveValue("zzz-no-match");
  await expect(filter).toBeFocused();
});

test("clearing the filter restores the rows", async ({ page, jinn }) => {
  // Given the archive modal open with a filter that matches nothing.
  const boot = await openSession(jinn, page);
  await openArchive(page);
  const filter = page.locator(sel.ARCHIVE_FILTER);
  await filter.fill("zzz-no-match");
  await expect(page.locator(sel.ARCHIVE_ROW_BY_ID(boot))).toHaveCount(0);

  // When clearing the filter.
  await filter.fill("");

  // Then the rows come back.
  await expect(page.locator(sel.ARCHIVE_ROW_BY_ID(boot))).toHaveCount(1);
});

test("a session archived in the store stays reachable from the picker", async ({
  page,
  jinn,
}) => {
  // Given the landing page, a session created through the modal, and
  // that session marked archived in the store (the statement jinn's own
  // store runs) while this instance keeps it loaded.
  await page.goto(`${jinn.baseUrl}/web`);
  const session = await createSession(jinn, page);
  archiveSession(jinn, session);
  await page.goto(`${jinn.baseUrl}/web`);

  // When opening the archive modal.
  await openArchive(page);

  // Then the archived-in-the-store session has a row, and clicking it
  // activates through the ordinary route.
  const row = page.locator(sel.ARCHIVE_ROW_BY_ID(session));
  await expect(row).toHaveCount(1);
  await row.click();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    session,
  );
});
