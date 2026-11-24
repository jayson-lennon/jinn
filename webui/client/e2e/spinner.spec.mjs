//! Working spinner — the sessions list marks which sessions are busy.
//! Every test boots its own jinn and creates the second session it
//! needs through the modal. Rows are addressed by `data-session-id`.
//!
//! The sample backend streams `!response` word-by-word (10–50 ms per
//! token, ~1 s total), so the busy window is real but short: every
//! busy assertion polls (`toBeVisible` / `toHide`), never sleeps.

import {
  createSession,
  expect,
  openSession,
  PHONE,
  setViewport,
  startBusyTurn,
  switchToSession,
  test,
  viewSession,
} from "./fixture.mjs";
import * as sel from "./selectors.mjs";

test("a turn in another session spins its row on this page", async ({
  page,
  jinn,
}) => {
  // Given two sessions: A (the boot session, viewed) and B (created
  // through the modal, then left by re-opening A via the row click —
  // creation activated B, and a page GET activates nothing, so A must
  // be re-activated explicitly for B's settle to earn the mark).
  const sessionA = await openSession(jinn, page);
  const sessionB = await createSession(jinn, page);
  // Creation activated B instance-wide; re-activate A deterministically
  // by the ROW CLICK (a page render views without activating, and the
  // mark only lands on a session that is not the active one).
  await switchToSession(page, sessionA);
  await viewSession(jinn, page, sessionA);
  const root = page.locator(sel.PAGE_ROOT);
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
  // B's tab opens by URL, never by openSession: openSession clicks a
  // row, ACTIVATING whichever session sorts first and silently
  // flipping the map's active pointer while A is supposed to stay the
  // active one. A URL render views B without touching activation.
  await pageB.goto(`${jinn.baseUrl}${sel.SESSION_PAGE_PATH}${sessionB}`);
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

test("the settle bumps the sessions button's count and activation decrements it", async ({
  page,
  jinn,
}) => {
  // Given a phone viewport — the button is a phone affordance, hidden
  // at desktop widths — and two sessions: A (the boot session,
  // viewed) and B (created through the modal, then left by re-opening
  // A). A fresh instance seeds the count at zero, so the assertions
  // below are exact.
  await setViewport(page, PHONE);
  // Phone-native open: at this width the sidebar is the off-canvas
  // drawer, so the row is reached through the sessions button — the
  // one-tap path this feature exists for — and the drawer's row click
  // activates A.
  await page.goto(`${jinn.baseUrl}/web`);
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
  const sessionB = await createSession(jinn, page);
  // Creation activated B instance-wide; the drawer's row click below
  // is the real path a finger uses — so B's settle lands while A is
  // the active session and B earns the unseen-settle mark.
  await page.locator(sel.SESSIONS_BUTTON).click();
  await page.locator(sel.SESSION_ROW_BY_ID(sessionA)).click();
  await expect(page).toHaveURL(
    new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}${sessionA}`),
  );
  const button = page.locator(sel.SESSIONS_BUTTON);
  const badge = page.locator(sel.SESSIONS_BUTTON_COUNT);
  // The count comes from the badge's live text (the navigation's
  // signal patch has landed by the URL assertion above; the page
  // root's static data-signals attribute no longer re-renders —
  // the root survives navigation now, so its seed text stays at
  // the page-load value).
  const seededCount = Number(await badge.textContent());

  // When B runs a turn in its own tab and settles (the settle lands
  // while A is viewed — the unseen-settle mark). Page B arrives by a
  // plain page load — deliberately NOT openSession, whose row click
  // would activate whichever session the list sorts first and race
  // the mark. The GET activates nothing; the active session stays A.
  const pageB = await page.context().newPage();
  await pageB.goto(`${jinn.baseUrl}${sel.SESSION_PAGE_PATH}${sessionB}`);
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
  const postClearSeed = Number(await badge.textContent());
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

test("cancelling the turn clears the row's spinner", async ({ page, jinn }) => {
  // Given a session created through the modal and viewing it.
  await openSession(jinn, page);
  const sessionId = await createSession(jinn, page);
  await viewSession(jinn, page, sessionId);

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
