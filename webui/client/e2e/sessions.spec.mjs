//! Sessions region flow — the sidebar list and its filter.
//!
//! Every test boots its own jinn, so the list holds exactly the one
//! session a fresh db mints. The list's observable contract is that
//! the active session renders as a row — not which words the row
//! carries — so the title assertions are relative (non-empty, and
//! narrowed away by a non-matching filter).

import { expect, test } from "./fixture.mjs";
import * as sel from "./selectors.mjs";

test("sidebar lists the boot session as a row", async ({ page, jinn }) => {
  // Given a freshly booted scratch jinn — one session, the one the
  // db mints at boot.
  // When navigating to the page root.
  await page.goto(`${jinn.baseUrl}/web`);

  // Then exactly that one row renders, with a non-empty title.
  const rows = page.locator(sel.SESSION_ROW);
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toBeAttached();
  await expect(rows.first().locator(".stitle")).toBeVisible();
  const title = rows.first().locator(".stitle");
  await expect(title).toBeVisible();
  const text = await title.innerText();
  if (text.trim().length === 0) {
    throw new Error("the session row rendered with an empty title");
  }
});
