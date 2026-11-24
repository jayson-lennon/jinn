//! Library flow — the sidebar's Sessions|Library toggle, the kebab
//! popup, membership persistence, and cross-surface refresh.
//!
//! Every test boots its own jinn and creates its own session through
//! the new-session modal, so no test depends on rows another test's
//! membership moves may have hidden.
//!
//! Two mechanics shape the tests:
//!
//! - **The reload lands on Sessions** — the active view is a client
//!   signal, deliberately not persisted, so a test asserting library
//!   persistence re-clicks the Library segment after its reload.
//! - **Membership is server state** — the store actor persists the
//!   column before the fan-out announces the change, so every open
//!   page re-pulls both lists through the existing sessions refresh.

import {
  createSession,
  expect,
  openSession,
  test,
  toggleLibrary,
} from "./fixture.mjs";
import * as sel from "./selectors.mjs";

test("adding a member moves the row out of Sessions into Library", async ({
  page,
  jinn,
}) => {
  // Given the boot session viewed and a fresh session created through
  // the modal (this test owns its row; the boot session stays put).
  await openSession(jinn, page);
  const created = await createSession(jinn, page);
  const createdRow = page.locator(sel.ROW_BY_ID(created));

  // When toggling the created row into the library through its kebab.
  await toggleLibrary(page, created);

  // Then the row leaves the Sessions view and, under the Library
  // segment, the same row renders there.
  await expect(createdRow).toHaveCount(0);
  await page.locator(sel.VIEW_LIBRARY).click();
  await expect(page.locator(sel.ROW_BY_ID(created))).toBeVisible();
});

test("membership survives a page reload", async ({ page, jinn }) => {
  // Given a member added through the previous flow's mechanism on a
  // fresh session.
  await openSession(jinn, page);
  const created = await createSession(jinn, page);
  await toggleLibrary(page, created);

  // When the page reloads — the view lands back on Sessions (the
  // active view is a client signal, not server state).
  await page.reload();

  // Then the member is absent from the Sessions list; re-opening the
  // Library view shows it — the persisted column, not the page.
  await expect(page.locator(sel.ROW_BY_ID(created))).toHaveCount(0);
  await page.locator(sel.VIEW_LIBRARY).click();
  await expect(page.locator(sel.ROW_BY_ID(created))).toBeVisible();
});

test("a membership change on a second page refreshes the first without reload", async ({
  page,
  jinn,
}) => {
  // Given a fresh session, a page viewing it, and a second page open
  // on a real session (the second page must hold a stream — the
  // landing page renders no stream by design, so it cannot receive
  // the push; clicking a row navigates it and rebinds one).
  await openSession(jinn, page);
  const created = await createSession(jinn, page);
  const second = await page.context().newPage();
  await second.goto(`${jinn.baseUrl}/web`);
  await expect(second.locator(sel.PAGE_ROOT)).toBeVisible();
  await second.locator(sel.SESSION_ROW).first().click();
  await expect(second).toHaveURL(
    new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}`),
  );

  // When the first page toggles the row into the library.
  await toggleLibrary(page, created);

  // Then the second page's Sessions list drops the row without any
  // reload — the fan-out's sessions refresh riding the existing
  // membership event.
  await expect(
    second.locator(sel.ROW_BY_ID(created)),
  ).toHaveCount(0);
});

test("the library filter narrows the flat member list", async ({ page, jinn }) => {
  // Given a member whose title is unique.
  await openSession(jinn, page);
  const created = await createSession(jinn, page);
  await toggleLibrary(page, created);
  await page.locator(sel.VIEW_LIBRARY).click();
  await expect(page.locator(sel.ROW_BY_ID(created))).toBeVisible();

  // When filtering for a fragment of the member's title (read from
  // the row — the minted title's exact text is not the contract).
  const title = await page
    .locator(`${sel.ROW_BY_ID(created)} .stitle`)
    .innerText();
  const fragment = title.slice(0, 6);
  await page.locator(sel.SESSIONS_FILTER).fill(fragment);

  // Then the member stays visible under the matching fragment.
  await expect(page.locator(sel.ROW_BY_ID(created))).toBeVisible();

  // And a non-matching filter empties the list.
  await page.locator(sel.SESSIONS_FILTER).fill("zzz-no-match");
  await expect(page.locator(sel.LIBRARY_ROW)).toHaveCount(0);
});

test("a non-member row's kebab offers add; a member's offers remove", async ({
  page,
  jinn,
}) => {
  // Given a fresh session row in the Sessions view.
  await openSession(jinn, page);
  const created = await createSession(jinn, page);

  // When opening its kebab.
  await page.locator(sel.ROW_MENU_IN(created)).click();

  // Then the single entry offers the add.
  await expect(
    page.locator(sel.MENU_ENTRY(sel.ACTION_LIBRARY_TOGGLE)).first(),
  ).toHaveText("add to library");

  // When toggled in and the kebab reopened (after the row moved to
  // Library, on the Library view).
  await page
    .locator(sel.MENU_ENTRY(sel.ACTION_LIBRARY_TOGGLE))
    .first()
    .click();
  await page.locator(sel.VIEW_LIBRARY).click();
  await expect(page.locator(sel.ROW_BY_ID(created))).toBeVisible();
  await page.locator(sel.ROW_MENU_IN(created)).click();

  // Then the single entry offers the remove.
  await expect(
    page.locator(sel.MENU_ENTRY(sel.ACTION_LIBRARY_TOGGLE)).first(),
  ).toHaveText("remove from library");
});

test("activating an archived member opens its page through the ordinary route", async ({
  page,
  jinn,
}) => {
  // Given a member added through the kebab, whose store row persists
  // after the run ends only as long as the scratch db lives — this
  // spec asserts the live member's click path, which is identical
  // for an archived one (the same activate route renders the same
  // row shape; the archive half is pinned by the serve-tier tests in
  // serve_tests.rs (`an_archived_member_lists_in_the_library_view_and_activates`,
  // real SQLite store, real activate POST) and the store actor test
  // `an_archived_library_member_loads_and_unarchives`).
  await openSession(jinn, page);
  const created = await createSession(jinn, page);
  await toggleLibrary(page, created);
  await page.locator(sel.VIEW_LIBRARY).click();
  await expect(page.locator(sel.ROW_BY_ID(created))).toBeVisible();

  // When clicking the member row in the Library view.
  await page.locator(sel.ROW_BY_ID(created)).click();

  // Then the page navigates by the ordinary session URL — no
  // library-specific route exists anywhere in the flow.
  await expect(page).toHaveURL(
    new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}${created}`),
  );
});

// ── Mobile: the popup's stacking slot and tap targets ───────────────
//
// Per-file viewport override, the mobile.spec.mjs pattern: this block
// re-overrides to phone width. The geometry claims are computed-style
// and bounding-box reads at phone width — the only tier that can see
// a real layout engine.
test.describe("mobile popup rules", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  // At phone width the sidebar is an off-canvas drawer (the
  // mobile.spec.mjs mechanics hold here too: a closed drawer's rows
  // cannot be clicked — Playwright cannot hit-test what a finger
  // could not reach). Every kebab interaction below opens the drawer
  // first, the real path a finger performs.
  test.beforeEach(async ({ page, jinn }) => {
    await page.goto(`${jinn.baseUrl}/web`);
    await page.locator(sel.SESSIONS_BUTTON).click();
    await expect(page.locator(sel.PAGE_ROOT)).toHaveClass(
      new RegExp(sel.DRAWER_OPEN_CLASS),
    );
  });

  test("the popup stacks above the drawer and below the switch overlay", async ({
    page,
    jinn,
  }) => {
    // Given the open drawer at phone width with one row's kebab open.
    const row = page.locator(sel.SESSION_ROW).first();
    await expect(row).toBeAttached();
    await page.locator(sel.ROW_MENU).first().click();
    const popup = page.locator(sel.ROW_MENU_POPUP).first();
    await expect(popup).toBeVisible();

    // Then the popup's stacking slot sits above the drawer (40) and
    // below the switch overlay (80).
    const layers = await page.evaluate(() => {
      const z = (el) => Number(getComputedStyle(el).zIndex);
      return {
        drawer: z(document.getElementById("sidebar")),
        popup: z(document.querySelector(".srow-menu-popup")),
        overlay: z(document.getElementById("switch-overlay")),
      };
    });
    expect(layers.drawer).toBe(40);
    expect(layers.popup).toBeGreaterThan(layers.drawer);
    expect(layers.overlay).toBe(80);
    expect(layers.popup).toBeLessThan(layers.overlay);
  });

  test("the kebab and popup entries have tap targets of at least 32px", async ({
    page,
  }) => {
    // Given the open drawer at phone width with one row's kebab open.
    const row = page.locator(sel.SESSION_ROW).first();
    await expect(row).toBeAttached();
    await page.locator(sel.ROW_MENU).first().click();
    const popup = page.locator(sel.ROW_MENU_POPUP).first();
    await expect(popup).toBeVisible();

    // Then the kebab and each popup entry measure ≥32px on both axes.
    const sizes = await page.evaluate(() => {
      const box = (el) => {
        const r = el.getBoundingClientRect();
        return { w: r.width, h: r.height };
      };
      const kebab = document.querySelector(".srow-menu");
      const entries = [...document.querySelectorAll(".srow-menu-popup button")];
      return [box(kebab), ...entries.map(box)];
    });
    for (const { w, h } of sizes) {
      expect(w).toBeGreaterThanOrEqual(32);
      expect(h).toBeGreaterThanOrEqual(32);
    }
  });

  test("an outside tap closes the popup without closing the drawer or activating the row", async ({
    page,
  }) => {
    // Given the open drawer at phone width with one row's kebab open.
    const row = page.locator(sel.SESSION_ROW).first();
    await expect(row).toBeAttached();
    await page.locator(sel.ROW_MENU).first().click();
    const popup = page.locator(sel.ROW_MENU_POPUP).first();
    await expect(popup).toBeVisible();

    // When tapping outside the popup (the page body, not a control).
    await page.mouse.click(10, 400);
    await expect(popup).toBeHidden();

    // Then the tap stopped at the popup's closer: the drawer is still
    // open (the scrim received nothing), and the row did not activate
    // (the URL never navigated off the landing page — a fallen-through
    // tap on a row would).
    await expect(page.locator(sel.PAGE_ROOT)).toHaveClass(
      new RegExp(sel.DRAWER_OPEN_CLASS),
    );
    await expect(page).not.toHaveURL(
      new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}`),
    );
  });
});
