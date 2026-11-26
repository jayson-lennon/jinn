//! The transcript is a bounded tail: a long session connects on its newest
//! entries, older ones load a page at a time on scroll, and a tool result's
//! full output loads on demand when its disclosure opens.
//!
//! No `/web2` gesture produces a long history or a truncated tool result,
//! so every test seeds the scratch store the way the store itself writes
//! rows — clones of a real persisted entry, and a real `ToolResult` map —
//! and restarts the instance to load it.

import {
  SEEDED_FULL_OUTPUT,
  awaitConnected,
  expect,
  seedOlderEntries,
  seedTruncatedToolResult,
  sel,
  sendAndWaitForEcho,
  test,
  viewedSessionId,
} from "./fixture.mjs";
import { restartJinn } from "./harness.mjs";

/** How many entries one page carries, mirroring the server's tail. */
const PAGE = 200;

/**
 * Sends one message, grows that session past a page in the store, and
 * restarts the instance — which boots on a fresh session, as a restart
 * does — then picks the long one from the Archive picker so its history
 * loads from disk the way a reader opening an old session does.
 *
 * @param {import('@playwright/test').Page} page the test's own page.
 * @param {{ root: string, baseUrl: string }} jinn the test's own instance.
 * @param {number} copies how many older entries to seed.
 * @returns {Promise<void>}
 */
async function arrangeLongHistory(page, jinn, copies) {
  await sendAndWaitForEcho(page, "the newest message");
  const id = await viewedSessionId(page);
  seedOlderEntries(jinn, id, { copies });
  await restartJinn(jinn);
  await page.goto(jinn.baseUrl);
  await awaitConnected(page);
  // The restart's fresh session is empty, so the long one is opened by its
  // title, through the disk-load path.
  await page.locator(sel.OPEN_PICKER).click();
  const row = page.locator(sel.PICKER_ROW).filter({ hasText: "the newest message" });
  await expect(row).toBeVisible({ timeout: 20_000 });
  await row.click();
  await expect(page.locator(sel.SESSION_LOAD)).toHaveCount(0, { timeout: 15_000 });
}

test("a long session connects on its newest entries, not its whole history", async ({
  page,
  jinn,
}) => {
  await arrangeLongHistory(page, jinn, PAGE + 50);

  // Then the newest message is in the transcript.
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`).last()).toContainText(
    "the newest message",
  );
  // And the transcript holds exactly one page of entries — not the whole
  // page-and-a-half history — with the oldest seeded entry not yet among
  // them.
  const entries = page.locator(`${sel.TRANSCRIPT} .entry`);
  await expect(entries).toHaveCount(PAGE, { timeout: 10_000 });
  await expect(entries.first()).not.toContainText("older entry 0");
});

test("scrolling to the top loads one older page", async ({ page, jinn }) => {
  await arrangeLongHistory(page, jinn, PAGE + 50);

  // When the reader scrolls to the top of the transcript.
  await page.locator(sel.TRANSCRIPT).evaluate((node) => {
    node.scrollTop = 0;
  });

  // Then the oldest page's entries arrive: all of them are now held.
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)).toHaveCount(PAGE + 1 + 50, {
    timeout: 10_000,
  });
  // And the spinner is gone, with nothing left to load.
  await expect(page.locator(sel.OLDER_SPINNER)).toHaveCount(0);
  await expect(page.locator(sel.OLDER_MORE)).toHaveCount(0);
});

test("the viewport stays on the same entry when an older page lands", async ({
  page,
  jinn,
}) => {
  await arrangeLongHistory(page, jinn, PAGE + 50);

  // Given the reader scrolled to the very top, on the oldest held entry.
  const transcript = page.locator(sel.TRANSCRIPT);
  await transcript.evaluate((node) => {
    node.scrollTop = 0;
  });
  const anchor = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`).first();
  await expect(anchor).toBeInViewport();

  // When the older page lands above it.
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)).toHaveCount(
    PAGE + 1 + 50,
    { timeout: 10_000 },
  );

  // Then the entry the reader was on is still in the viewport.
  await expect(anchor).toBeInViewport();
});

/**
 * Picks `title` from the Archive picker and waits for its load to settle.
 *
 * @param {import('@playwright/test').Page} page the test's own page.
 * @param {string} title the session's title, as the picker shows it.
 * @returns {Promise<void>}
 */
async function openPickedSession(page, title) {
  await page.locator(sel.OPEN_PICKER).click();
  const row = page.locator(sel.PICKER_ROW).filter({ hasText: title });
  await expect(row).toBeVisible({ timeout: 20_000 });
  await row.click();
  await expect(page.locator(sel.SESSION_LOAD)).toHaveCount(0, { timeout: 15_000 });
}

test("a truncated tool result's full output loads on demand", async ({ page, jinn }) => {
  // Given a session whose last entry is a truncated tool result.
  await sendAndWaitForEcho(page, "run the tool");
  const id = await viewedSessionId(page);
  seedTruncatedToolResult(jinn, id);
  await restartJinn(jinn);
  await page.goto(jinn.baseUrl);
  await awaitConnected(page);
  await openPickedSession(page, "run the tool");

  // Then the result is on the page, with its full output not yet on it.
  const result = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("tool_result")}`).last();
  await expect(result).toBeVisible();
  await expect(page.locator(sel.TRANSCRIPT)).not.toContainText(SEEDED_FULL_OUTPUT);

  // When the result opens, and then its "full output" disclosure.
  await result.locator("details.entry-body > summary").click();
  const disclosure = result.locator(sel.TOOL_OUTPUT);
  await expect(disclosure).toBeVisible();
  await disclosure.locator("summary").first().click();

  // Then the full output arrives from the route, and the spinner is gone.
  await expect(disclosure).toContainText(SEEDED_FULL_OUTPUT, { timeout: 10_000 });
  await expect(page.locator(sel.TOOL_OUTPUT_SPINNER)).toHaveCount(0);
});
