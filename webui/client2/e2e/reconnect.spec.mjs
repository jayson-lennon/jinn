//! A dropped connection: the overlay, the automatic reconnect, and a
//! correct transcript afterwards.

import { expect, sel, sendAndWaitForEcho, test } from "./fixture.mjs";

test("a dropped socket shows the overlay and reconnects with a correct transcript", async ({
  page,
}) => {
  await sendAndWaitForEcho(page, "before the drop");

  // When the network drops out from under the page.
  await page.context().setOffline(true);

  // Then the overlay is up while disconnected.
  await expect(page.locator(sel.OVERLAY)).toBeVisible({ timeout: 5_000 });

  // When the network returns.
  await page.context().setOffline(false);

  // Then the overlay clears and the transcript is intact.
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0, { timeout: 15_000 });
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)).toContainText(
    "before the drop",
  );
});

/** How long a caught-up page is left alone after its second reconnect.
 *
 *  Long enough to span two handshake timeouts: a page whose `hello` goes
 *  unanswered closes its own socket every ten seconds, so a loop shows up
 *  as the overlay being up again well inside this window. */
const IDLE_SPAN_MS = 25_000;

test("a reconnect that missed nothing is answered and does not loop", async ({ page }) => {
  // Two offline round-trips plus the idle span outlast the default budget.
  test.setTimeout(90_000);

  // Given a page that has exchanged one message.
  await sendAndWaitForEcho(page, "before the two drops");

  // When the network drops and returns once. This round-trip is setup: it
  // brings the page back with its cursor at the newest entry and a matching
  // revision, which is the state the second drop needs.
  await page.context().setOffline(true);
  await expect(page.locator(sel.OVERLAY)).toBeVisible({ timeout: 5_000 });
  await page.context().setOffline(false);
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0, { timeout: 15_000 });

  // When the network drops and returns a second time, from a page that is
  // fully caught up, so its `hello` has nothing to resume.
  await page.context().setOffline(true);
  await expect(page.locator(sel.OVERLAY)).toBeVisible({ timeout: 5_000 });
  await page.context().setOffline(false);
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0, { timeout: 15_000 });

  // When the page is then left idle.
  await page.waitForTimeout(IDLE_SPAN_MS);

  // Then the overlay has not come back: the empty resume was answered, so
  // the page never closed a healthy socket to retry.
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0);
});
