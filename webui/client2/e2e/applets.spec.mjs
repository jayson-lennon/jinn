//! The hamburger menu's Applets item: the modal it opens over the
//! saved-app catalog, the filter, and the new-tab link each row is while
//! the applets server is serving.
//!
//! The staging writes one applet into the data tree before boot, so the
//! catalog's first rescan runs on a catalog that already holds it. The
//! link tests read the server's bind from the status line rather than
//! hardcoding a port: the applets server binds an OS-assigned port, so
//! its address is the instance's own.

import { APPLET_ID, APPLET_TITLE, expect, openApplets, openMenu, sel, test } from "./fixture.mjs";

/** The status line's bind: `serving http://<bind>` → `<bind>`. */
function bindFromStatus(statusText) {
  const match = /^serving http:\/\/(\S+)$/.exec(statusText);
  if (!match) throw new Error(`applets status is not serving: ${statusText}`);
  return match[1];
}

test("the menu offers an Applets item while a session is active", async ({
  page,
}) => {
  // Given an open menu.
  await openMenu(page);

  // Then it lists Applets.
  await expect(page.locator(sel.MENU_ITEM_APPLETS)).toBeVisible();
});

test.with({ applet: true })(
  "the Applets item opens the modal listing the saved applet",
  async ({ page }) => {
    // Given the boot session, whose data tree holds the staged applet.
    // When opening the applets modal through the menu.
    await openApplets(page);

    // Then the menu closed, and the modal lists that applet's title.
    await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
    await expect(page.locator(sel.APPLET_ROW_NAMED(APPLET_ID))).toHaveText(APPLET_TITLE);
  },
);

test.with({ applet: true })(
  "the status line reads serving with the applets server's bind",
  async ({ page }) => {
    // Given the open applets modal.
    await openApplets(page);

    // Then the status line names the serving bind: a loopback address with
    // a port.
    const status = page.locator(sel.APPLET_STATUS);
    await expect(status).toHaveAttribute("data-serving", "true");
    await expect(status).toHaveText(/^serving http:\/\/127\.0\.0\.1:\d+$/);
  },
);

test.with({ applet: true })(
  "each row is a new-tab link to the applet under the serving bind",
  async ({ page }) => {
    // Given the open applets modal.
    await openApplets(page);

    // Then the row is an anchor to the applet's URL under the status
    // line's bind, opening in a new tab.
    const bind = bindFromStatus(await page.locator(sel.APPLET_STATUS).innerText());
    const row = page.locator(sel.APPLET_ROW_NAMED(APPLET_ID));
    await expect(row).toHaveAttribute("href", `http://${bind}/applets/${APPLET_ID}/`);
    await expect(row).toHaveAttribute("target", "_blank");
    await expect(row).toHaveAttribute("rel", "noopener");
  },
);

test.with({ applet: true })(
  "the linked applet page is served at the row's href",
  async ({ page, request }) => {
    // Given the open applets modal.
    await openApplets(page);

    // When fetching the row's href.
    const href = await page.locator(sel.APPLET_ROW_NAMED(APPLET_ID)).getAttribute("href");
    const response = await request.get(href);

    // Then the applets server returns the staged artifact.
    expect(response.ok()).toBe(true);
    expect(await response.text()).toContain("e2e-scratch-applet-marker");
  },
);

test.with({ applet: true })(
  "typing in the filter keeps its text and marks the matched run",
  async ({ page }) => {
    // Given the open applets modal.
    await openApplets(page);

    // When typing a filter the staged applet's title matches.
    await page.locator(sel.APPLETS_FILTER).fill("scra");

    // Then the input keeps its text, the row is still listed, and the
    // match is marked.
    await expect(page.locator(sel.APPLETS_FILTER)).toHaveValue("scra");
    const row = page.locator(sel.APPLET_ROW_NAMED(APPLET_ID));
    await expect(row).toBeVisible();
    await expect(row.locator("mark")).toHaveText("scra");
  },
);

test.with({ applet: true })(
  "a filter nothing matches lists no rows",
  async ({ page }) => {
    // Given the open applets modal.
    await openApplets(page);

    // When typing a filter no title matches.
    await page.locator(sel.APPLETS_FILTER).fill("zzz-no-match");

    // Then the input keeps its text, and no row is listed.
    await expect(page.locator(sel.APPLETS_FILTER)).toHaveValue("zzz-no-match");
    await expect(page.locator(sel.APPLET_ROW)).toHaveCount(0);
  },
);

test.with({ applet: true })("Escape closes the applets modal", async ({
  page,
}) => {
  // Given the open applets modal.
  await openApplets(page);

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the modal closed.
  await expect(page.locator(sel.APPLETS_MODAL)).toBeHidden();
});

test.with({ applet: true })(
  "each applet row renders on one line",
  async ({ page }) => {
    // Given the open applets modal, at a desktop width.
    await page.setViewportSize({ width: 1100, height: 700 });
    await openApplets(page);

    // Then the row is a single line's height, and its title does not wrap.
    const row = page.locator(sel.APPLET_ROW_NAMED(APPLET_ID));
    const height = (await row.boundingBox()).height;
    expect(height).toBeLessThan(48);
    const whiteSpace = await row
      .locator(".picker-name")
      .evaluate((el) => getComputedStyle(el).whiteSpace);
    expect(whiteSpace).toBe("nowrap");
  },
);
