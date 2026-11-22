//! Sessions region flow — the sidebar list and its filter.
//!
//! Alphabetical file order puts the chat specs first: by the time
//! these run, the boot session has been messaged and its title minted
//! from the first user line. The list's observable contract is that
//! the active session renders as a row — not which words the row
//! carries — so the title assertions are relative (non-empty, and
//! narrowed away by a non-matching filter).

import { expect, test } from "@playwright/test";
import * as sel from "./selectors.mjs";
import { baseUrl } from "./run.mjs";

test("sidebar lists the boot session as a row", async ({ page }) => {
  // Given the scratch jinn — by this spec's alphabetical position the
  // serial tier may hold earlier-created sessions (the mobile pivot
  // creates one), so the count is a lower bound, not a pin.
  // When navigating to the page root.
  await page.goto(`${baseUrl()}/web`);

  // Then at least one row renders, with a non-empty title.
  const rows = page.locator(sel.SESSION_ROW);
  await expect(rows.first()).toBeAttached();
  await expect(rows.first().locator(".stitle")).toBeVisible();
  const title = rows.first().locator(".stitle");
  await expect(title).toBeVisible();
  const text = await title.innerText();
  if (text.trim().length === 0) {
    throw new Error("the session row rendered with an empty title");
  }
});

test("sessions filter narrows the list", async ({ page }) => {
  // Given the page root with its rows (at least the boot session's).
  await page.goto(`${baseUrl()}/web`);
  await expect(page.locator(sel.SESSION_ROW).first()).toBeAttached();

  // When filtering for a string no title contains.
  const filter = page.locator(sel.SESSIONS_FILTER);
  await filter.fill("zzz-no-match");

  // Then the list narrows to nothing.
  await expect(page.locator(sel.SESSION_ROW)).toHaveCount(0);
});
