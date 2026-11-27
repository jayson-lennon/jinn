//! The row `⋮` menu's teardown + archive: the terminal's `x`, end to end.
//!
//! The gesture is one publish of the TUI's own `CloseSession`, and the
//! lifecycle actor owns everything downstream — so these specs assert the
//! observable contract only: what the menu lists, that a due teardown
//! runs (a marker file appears), that the fallback archives silently, and
//! that a busy session is refused. The busy dot is the one thing the web
//! adds on its own, so the in-flight and failed teardowns are asserted on
//! the row itself, against a teardown slow enough to observe.

import {
  createSession,
  createSessionWithLifecycle,
  expect,
  openSession,
  sel,
  startBusyTurn,
  teardownThroughRowMenu,
  test,
} from "./fixture.mjs";
import fs from "node:fs/promises";
import path from "node:path";

/** The staged lifecycle whose teardown writes a marker file. */
const MARKER_LIFECYCLE = "marker-teardown";

/** The staged lifecycle whose teardown sleeps long enough to be observed. */
const SLOW_LIFECYCLE = "slow-teardown";

/** The staged lifecycle whose teardown exits non-zero. */
const FAILING_LIFECYCLE = "failing-teardown";

/** Where the staged teardowns write their markers, relative to the scratch root. */
const MARKER = ".teardown-marker";
const SLOW_DONE = ".slow-teardown-done";

/** The slow teardown's own sleep, in ms, with a margin for the spawn. */
const SLOW_TEARDOWN_MS = 4_000;

test("the row menu lists teardown + archive between the library toggle and archive", async ({
  page,
}) => {
  // Given a session, and its row's `⋮` pressed.
  const id = await createSession(page);
  await page.locator(`${sel.ROW_WRAP(id)} ${sel.ROW_MENU}`).click();

  // When the popup is open.
  // Then its three items are, in order: the library toggle, teardown +
  // archive, archive.
  const labels = await page
    .locator(`${sel.ROW_WRAP(id)} [data-testid="row-menu-popup"] .row-menu-item`)
    .allTextContents();
  expect(labels).toEqual(["add to library", "teardown + archive", "archive"]);
});

test("teardown + archive on a session with a due teardown runs it, then the row leaves the rail", async ({
  page,
  jinn,
}) => {
  // Given a session created with the marker lifecycle — setup ran, so a
  // teardown is due.
  const id = await createSessionWithLifecycle(page, MARKER_LIFECYCLE);

  // When the page tears it down and archives it.
  await teardownThroughRowMenu(page, id);

  // Then the teardown command ran — the marker file exists — and the row
  // is gone from the rail.
  await expect
    .poll(async () => fs.access(path.join(jinn.root, MARKER)).then(() => true, () => false), {
      timeout: 10_000,
    })
    .toBe(true);
  await expect(page.locator(sel.ROW_WRAP(id))).toHaveCount(0);
});

test("teardown + archive on a session with nothing to tear down archives with no toast", async ({
  page,
}) => {
  // Given a session created with the blank lifecycle — no setup ran, so no
  // teardown is ever due.
  const id = await createSession(page);

  // When the page tears it down and archives it.
  await teardownThroughRowMenu(page, id);

  // Then the session is archived — the row is gone — and the page said
  // nothing about it: no toast.
  await expect(page.locator(sel.ROW_WRAP(id))).toHaveCount(0);
  await expect(page.locator(sel.TOAST)).toHaveCount(0);
});

test("teardown + archive on a busy session is refused with a toast and the row stays", async ({
  page,
}) => {
  // Given a session running a turn.
  const id = await createSession(page);
  await openSession(page, id);
  await startBusyTurn(page);

  // When the page tears it down and archives it.
  await teardownThroughRowMenu(page, id);

  // Then the page reports the refusal and the session is neither archived
  // nor torn down: the row is still listed.
  await expect(page.locator(sel.TOAST)).toBeVisible();
  await expect(page.locator(sel.ROW_WRAP(id))).toBeVisible();
});

test("the row shows its busy dot while its teardown runs", async ({ page, jinn }) => {
  // Given a session created with the slow lifecycle — its teardown sleeps.
  const id = await createSessionWithLifecycle(page, SLOW_LIFECYCLE);

  // When the page tears it down and archives it.
  await teardownThroughRowMenu(page, id);

  // Then the row shows its busy dot while the teardown is in flight.
  await expect(page.locator(`${sel.ROW_WRAP(id)} ${sel.ROW_BUSY}`)).toBeVisible({ timeout: 5_000 });

  // Leave the instance no teardown still sleeping past the test.
  await waitForFile(jinn.root, SLOW_DONE);
});

test("a page reloaded mid-teardown shows the busy dot from its first frame", async ({ page, jinn }) => {
  // Given a teardown in flight on a slow-lifecycle session.
  const id = await createSessionWithLifecycle(page, SLOW_LIFECYCLE);
  await teardownThroughRowMenu(page, id);
  await expect(page.locator(`${sel.ROW_WRAP(id)} ${sel.ROW_BUSY}`)).toBeVisible({ timeout: 5_000 });

  // When the page reloads while the teardown is still running.
  await page.reload();
  await page.locator(sel.ROW_WRAP(id)).waitFor({ timeout: 5_000 });

  // Then the fresh page's first session list already carries the flag:
  // the dot is on the row, with no event needed to light it.
  await expect(page.locator(`${sel.ROW_WRAP(id)} ${sel.ROW_BUSY}`)).toBeVisible();

  // Leave the instance no teardown still sleeping past the test.
  await waitForFile(jinn.root, SLOW_DONE);
});

test("a failed teardown leaves the session open and drops the busy dot", async ({ page }) => {
  // Given a session created with the failing lifecycle — its teardown
  // exits non-zero.
  const id = await createSessionWithLifecycle(page, FAILING_LIFECYCLE);

  // When the page tears it down and archives it.
  await teardownThroughRowMenu(page, id);

  // Then the session is neither archived nor left spinning: the row is
  // still listed, and its dot is gone.
  await expect(page.locator(sel.ROW_WRAP(id))).toBeVisible();
  await expect(page.locator(`${sel.ROW_WRAP(id)} ${sel.ROW_BUSY}`)).toHaveCount(0, {
    timeout: 5_000,
  });
});

/** Resolves once `relative` exists under `root`; never throws. */
async function waitForFile(root, relative) {
  await expect
    .poll(async () => fs.access(path.join(root, relative)).then(() => true, () => false), {
      timeout: SLOW_TEARDOWN_MS + 5_000,
    })
    .toBe(true);
}
