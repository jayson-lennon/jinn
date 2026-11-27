//! The dash's pin jump: a pinned entry the page does not hold.
//!
//! Most pins sit far past the newest page, so a tap on one asks the server
//! for history until the entry is held — one request, answered with a stream
//! of the page's own older-page frames — and the transcript then scrolls to
//! it. A pin whose entry is gone is refused, and the page says so.

import {
  awaitConnected,
  expect,
  openDash,
  seedOlderEntries,
  sel,
  sendAndWaitForEcho,
  test,
  viewedSessionId,
} from "./fixture.mjs";
import { restartJinn } from "./harness.mjs";

/** How many entries one page carries, mirroring the server's tail. */
const PAGE = 200;

/** The user entries the page's one-page tail shows for the arrangement: the
 *  page ends with the assistant echo, which is not a user entry. */
const USERS_IN_TAIL = PAGE - 2;

/** How many older entries the arrangement seeds: a page and a half. */
const COPIES = 300;

/** The ordinal of the one clone the arrangement pins — well before the
 *  page's tail, so the jump has pages to load before it is held. */
const PIN_ORDINAL = 5;

/**
 * Sends one message, grows that session past a page with one early clone
 * pinned, and restarts the instance so the page boots on a fresh session
 * and opens the long one from disk, the way a reader opening an old session
 * does.
 *
 * @returns {Promise<string>} the pinned clone's id.
 */
async function arrangePinnedFarBack(page, jinn) {
  await sendAndWaitForEcho(page, "the newest message");
  const id = await viewedSessionId(page);
  const pinned = seedOlderEntries(jinn, id, { copies: COPIES, pinOrdinal: PIN_ORDINAL });
  await restartJinn(jinn);
  await page.goto(jinn.baseUrl);
  await awaitConnected(page);
  await page.locator(sel.OPEN_PICKER).click();
  const row = page.locator(sel.PICKER_ROW).filter({ hasText: "the newest message" });
  await expect(row).toBeVisible({ timeout: 20_000 });
  await row.click();
  await expect(page.locator(sel.SESSION_LOAD)).toHaveCount(0, { timeout: 15_000 });
  return pinned;
}

test("a pin in unloaded history loads older pages until the entry is in view", async ({
  page,
  jinn,
}) => {
  // Given a session a page and a half long, with its fifth entry pinned —
  // one the page does not hold.
  const pinned = await arrangePinnedFarBack(page, jinn);
  await expect(page.locator(sel.ENTRY("user"))).toHaveCount(USERS_IN_TAIL);

  // When tapping the pin in the dash.
  await openDash(page);
  await page.locator(sel.DASH_PIN_NAMED(pinned)).click();

  // Then the dash closed, and the transcript holds and shows the pinned
  // entry: older pages were loaded until it was held, then it was scrolled to.
  await expect(page.locator(sel.DASH)).toBeHidden();
  await expect(page.locator(`${sel.TRANSCRIPT} [data-entry-id="${pinned}"]`)).toBeInViewport({
    timeout: 20_000,
  });
  // And the jump loaded only what reaching the pin needed: the page holds
  // the clones up to it plus the tail, not the whole session.
  await expect(page.locator(sel.ENTRY("user"))).toHaveCount(USERS_IN_TAIL + PIN_ORDINAL + 1);
});

test("the dash lists the far-back pin with its label", async ({ page, jinn }) => {
  // Given the same arrangement.
  const pinned = await arrangePinnedFarBack(page, jinn);

  // Then the dash's pin row carries the entry's id and the sidebar's label:
  // the user prefix, on the entry's text.
  await openDash(page);
  const row = page.locator(sel.DASH_PIN_NAMED(pinned));
  await expect(row).toBeVisible();
  await expect(row).toHaveText(/^\[TOP\] > older entry 5$/);
});
