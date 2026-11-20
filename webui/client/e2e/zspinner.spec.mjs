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
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  const modal = page.locator(sel.SESSION_NEW_MODAL);
  await expect(modal).toBeVisible();
  await page.locator(sel.SESSION_NEW_SUBMIT).click();
  const root = page.locator(sel.PAGE_ROOT);
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
  await expect(page.locator(sel.MODEL_LABEL)).toHaveText(sel.MODEL_ID);
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
  await page.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionA}`);
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
  await openSession(pageB);
  await pageB.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionB}`);
  await expect(pageB.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionB,
  );
  // The new page needs its own stream connection before a pushed
  // spinner can reach it.
  await pageB.locator(sel.SESSION_ROW_BY_ID(sessionB)).waitFor();
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
