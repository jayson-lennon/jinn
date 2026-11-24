//! Session-switch interstitial — the loading overlay a session-row
//! click raises until the activation response's page swap lands.
//!
//! Sorts after `zskills.spec.mjs` on purpose: it creates a second
//! session through the modal (the `znew-session` flow) and needs the
//! multi-session world the earlier specs establish. The overlay is
//! feedback for the activate POST's whole round trip — in-memory
//! activation can clear it within milliseconds, so the spec's timing
//! is made deterministic by intercepting the `*/activate` POST and
//! delaying its fulfillment: the overlay must be visible during the
//! delay, and hidden again once the response lands and the swap
//! re-seeds the `switching` signal.
//!
//! The route intercepts ONLY the activate POST — the page's SSE
//! stream `@get` retries infinitely and a broad route pattern would
//! poison the page.

import { expect, test } from "@playwright/test";
import * as sel from "./selectors.mjs";
import { openSession } from "./nav.mjs";
import { baseUrl } from "./run.mjs";

/** The phone viewport the drawer tests run at (mobile.spec's 390×844). */
const PHONE = { width: 390, height: 844 };

/**
 * Creates a session through the new-session modal and lands on its
 * page — the znew-session flow, condensed to the id it returns.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<string>} the created session's id.
 */
async function createSession(page) {
  const beforeId = await page
    .locator(sel.PAGE_ROOT)
    .getAttribute(sel.PAGE_SESSION_ATTR);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  const modal = page.locator(sel.SESSION_NEW_MODAL);
  await expect(modal).toBeVisible();
  await page.locator(sel.SESSION_NEW_SUBMIT).click();
  const root = page.locator(sel.PAGE_ROOT);
  // The create's swap is async: the id must CHANGE before this
  // returns, or the caller reads the pre-create session and clicks
  // the viewed row (inert).
  await expect(root).not.toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    beforeId ?? "",
  );
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
  const createdId = await root.getAttribute(sel.PAGE_SESSION_ATTR);
  if (!createdId) {
    throw new Error("create did not navigate; page root carries no session id");
  }
  await expect(page).toHaveURL(
    new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}${createdId}`),
  );
  return createdId;
}

test("switching shows the interstitial until the new page lands", async ({
  page,
}) => {
  // Given two sessions: A viewed, B created and left behind — and an
  // activate POST whose fulfillment is delayed until the spec says so.
  await openSession(page);
  const sessionA = await page
    .locator(sel.PAGE_ROOT)
    .getAttribute(sel.PAGE_SESSION_ATTR);
  const sessionB = await createSession(page);
  await page.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionA}`);
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionA,
  );

  let releaseActivate;
  const gate = new Promise((resolve) => {
    releaseActivate = resolve;
  });
  await page.route("**/activate", async (route) => {
    await gate;
    await route.continue();
  });

  // When clicking B's row.
  await page.locator(sel.SESSION_ROW_BY_ID(sessionB)).click();

  // Then the interstitial shows while the response is held — no
  // round trip first: the click raises it synchronously.
  await expect(page.locator(sel.SWITCH_OVERLAY)).toBeVisible();

  // When releasing the held response.
  releaseActivate();

  // Then the swap lands (the page root now names B) and the overlay
  // cleared with it — the swap re-seeding the signal is the one
  // clear path.
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionB,
  );
  await expect(page.locator(sel.SWITCH_OVERLAY)).toBeHidden();
  await expect(page).toHaveURL(
    new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}${sessionB}`),
  );
});

test("clicking the viewed session's row does nothing", async ({ page }) => {
  // Given a session page with a request counter on the activate
  // endpoint.
  await openSession(page);
  const viewedId = await page
    .locator(sel.PAGE_ROOT)
    .getAttribute(sel.PAGE_SESSION_ATTR);
  let activateCalls = 0;
  await page.route("**/activate", (route) => {
    activateCalls += 1;
    return route.continue();
  });

  // When clicking the row naming the viewed session.
  await page.locator(sel.SESSION_ROW_BY_ID(viewedId)).click();

  // Then nothing fired: no POST, no overlay, no navigation — the
  // viewed row is inert.
  await page.waitForTimeout(500);
  expect(activateCalls).toBe(0);
  await expect(page.locator(sel.SWITCH_OVERLAY)).toBeHidden();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    viewedId,
  );
});

test("every row activates from the landing page", async ({ page }) => {
  // Given the landing page — nothing is viewed there, so even the
  // active row must activate (this is how the tier's own navigation
  // reaches a session).
  await page.goto(`${baseUrl()}/web`);
  await expect(page.locator(sel.SESSION_ROW).first()).toBeVisible();
  const rowId = await page
    .locator(sel.SESSION_ROW)
    .first()
    .getAttribute(sel.SESSION_ID_ATTR);

  // When clicking the first row.
  await page.locator(sel.SESSION_ROW).first().click();

  // Then the page swaps to that row's session — the landing page's
  // rows keep today's behavior.
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    rowId,
  );
  await expect(page).toHaveURL(
    new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}${rowId}`),
  );
});

test("a session switch fetches no list and reopens the stream exactly once", async ({
  page,
}) => {
  // Given a page with a request counter on the sessions list pull and
  // the stream — the targeted-patch navigation law's observable
  // claim, byte-level: a switch carries signals plus the stream-span
  // rebind only, so ZERO list pulls fire (the sidebar's DOM survives
  // the navigation and recolors off the viewedSession patch) and
  // exactly ONE stream reopens (the old connection cancels with the
  // replaced span). A list pull per click is the rework's blank-flash
  // regression; a second stream is a leaked connection.
  await openSession(page);
  const sessionA = await page
    .locator(sel.PAGE_ROOT)
    .getAttribute(sel.PAGE_SESSION_ATTR);
  const sessionB = await createSession(page);
  await page.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionA}`);
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionA,
  );

  let settled = false;
  const counts = { list: 0, stream: 0 };
  const seen = { list: [], stream: [] };
  const count = (url) => {
    if (url.includes("/web/sessions?") || url.endsWith("/web/sessions")) {
      counts.list += 1;
      seen.list.push(url);
    }
    if (url.includes("/web/stream/")) {
      counts.stream += 1;
      seen.stream.push(url);
    }
  };
  await page.route("**/web/sessions*", (route) => {
    count(route.request().url());
    return route.continue();
  });
  await page.route("**/web/stream/*", (route) => {
    count(route.request().url());
    if (route.request().url().includes(sessionB)) settled = true;
    return route.continue();
  });

  // When switching to B through its row.
  await page.locator(sel.SESSION_ROW_BY_ID(sessionB)).click();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionB,
  );

  // Then the wire carried no list pull and exactly one stream open:
  // wait for B's stream to connect (the page's own convergence),
  // let any stragglers land, and assert the counts. (A modal-created
  // session has no history, so no catch-up completion patch ever
  // arrives — the stream open is the settle.)
  await expect.poll(() => settled, { timeout: 5_000 }).toBe(true);
  await page.waitForTimeout(1_000);
  expect(counts.list, "list pulls after the click").toBe(0);
  expect(counts.stream).toBe(1);
});

test.describe("phone drawer", () => {
  test.use({ viewport: PHONE });

  test("selecting a non-viewed row closes the drawer via the page swap", async ({
    page,
  }) => {
    // Given two sessions: A viewed, B created — then A re-opened by
    // URL (the created session sorts first, so openSession's
    // first-row click is not addressable here) — and the drawer open.
    // The boot session's page opens by URL: at phone width the rows
    // live in the off-canvas drawer, and openSession's row click
    // cannot reach them (the drawer's own spec documents this).
    await page.goto(`${baseUrl()}/web`);
    await expect(page.locator(sel.SESSION_ROW).first()).toBeAttached();
    const sessionA = await page
      .locator(sel.SESSION_ROW)
      .first()
      .getAttribute(sel.SESSION_ID_ATTR);
    await page.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionA}`);
    await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
      sel.PAGE_SESSION_ATTR,
      sessionA,
    );
    const sessionB = await createSession(page);
    await page.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionA}`);
    await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
      sel.PAGE_SESSION_ATTR,
      sessionA,
    );
    // The drawer opens from the menu's Sessions item — the real path
    // a finger performs (mobile.spec's openDrawer flow, condensed).
    await page.locator(sel.MENU_BUTTON).click();
    await page.locator(sel.MENU_ITEM_SESSIONS).click();
    await expect(page.locator(sel.PAGE_ROOT)).toHaveClass(
      new RegExp(sel.DRAWER_OPEN_CLASS),
    );
    await expect(page.locator(sel.DRAWER_SCRIM)).toBeVisible();

    // When selecting the non-viewed row (the activate response's
    // signal patch re-seeds drawerOpen false).
    await page.locator(sel.SESSION_ROW_BY_ID(sessionB)).click();
    await expect(page).toHaveURL(new RegExp(sel.SESSION_PAGE_PATH));
    await expect(page.locator(sel.PAGE_ROOT)).toBeVisible();

    // Then the surviving page root carries no drawer-open class and
    // the sidebar settles back off-canvas — the response's overlay
    // seed is the explicit reset. The off-canvas parking is a
    // `transition: transform .25s` (the drawer's slide), so the
    // matrix read is settled-state, not immediate: an instant read
    // catches the slide mid-flight at positive-x.
    await expect(page.locator(sel.PAGE_ROOT)).not.toHaveClass(
      new RegExp(sel.DRAWER_OPEN_CLASS),
    );
    await expect
      .poll(async () => {
        // The drawer parks off-canvas by transform; the computed
        // matrix reads negative-x (transform-based visibility is
        // Playwright-blind).
        return page.evaluate((id) => {
          const sidebar = document.getElementById(id);
          if (!sidebar) return false;
          const matrix = new DOMMatrix(getComputedStyle(sidebar).transform);
          return matrix.m41 < 0;
        }, "sidebar");
      })
      .toBe(true);
    await expect(page.locator(sel.DRAWER_SCRIM)).not.toBeVisible();
  });
});
