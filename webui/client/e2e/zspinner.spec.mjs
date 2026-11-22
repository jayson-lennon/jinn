//! Working spinner — the sessions list marks which sessions are busy.
//!
//! Sorts after `znew-session.spec.mjs` on purpose: it creates another
//! session through the modal and needs the multi-session world that
//! spec establishes. Rows are addressed by `data-session-id` — the
//! scratch db accumulates sessions across the alphabetical run.
//!
//! The sample backend streams `!response` word-by-word (10–50 ms per
//! token, ~1 s total), so the busy window is real but short: every
//! busy assertion polls (`toBeVisible` / `toHide`), never sleeps.

import { expect, test } from "@playwright/test";
import * as sel from "./selectors.mjs";
import { openSession } from "./nav.mjs";
import { baseUrl } from "./run.mjs";

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
  // returns, or the caller reads the pre-create session.
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

/**
 * Selects the scratch tier's one model for the viewed session — a
 * modal-created session boots with no selection, and a send without
 * one errors instantly instead of going busy.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves once the switch landed.
 */
async function selectModel(page) {
  await page.locator(sel.PICKER_BUTTON).click();
  await expect(page.locator(sel.PICKER_MODAL)).toBeVisible();
  await page.locator(sel.PICKER_ROW_MODEL_SPAN(sel.MODEL_ID)).click();
  await expect(page.locator(sel.PICKER_BUTTON)).toHaveText(sel.MODEL_ID);
}

/**
 * Sends `!response` — the delayed stream — through the composer of
 * the page's current session, returning once the turn is in flight
 * (the cancel button present). The caller's page is ON that session.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves when the turn is busy.
 */
async function startBusyTurn(page) {
  await selectModel(page);
  await page.locator(sel.INPUT).fill("!response");
  await page.locator(sel.SEND_BUTTON).click();
  await expect(page.locator(sel.CANCEL_BUTTON)).toBeVisible();
}

test("a turn in another session spins its row on this page", async ({
  page,
}) => {
  // Given two sessions: A (the boot session, viewed) and B (created
  // through the modal, then left by re-opening A).
  await openSession(page);
  const sessionA = await page
    .locator(sel.PAGE_ROOT)
    .getAttribute(sel.PAGE_SESSION_ATTR);
  const sessionB = await createSession(page);
  // Re-open A by ROW CLICK, not by URL: a page render views without
  // activating, and the settle's unread mark only lands on a session
  // that is not the map-active one — A must be ACTIVE when B settles.
  await page.locator(sel.SESSION_ROW_BY_ID(sessionA)).click();
  const root = page.locator(sel.PAGE_ROOT);
  await expect(root).toBeVisible();
  await expect(root).toHaveAttribute(sel.PAGE_SESSION_ATTR, sessionA);
  // And neither row spins while both sit idle.
  const rowB = page.locator(sel.SESSION_ROW_BY_ID(sessionB));
  await expect(rowB).toBeVisible();
  await expect(rowB).not.toHaveClass(/working/);

  // When B's turn starts — driven from THIS page's browser context is
  // impossible while viewing A, so the drive comes from the second
  // page below; here we open B's page in a fresh tab, start the turn,
  // and come back to A's page without touching it again.
  const pageB = await page.context().newPage();
  // B's tab opens by URL, never by openSession: openSession clicks a
  // row, ACTIVATING whichever session sorts first and silently
  // flipping the map's active pointer while A is supposed to stay the
  // active one. A URL render views B without touching activation.
  await pageB.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionB}`);
  await expect(pageB.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionB,
  );
  await startBusyTurn(pageB);

  // Then B's row spins on A's page — without any reload of A — and
  // B's own page shows its row spinning too.
  await expect(page.locator(sel.SESSION_ROW_BY_ID(sessionB))).toHaveClass(
    /working/,
  );
  await expect(
    page.locator(`${sel.SESSION_ROW_BY_ID(sessionB)} .working`),
  ).toBeVisible();
  await expect(pageB.locator(sel.SESSION_ROW_BY_ID(sessionB))).toHaveClass(
    /working/,
  );

  // And the spin clears on both pages when the turn settles naturally
  // (~1 s of stream), B's row marked unread on A's page (the settle
  // landed while A was viewed) and clean-but-idle on B's own page.
  await expect(page.locator(sel.SESSION_ROW_BY_ID(sessionB))).not.toHaveClass(
    /working/,
    { timeout: 10_000 },
  );
  await expect(pageB.locator(sel.SESSION_ROW_BY_ID(sessionB))).not.toHaveClass(
    /working/,
    { timeout: 10_000 },
  );
  await expect(page.locator(sel.SESSION_ROW_BY_ID(sessionB))).toHaveClass(
    /unread/,
  );
  await pageB.close();
});

test.describe("the sessions button's attention lifecycle", () => {
  // Phone viewport: the drawer steps need it — the sessions button is
  // the drawer's opener below 768px and hidden above it.
  test.use({ viewport: { width: 390, height: 844 } });

  test("the settle turns the sessions button's attention on and activation clears it", async ({
    page,
  }) => {
  // Given two sessions: A (the boot session) and B (created through
  // the modal); and neither marks attention while both sit idle — the
  // button is neutral, the badge hidden. At phone width the rows live
  // in the off-canvas drawer, so setup opens A by URL, creates B
  // through the menu, then ACTIVATES A through the drawer (the mark
  // keys off the map's active pointer, which a URL render never
  // moves).
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
  await page.locator(sel.SESSIONS_BUTTON).click();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveClass(
    new RegExp(sel.DRAWER_OPEN_CLASS),
  );
  await page.locator(sel.SESSION_ROW_BY_ID(sessionA)).click();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionA,
  );
  const button = page.locator(sel.SESSIONS_BUTTON);
  await expect(button).not.toHaveClass(
    new RegExp(sel.SESSIONS_ATTENTION_CLASS),
  );
  await expect(page.locator(sel.SESSIONS_BUTTON_COUNT)).toBeHidden();

  // When B runs a turn in its own tab and settles (the settle lands
  // while A is viewed — the unseen-settle mark).
  const pageB = await page.context().newPage();
  // B's tab opens by URL (see test 1's note): openSession would
  // activate whichever row sorts first, flipping the active pointer
  // the unread mark keys off.
  await pageB.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionB}`);
  await expect(pageB.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionB,
  );
  await startBusyTurn(pageB);
  await expect(page.locator(sel.SESSION_ROW_BY_ID(sessionB))).toHaveClass(
    /unread/,
    { timeout: 10_000 },
  );

  // Then A's button wears the attention class and the badge reads 1 —
  // pushed live, no reload of A.
  await expect(button).toHaveClass(new RegExp(sel.SESSIONS_ATTENTION_CLASS));
  await expect(page.locator(sel.SESSIONS_BUTTON_COUNT)).toHaveText("1");

  // And when A activates B (the row click through the drawer — the
  // real path a finger performs), the mark clears and the button
  // goes neutral with the badge hidden again.
  await page.locator(sel.SESSIONS_BUTTON).click();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveClass(
    new RegExp(sel.DRAWER_OPEN_CLASS),
  );
  await page.locator(sel.SESSION_ROW_BY_ID(sessionB)).click();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionB,
  );
  await expect(page.locator(sel.SESSIONS_BUTTON)).not.toHaveClass(
    new RegExp(sel.SESSIONS_ATTENTION_CLASS),
  );
    await expect(page.locator(sel.SESSIONS_BUTTON_COUNT)).toBeHidden();
    await pageB.close();
  });
});

test("cancelling the turn clears the row's spinner", async ({ page }) => {
  // Given a session created through the modal and viewing it.
  await openSession(page);
  const sessionId = await createSession(page);
  await page.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionId}`);
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionId,
  );

  // When a turn starts and is cancelled mid-stream.
  await startBusyTurn(page);
  const row = page.locator(sel.SESSION_ROW_BY_ID(sessionId));
  await expect(row).toHaveClass(/working/);
  await page.locator(sel.CANCEL_BUTTON).click();

  // Then the spinner clears — the cancel drain settles the phase and
  // the flip fans the list.
  await expect(row).not.toHaveClass(/working/, { timeout: 10_000 });
  // And the page is watchable again: the send control returned.
  await expect(page.locator(sel.SEND_BUTTON)).toBeVisible();
});
