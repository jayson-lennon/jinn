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
  const before = page.url();
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  const modal = page.locator(sel.SESSION_NEW_MODAL);
  await expect(modal).toBeVisible();
  await page.locator(sel.SESSION_NEW_SUBMIT).click();
  const root = page.locator(sel.PAGE_ROOT);
  // The swap signal: the response closes the menu (a signal patch)
  // before it replaces #page-root, so menu-hidden alone can race the
  // swap. The URL pushState happens with the swap — wait for it.
  await expect(page).not.toHaveURL(before);
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
 * Reads the page root's rendered `sessionsAttention` seed — the
 * server-rendered signal value in the `data-signals` attribute, which
 * a page-root swap refreshes atomically with the swap itself.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<number>} the rendered attention count.
 */
async function seedAttentionOf(page) {
  const signals = await page.locator(sel.PAGE_ROOT).getAttribute("data-signals");
  const match = signals?.match(/sessionsAttention:\s*(\d+)/);
  if (!match) {
    throw new Error("page root carries no sessionsAttention seed");
  }
  return Number.parseInt(match[1], 10);
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
  // through the modal, then left by re-opening A via the row click —
  // creation activated B, and a page GET activates nothing, so A must
  // be re-activated explicitly for B's settle to earn the mark).
  await openSession(page);
  const sessionA = await page
    .locator(sel.PAGE_ROOT)
    .getAttribute(sel.PAGE_SESSION_ATTR);
  const sessionB = await createSession(page);
  // Creation activated B instance-wide; re-activate A deterministically
  // by the row click, and WAIT for the swap's pushState — the click
  // dispatches an async activate POST, and a page load racing it can
  // abort the request before the activation lands.
  await page.locator(sel.SESSION_ROW_BY_ID(sessionA)).click();
  await expect(page).toHaveURL(
    new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}${sessionA}`),
  );
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
  // page below; there B's page opens by a plain load (openSession's
  // row click would activate whichever session sorts first and race
  // the mark), the turn runs, and A's page is not touched again.
  const pageB = await page.context().newPage();
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

test("the settle bumps the sessions button's count and activation decrements it", async ({
  page,
}) => {
  // Given a phone viewport — the button is a phone affordance, hidden
  // at desktop widths — and two sessions: A (the boot session,
  // viewed) and B (created through the modal, then left by re-opening
  // A). The scratch db accumulates sessions from earlier specs, so
  // the seeded count may already be non-zero — the assertions below
  // are deltas on the seeded value, never absolutes.
  await page.setViewportSize({ width: 390, height: 844 });
  // Phone-native open: at this width the sidebar is the off-canvas
  // drawer, so the row is reached through the sessions button — the
  // one-tap path this feature exists for — and the drawer's row click
  // activates A.
  await page.goto(`${baseUrl()}/web`);
  await page.locator(sel.SESSIONS_BUTTON).click();
  const rowA0 = page.locator(sel.SESSION_ROW).first();
  await expect(rowA0).toBeVisible();
  await rowA0.click();
  await expect(page).toHaveURL(
    new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}`),
  );
  const root0 = page.locator(sel.PAGE_ROOT);
  await expect(root0).toBeVisible();
  const sessionA = await root0.getAttribute(sel.PAGE_SESSION_ATTR);
  if (!sessionA) {
    throw new Error("page root carries no session id after activating a session");
  }
  const sessionB = await createSession(page);
  // Creation activated B instance-wide; page A's GET below is only a
  // page load, not an activation. Re-activate A deterministically by
  // the drawer's row click — the same path a finger uses — so B's
  // settle lands while A is the active session and B earns the
  // unseen-settle mark.
  await page.locator(sel.SESSIONS_BUTTON).click();
  await page.locator(sel.SESSION_ROW_BY_ID(sessionA)).click();
  await expect(page).toHaveURL(
    new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}${sessionA}`),
  );
  const button = page.locator(sel.SESSIONS_BUTTON);
  const badge = page.locator(sel.SESSIONS_BUTTON_COUNT);
  // The seed comes from the page-root's rendered signals, not the
  // badge's live text: a page-root swap (this row click was one)
  // re-seeds the signal asynchronously, so an immediate textContent
  // read can still hold the pre-swap value.
  const seededCount = await seedAttentionOf(page);

  // When B runs a turn in its own tab and settles (the settle lands
  // while A is viewed — the unseen-settle mark). Page B arrives by a
  // plain page load — deliberately NOT openSession, whose row click
  // would activate whichever session the list sorts first and race
  // the mark. The GET activates nothing; the active session stays A.
  const pageB = await page.context().newPage();
  await pageB.goto(`${baseUrl()}${sel.SESSION_PAGE_PATH}${sessionB}`);
  await expect(pageB.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionB,
  );
  await pageB.locator(sel.SESSION_ROW_BY_ID(sessionB)).waitFor();
  await startBusyTurn(pageB);
  await expect(page.locator(sel.SESSION_ROW_BY_ID(sessionB))).toHaveClass(
    /unread/,
    { timeout: 10_000 },
  );

  // Then A's button wears the attention class and the badge reads
  // seeded + 1 — pushed live, no reload of A.
  await expect(button).toHaveClass(new RegExp(sel.SESSIONS_ATTENTION_CLASS));
  await expect(badge).toHaveText(`${seededCount + 1}`);

  // And when A activates B (the row click through the drawer — the
  // real path a finger performs), B's mark clears: the swap's seed is
  // rendered after the fan-out's clear wave (the activation handler
  // awaits it), so the badge converges to the seed the swapped page
  // carries. Other sessions' marks may keep the button yellow.
  await page.locator(sel.SESSIONS_BUTTON).click();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveClass(
    new RegExp(sel.DRAWER_OPEN_CLASS),
  );
  await page.locator(sel.SESSION_ROW_BY_ID(sessionB)).click();
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(
    sel.PAGE_SESSION_ATTR,
    sessionB,
  );
  const postClearSeed = await seedAttentionOf(page);
  if (postClearSeed === 0) {
    await expect(button).not.toHaveClass(
      new RegExp(sel.SESSIONS_ATTENTION_CLASS),
    );
    await expect(badge).toBeHidden();
  } else {
    await expect(badge).toHaveText(`${postClearSeed}`);
    await expect(button).toHaveClass(new RegExp(sel.SESSIONS_ATTENTION_CLASS));
  }
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
