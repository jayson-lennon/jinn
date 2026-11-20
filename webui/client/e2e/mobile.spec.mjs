//! Mobile flow — the drawer at a phone viewport, plus the desktop
//! no-op half.
//!
//! Per-file viewport override (the config pins 1280×800 for the
//! desktop specs; this file's `test.use` names 390×844 for its own
//! tests, and one describe block re-overrides back to 1280×800 for
//! the desktop assertions). One test per behavior, mirroring the Rust
//! suite's discipline: a failure names the one thing that broke.
//!
//! Two phone-width realities shape the mechanics here:
//!
//! - **Off-canvas ≠ invisible to Playwright.** The drawer hides by
//!   `transform: translateX(-100%)`, and Playwright's visibility
//!   check is bounding-box based (transform-blind) — an off-canvas
//!   sidebar still reports "visible". The assertions below therefore
//!   read the computed transform (negative x) and the drawer-open
//!   class, the hooks the drawer actually hangs on.
//! - **A closed drawer's rows cannot be clicked.** The session rows
//!   live inside the off-canvas sidebar; Playwright cannot hit-test
//!   what a finger could not reach either. Tests that need the
//!   session page navigate to it by URL (the row's id attribute reads
//!   fine on hidden elements); the one row-click test opens the
//!   drawer first — the real path a finger performs.
//!
//! The geometry claims (End States 5–7) are computed-style and
//! bounding-box reads at phone width — the only tier that can see a
//! real layout engine. The menu itself is a slide-in panel from the
//! left edge over its own scrim at every width (Addendum 1), so its
//! assertions read the same transform/scrim hooks the drawer's do.

import { expect, test } from "@playwright/test";
import * as sel from "./selectors.mjs";
import { baseUrl } from "./run.mjs";

const PHONE = { width: 390, height: 844 };
const NARROW = { width: 360, height: 740 };
const DESKTOP = { width: 1280, height: 800 };

test.use({ viewport: PHONE });

/**
 * Opens the boot session's page at the current viewport without
 * clicking through the drawer: reads the row's session id from the
 * landing page (an attribute read works on the off-canvas row) and
 * navigates to its page URL.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 */
async function openBootedSession(page) {
  await page.goto(`${baseUrl()}/web`);
  const row = page.locator(sel.SESSION_ROW).first();
  await expect(row).toBeAttached();
  const sessionId = await row.getAttribute(sel.SESSION_ID_ATTR);
  if (!sessionId) {
    throw new Error("the boot session row carries no session id");
  }
  await page.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionId}`);
  await expect(page.locator(sel.PAGE_ROOT)).toBeVisible();
}

/**
 * Asserts the sidebar is currently off-canvas: parked left of the
 * viewport by the drawer transform (a negative x on the computed
 * matrix — the hook `translateX(-100%)` produces).
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves when the transform reads negative.
 */
async function expectSidebarOffCanvas(page) {
  await expect
    .poll(() =>
      page.evaluate(() => {
        const sidebar = document.getElementById("sidebar");
        if (!sidebar) return 0;
        const matrix = new DOMMatrix(getComputedStyle(sidebar).transform);
        return matrix.e;
      }),
    )
    .toBeLessThan(0);
}

/**
 * Asserts the sidebar is currently on-canvas: no drawer transform
 * (the matrix x is 0).
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves when the transform reads zero.
 */
async function expectSidebarOnCanvas(page) {
  await expect
    .poll(() =>
      page.evaluate(() => {
        const sidebar = document.getElementById("sidebar");
        if (!sidebar) return -1;
        const matrix = new DOMMatrix(getComputedStyle(sidebar).transform);
        return matrix.e;
      }),
    )
    .toBe(0);
}

/**
 * Asserts the menu panel is currently parked off-canvas (a negative
 * transform x — the closed slide-in state).
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves when the transform reads negative.
 */
async function expectMenuPanelOffCanvas(page) {
  await expect
    .poll(() =>
      page.evaluate(() => {
        const panel = document.getElementById("menu-panel");
        if (!panel) return 0;
        return new DOMMatrix(getComputedStyle(panel).transform).e;
      }),
    )
    .toBeLessThan(0);
}

/**
 * Asserts the menu panel is currently on-canvas (transform x 0 —
 * the slid-in state).
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves when the transform reads zero.
 */
async function expectMenuPanelOnCanvas(page) {
  await expect
    .poll(() =>
      page.evaluate(() => {
        const panel = document.getElementById("menu-panel");
        if (!panel) return -1;
        return new DOMMatrix(getComputedStyle(panel).transform).e;
      }),
    )
    .toBe(0);
}

/**
 * Opens the menu panel via the hamburger and settles its slide-in.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 */
async function openMenuPanel(page) {
  await page.locator(sel.MENU_BUTTON).click();
  await expectMenuPanelOnCanvas(page);
  await expect(page.locator(sel.MENU_SCRIM)).toBeVisible();
}

/**
 * Opens the drawer via the real path: hamburger, then the Sessions
 * item. Every drawer test goes through the UI, never straight to the
 * class — the spec tests the flow a finger performs.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 */
async function openDrawer(page) {
  await openMenuPanel(page);
  await page.locator(sel.MENU_ITEM_SESSIONS).click();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveClass(
    new RegExp(sel.DRAWER_OPEN_CLASS),
  );
  await expect(page.locator(sel.DRAWER_SCRIM)).toBeVisible();
  await expectSidebarOnCanvas(page);
}

// ── The drawer at a phone viewport ──────────────────────────────────

test("a fresh page shows no sidebar below 768px", async ({ page }) => {
  // Given a freshly booted scratch jinn at 390×844.
  // When navigating to the page root (no drawer-open class exists).
  await page.goto(`${baseUrl()}/web`);
  await expect(page.locator(sel.PAGE_ROOT)).toBeVisible();

  // Then the sidebar is off-canvas: the drawer transform parks it
  // left of the viewport, and the page carries no open state.
  await expectSidebarOffCanvas(page);
  await expect(page.locator(sel.PAGE_ROOT)).not.toHaveClass(
    new RegExp(sel.DRAWER_OPEN_CLASS),
  );
});

test("the hamburger is visible at a phone viewport", async ({ page }) => {
  // Given the session page at 390×844.
  await openBootedSession(page);

  // Then the hamburger renders — the menu is visible at every width.
  await expect(page.locator(sel.MENU_BUTTON)).toBeVisible();
});

test("the hamburger slides in the panel with its scrim and one Sessions item", async ({
  page,
}) => {
  // Given the session page at 390×844.
  await openBootedSession(page);

  // When tapping the hamburger.
  await page.locator(sel.MENU_BUTTON).click();

  // Then the panel slid in from the left over its scrim, carrying
  // exactly three items — new-session, persona (a session is
  // viewed), sessions — persona between the other two.
  await expectMenuPanelOnCanvas(page);
  await expect(page.locator(sel.MENU_SCRIM)).toBeVisible();
  const items = page.locator(`${sel.MENU_PANEL} button`);
  await expect(items).toHaveCount(3);
  await expect(items.nth(0)).toHaveText("New session");
  await expect(items.nth(1)).toHaveText("Persona");
  await expect(items.nth(2)).toHaveText("Sessions");
});

test("the Sessions item closes the menu and opens the drawer", async ({
  page,
}) => {
  // Given the session page with the menu open.
  await openBootedSession(page);
  await openMenuPanel(page);

  // When activating the Sessions item.
  await page.locator(sel.MENU_ITEM_SESSIONS).click();

  // Then the menu closed (panel parked off-canvas, its scrim gone),
  // the drawer-open class landed on the page root, the sidebar slid
  // in, and the drawer scrim shows.
  await expectMenuPanelOffCanvas(page);
  await expect(page.locator(sel.MENU_SCRIM)).not.toBeVisible();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveClass(
    new RegExp(sel.DRAWER_OPEN_CLASS),
  );
  await expectSidebarOnCanvas(page);
  await expect(page.locator(sel.DRAWER_SCRIM)).toBeVisible();
});

test("a scrim tap closes the drawer", async ({ page }) => {
  // Given the session page with the drawer open. The drawer covers
  // the viewport's left side; the exposed strip is the tap target a
  // finger would actually use.
  await openBootedSession(page);
  await openDrawer(page);
  const scrim = page.locator(sel.DRAWER_SCRIM);

  // When tapping the scrim on its exposed right strip.
  const box = await scrim.boundingBox();
  if (!box) {
    throw new Error("the scrim has no box while the drawer is open");
  }
  await page.mouse.click(box.x + box.width - 20, box.y + box.height / 2);

  // Then the drawer closed — class gone, sidebar off-canvas, scrim
  // hidden.
  await expect(page.locator(sel.PAGE_ROOT)).not.toHaveClass(
    new RegExp(sel.DRAWER_OPEN_CLASS),
  );
  await expectSidebarOffCanvas(page);
  await expect(scrim).not.toBeVisible();
});

test("Escape with focus on body closes the drawer", async ({ page }) => {
  // Given the session page with the drawer open and focus parked on
  // body (no element focused — the case the selector dispatch cannot
  // see).
  await openBootedSession(page);
  await openDrawer(page);
  await page.evaluate(() => document.activeElement?.blur());
  await expect
    .poll(() => page.evaluate(() => document.activeElement?.tagName ?? "BODY"))
    .toBe("BODY");

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the drawer closed — the document-level dismissal fired at
  // body focus.
  await expect(page.locator(sel.PAGE_ROOT)).not.toHaveClass(
    new RegExp(sel.DRAWER_OPEN_CLASS),
  );
  await expectSidebarOffCanvas(page);
  await expect(page.locator(sel.DRAWER_SCRIM)).not.toBeVisible();
});

test("Escape closes an open menu when the drawer is closed", async ({
  page,
}) => {
  // Given the session page with only the menu open.
  await openBootedSession(page);
  await openMenuPanel(page);
  await page.evaluate(() => document.activeElement?.blur());

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the menu closed (panel parked, scrim gone) and no drawer
  // state appeared.
  await expectMenuPanelOffCanvas(page);
  await expect(page.locator(sel.MENU_SCRIM)).not.toBeVisible();
  await expect(page.locator(sel.PAGE_ROOT)).not.toHaveClass(
    new RegExp(sel.DRAWER_OPEN_CLASS),
  );
  await expectSidebarOffCanvas(page);
});

test("selecting a session row closes the drawer via the page swap", async ({
  page,
}) => {
  // Given the session page with the drawer open — the only state a
  // phone user can click a row from.
  await openBootedSession(page);
  await openDrawer(page);

  // When selecting a session row (the activate page swap replaces the
  // whole root).
  await page.locator(sel.SESSION_ROW).first().click();
  await expect(page).toHaveURL(new RegExp(sel.SESSION_PAGE_PATH));
  await expect(page.locator(sel.PAGE_ROOT)).toBeVisible();

  // Then the fresh page root carries no drawer-open class and the
  // sidebar is back off-canvas — the swap is the structural reset.
  await expect(page.locator(sel.PAGE_ROOT)).not.toHaveClass(
    new RegExp(sel.DRAWER_OPEN_CLASS),
  );
  await expectSidebarOffCanvas(page);
  await expect(page.locator(sel.DRAWER_SCRIM)).not.toBeVisible();
});

// ── Geometry claims (End States 5–7) at phone widths ────────────────

test.describe("phone geometry", () => {
  test.use({ viewport: NARROW });

  test("a 360px viewport has no horizontal overflow", async ({ page }) => {
    // Given the session page at 360×740.
    await openBootedSession(page);

    // Then the document never scrolls horizontally — every titlebar
    // element shrinks or ellipsizes instead of pushing the page wide.
    const scrollWidth = await page.evaluate(
      () => document.documentElement.scrollWidth,
    );
    if (scrollWidth > NARROW.width) {
      throw new Error(
        `horizontal overflow at 360px: scrollWidth=${scrollWidth}`,
      );
    }
  });
});

test("the session title ellipsizes instead of overflowing", async ({
  page,
}) => {
  // Given the session page at 390×844.
  await openBootedSession(page);
  const title = page.locator("#titlebar .t");

  // Then the title carries the ellipsis chain and stays inside the
  // viewport.
  const styles = await title.evaluate((element) => {
    const computed = getComputedStyle(element);
    return {
      overflow: computed.overflow,
      textOverflow: computed.textOverflow,
      whiteSpace: computed.whiteSpace,
      box: element.getBoundingClientRect(),
    };
  });
  expect(styles.overflow).toBe("hidden");
  expect(styles.textOverflow).toBe("ellipsis");
  expect(styles.whiteSpace).toBe("nowrap");
  if (styles.box.right > PHONE.width) {
    throw new Error(
      `the title overflows the viewport: right=${styles.box.right}`,
    );
  }
});

test("the page root's height tracks the viewport (the dvh application)", async ({
  page,
}) => {
  // Given the session page at 390×844.
  await openBootedSession(page);

  // Then the page root's computed height is the viewport height —
  // less than a pixel of drift — proving the dvh declaration applied.
  const height = await page.evaluate(
    () => document.getElementById("page-root")?.getBoundingClientRect().height ?? 0,
  );
  if (Math.abs(height - PHONE.height) > 1) {
    throw new Error(
      `the page root is not viewport-height: ${height} vs ${PHONE.height}`,
    );
  }
});

test("text inputs render at 16px at a phone viewport", async ({ page }) => {
  // Given the session page at 390×844 (the iOS focus-zoom guard).
  await openBootedSession(page);

  // Then the composer textarea and the sessions filter both compute
  // 16px.
  const sizes = await page.evaluate(() => {
    const read = (selector) => {
      const element = document.querySelector(selector);
      return element ? getComputedStyle(element).fontSize : "absent";
    };
    return { textarea: read("#input"), filter: read("#sessions-filter") };
  });
  expect(sizes.textarea).toBe("16px");
  expect(sizes.filter).toBe("16px");
});

test("interactive controls reach the 40px hit height at a phone viewport", async ({
  page,
}) => {
  // Given the session page at 390×844 with the menu open (the item is
  // a menu-panel child — it measures only while the panel is slid in;
  // the drawer stays closed).
  await openBootedSession(page);
  await openMenuPanel(page);

  // When measuring the row, the send button, the hamburger, and the
  // menu item.
  const heights = await page.evaluate(() => {
    const read = (selector) => {
      const element = document.querySelector(selector);
      if (!element) return -1;
      return element.getBoundingClientRect().height;
    };
    return {
      row: read(".srow"),
      send: read('[data-webui-action="send"]'),
      menuButton: read("#menu-button"),
      menuItem: read("#menu-item-sessions"),
    };
  });

  // Then every one of them clears 40px — the touch-polish floor.
  for (const [name, height] of Object.entries(heights)) {
    if (height < 40) {
      throw new Error(`${name} hit height ${height}px is below 40px`);
    }
  }
});

// ── The desktop half (≥768px: sidebar static, menu still slides) ────

test.describe("desktop no-op", () => {
  test.use({ viewport: DESKTOP });

  test("the sidebar stays in flow and the panel slides over it", async ({
    page,
  }) => {
    // Given the session page at 1280×800.
    await openBootedSession(page);

    // Then the sidebar is statically visible (in flow, not fixed) and
    // the hamburger is present.
    const sidebar = page.locator("#sidebar");
    await expect(sidebar).toBeVisible();
    const position = await sidebar.evaluate((element) =>
      getComputedStyle(element).position,
    );
    expect(position).toBe("static");
    await expect(page.locator(sel.MENU_BUTTON)).toBeVisible();

    // When opening the menu.
    await openMenuPanel(page);

    // Then the panel slid in OVER the permanent sidebar (the menu is
    // the entry surface): fixed, stacked above it, scrim showing, and
    // the sidebar itself never moved.
    const panelState = await page.evaluate(() => {
      const panel = document.getElementById("menu-panel");
      const scrim = document.getElementById("menu-scrim");
      const sidebarElement = document.getElementById("sidebar");
      if (!panel || !scrim || !sidebarElement) return null;
      const panelComputed = getComputedStyle(panel);
      const sidebarComputed = getComputedStyle(sidebarElement);
      const sidebarTransform = sidebarComputed.transform;
      // A static sidebar has `z-index: auto` — it paints below any
      // positioned element; read `auto` as 0 for the ordering claim.
      const sidebarZ = sidebarComputed.zIndex === "auto"
        ? 0
        : Number.parseInt(sidebarComputed.zIndex, 10);
      return {
        panelPosition: panelComputed.position,
        panelTransformX: new DOMMatrix(panelComputed.transform).e,
        panelZ: Number.parseInt(panelComputed.zIndex, 10),
        sidebarZ,
        scrimHidden: scrim.hidden,
        sidebarTransform,
      };
    });
    if (panelState === null) {
      throw new Error("the panel/scrim/sidebar triad is missing");
    }
    expect(panelState.panelPosition).toBe("fixed");
    expect(panelState.panelTransformX).toBe(0);
    expect(panelState.scrimHidden).toBe(false);
    expect(panelState.panelZ).toBeGreaterThan(panelState.sidebarZ);
    expect(panelState.sidebarTransform).toBe("none");
    await expect(sidebar).toBeVisible();

    // When activating Sessions.
    await page.locator(sel.MENU_ITEM_SESSIONS).click();

    // Then the panel closed and NO drawer appeared — the class landed
    // but the ≥768px media query gives it nothing to move.
    await expectMenuPanelOffCanvas(page);
    await expect(page.locator(sel.MENU_SCRIM)).not.toBeVisible();
    await expect(page.locator(sel.PAGE_ROOT)).toHaveClass(
      new RegExp(sel.DRAWER_OPEN_CLASS),
    );
    await expect(page.locator(sel.DRAWER_SCRIM)).not.toBeVisible();
    await expect(sidebar).toBeVisible();
    const after = await sidebar.evaluate((element) => {
      const computed = getComputedStyle(element);
      return { position: computed.position, transform: computed.transform };
    });
    expect(after.position).toBe("static");
    expect(after.transform).toBe("none");
  });
});
