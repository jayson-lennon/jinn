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
