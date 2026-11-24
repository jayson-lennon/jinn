//! Applets flow — the menu item opens the modal, a seeded applet
//! renders one `target="_blank"` row whose href points at the
//! applets server's resolved bind, and the filter excludes a row it
//! does not match.
//!
//! The scratch applet is staged in global-setup (pre-boot, the
//! persona/skill hook's pattern): `<root>/data/jinn/applets/<id>/`
//! plus `applets.json`. The applets server binds on the modal's first
//! wake (the GET publishes it), so the row's href carries the
//! OS-resolved loopback bind.

import { expect, test } from "@playwright/test";
import { openSession } from "./nav.mjs";
import { baseUrl } from "./run.mjs";
import * as sel from "./selectors.mjs";
const { APPLET } = sel;

test("the menu item opens the modal into its host", async ({ page }) => {
  // Given the landing page (no session viewed — the Applets item
  // carries no data-show guard, the catalog is instance-wide).
  await page.goto(`${baseUrl()}/web`);
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();

  // When clicking the Applets item.
  await page.locator(sel.MENU_ITEM_APPLETS).click();

  // Then the modal renders inside its own host span (never the
  // menu's container), with the filter input and rows container
  // present.
  const modal = page.locator(sel.APPLET_MODAL);
  await expect(modal).toBeVisible();
  const host = page.locator(sel.APPLET_MODAL_HOST);
  await expect(host.locator(sel.APPLET_MODAL)).toHaveCount(1);
  await expect(page.locator(sel.APPLET_FILTER)).toBeVisible();
  await expect(page.locator(sel.APPLET_ROWS)).toBeVisible();
  // And Escape closes it (the persona modal's exact mechanism).
  await page.keyboard.press("Escape");
  await expect(modal).not.toBeVisible();
});

test("a seeded applet renders one row linking to the server bind", async ({
  page,
}) => {
  // Given the boot session open and the menu opened.
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_APPLETS).click();
  const modal = page.locator(sel.APPLET_MODAL);
  await expect(modal).toBeVisible();

  // Then the seeded applet renders exactly one row — an anchor with
  // an http href ending in the applet's path with the trailing
  // slash, opening a new tab. The attributes are asserted, not the
  // click: the applets server binds on loopback inside this run's
  // jinn and the test has no reason to drive a second page.
  const row = page.locator(sel.APPLET_ROW(APPLET.id));
  await expect(row).toHaveCount(1);
  const href = await row.getAttribute("href");
  expect(href).toMatch(/^http:\/\//);
  expect(href?.endsWith(`/applets/${APPLET.id}/`)).toBe(true);
  expect(await row.getAttribute("target")).toBe("_blank");
  expect(await row.getAttribute("rel")).toBe("noopener");
  // And the title renders in the row.
  await expect(row).toContainText(APPLET.title);
});

test("a filter that matches nothing leaves the row absent", async ({
  page,
}) => {
  // Given the applets modal open with the seeded row visible.
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_APPLETS).click();
  const modal = page.locator(sel.APPLET_MODAL);
  await expect(modal).toBeVisible();
  const row = page.locator(sel.APPLET_ROW(APPLET.id));
  await expect(row).toHaveCount(1);

  // When typing a filter no title matches.
  await page.locator(sel.APPLET_FILTER).fill("zzz-no-match");

  // Then the seeded row is withheld server-side — the rows-only
  // re-pull swapped the container, so the filter input itself
  // survives (never re-rendered mid-typing).
  await expect(row).toHaveCount(0);
  await expect(page.locator(sel.APPLET_FILTER)).toBeVisible();
});
