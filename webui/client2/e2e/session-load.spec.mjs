//! The session-load overlay: what raises it, and the ways it ends.
//!
//! The page raises it itself, the instant it asks the server to activate a
//! session, and clears it only when the snapshot for *that* session lands, a
//! refusal answers, or ten seconds pass with neither. It covers the whole
//! page — a second activation cannot fire under it.
//!
//! Two tests hold the page's socket with `page.routeWebSocket` so no snapshot
//! ever answers the click. The route forwards everything the server sends and
//! drops only the page's `activate_session`, so the connection stays alive
//! (the liveness timeout never trips) and the overlay is the only thing on
//! the move. The route applies to sockets opened *after* it is installed,
//! so the helper reloads the page once it is in place.

import {
  archiveSession,
  createSession,
  expect,
  sel,
  sendAndWaitForEcho,
  test,
  viewedSessionId,
} from "./fixture.mjs";
import { restartJinn } from "./harness.mjs";

/** The sentence the composer shows when a load stalls. */
const STALL_TEXT = "Session switch timed out. The session did not load.";

/**
 * Routes the page's WebSocket so the server's frames still arrive but the
 * page's `activate_session` never reaches it: a click raises the overlay and
 * nothing can clear it. Every other client frame is forwarded, so the page
 * behaves normally otherwise. Reloads the page so its socket goes through
 * the route, and waits for it to reconnect.
 */
async function holdActivate(page) {
  await page.routeWebSocket(/\/ws$/, (ws) => {
    const server = ws.connectToServer();
    // Page -> server: the activation is swallowed, everything else passes.
    // Once this handler is set nothing forwards on its own, so `hello` and
    // the rest must be sent on explicitly.
    ws.onMessage((message) => {
      if (typeof message === "string" && message.includes('"activate_session"')) {
        return;
      }
      server.send(message);
    });
    // Server -> page: every frame still reaches the page.
    server.onMessage((message) => {
      ws.send(message);
    });
  });
  await page.reload();
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0);
}

/**
 * Arms a latch that records whether the loading overlay ever entered the
 * DOM, then runs `act` and reports what it saw.
 *
 * An in-memory switch answers within milliseconds, so the overlay can be
 * raised and cleared between two of Playwright's polls — a `toBeVisible`
 * that auto-retries can miss the whole thing. A MutationObserver latched
 * before the click cannot: it sees the node the instant it is inserted,
 * however briefly it stays.
 *
 * @returns {Promise<boolean>} whether the overlay appeared during `act`.
 */
async function overlayAppearsDuring(page, act) {
  await page.evaluate((selector) => {
    const seen = { ever: false };
    window.__sessionLoadSeen = seen;
    const observer = new MutationObserver(() => {
      if (document.querySelector(selector) !== null) seen.ever = true;
    });
    observer.observe(document.body, { childList: true, subtree: true });
    window.__sessionLoadObserver = observer;
  }, sel.SESSION_LOAD);
  await act();
  return page.evaluate((selector) => {
    window.__sessionLoadObserver.disconnect();
    const seen =
      window.__sessionLoadSeen.ever || document.querySelector(selector) !== null;
    delete window.__sessionLoadObserver;
    delete window.__sessionLoadSeen;
    return seen;
  }, sel.SESSION_LOAD);
}

test("clicking a session row shows the loading overlay until the session lands", async ({
  page,
}) => {
  // Given two sessions, the first in view, each titled by its own message.
  const firstId = await viewedSessionId(page);
  await sendAndWaitForEcho(page, "a marker only the first session holds");
  await createSession(page);
  await page.reload();
  const first = page
    .locator(sel.SESSION_ROW)
    .filter({ hasText: "a marker only the first session holds" });
  const secondId = await viewedSessionId(page);
  expect(secondId).not.toBe(firstId);

  // When clicking the first session's row — with the latch armed first, so
  // an overlay that lives for only one frame is still seen.
  const appeared = await overlayAppearsDuring(page, () => first.click());

  // Then the overlay was raised by the click — no wait for the click's own
  // round trip. (The label itself is pinned by the held-socket tests, where
  // the overlay stays up.)
  expect(appeared).toBe(true);

  // And it clears when that session's snapshot lands: the page now views it.
  await expect(page.locator(sel.SESSION_LOAD)).toHaveCount(0);
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    firstId,
  );
  await expect(page.locator(sel.TRANSCRIPT)).toContainText(
    "a marker only the first session holds",
  );
});

test("the overlay covers the whole viewport and sits above the rail", async ({
  page,
}) => {
  // Given the overlay up (the activate frame held so it cannot clear).
  await createSession(page);
  await holdActivate(page);
  await page.locator(`${sel.SESSION_ROW}`).first().click();
  await expect(page.locator(sel.SESSION_LOAD)).toBeVisible();

  // Then it carries the label, verbatim.
  await expect(page.locator(sel.SESSION_LOAD)).toContainText("Loading session…");

  // And its box is the whole viewport…
  const box = await page.locator(sel.SESSION_LOAD).boundingBox();
  const viewport = page.viewportSize();
  expect(box.x).toBe(0);
  expect(box.y).toBe(0);
  expect(box.width).toBe(viewport.width);
  expect(box.height).toBe(viewport.height);

  // …and the element the pointer would hit at the rail's centre is the
  // overlay, not the rail under it.
  const railBox = await page.locator(sel.RAIL).boundingBox();
  const hit = await page.evaluate(
    ([x, y]) => {
      const node = document.elementFromPoint(x, y);
      return node === null ? null : node.getAttribute("data-testid");
    },
    [railBox.x + railBox.width / 2, railBox.y + railBox.height / 2],
  );
  expect(hit).toBe("session-load-overlay");
});

test("a snapshot naming another session does not clear the overlay", async ({
  page,
  browser,
}) => {
  // Given three sessions — the viewed one, one this page will ask for, and
  // a third another page will switch to — with the page reloaded through
  // the held route.
  await createSession(page);
  await createSession(page);
  await holdActivate(page);
  const viewingId = await viewedSessionId(page);
  const ids = await page
    .locator(`${sel.SESSION_ROW}`)
    .evaluateAll((rows) => rows.map((row) => row.dataset.sessionId));
  // The page asks for the first row that is not the viewed session; the
  // other page will later switch to one that is neither.
  const requestedId = ids.find((id) => id !== viewingId);
  const thirdId = ids.find((id) => id !== viewingId && id !== requestedId);
  const rowOf = (id) => page.locator(`${sel.SESSION_ROW}[data-session-id="${id}"]`);
  await rowOf(requestedId).click();

  // Then the overlay is up, and the page still views the session it never
  // left — its activate frame is held, so nothing has answered.
  await expect(page.locator(sel.SESSION_LOAD)).toBeVisible();
  expect(await viewedSessionId(page)).toBe(viewingId);

  // When another page switches the instance to the third session: this page
  // receives a pushed snapshot, but for a session it never asked for.
  const other = await browser.newPage();
  try {
    await other.goto(page.url());
    await other.locator(`${sel.SESSION_ROW}[data-session-id="${thirdId}"]`).click();
    await expect(
      other.locator(`${sel.SESSION_ROW}[data-session-id="${thirdId}"]`),
    ).toHaveClass(/active/);
  } finally {
    await other.close();
  }

  // Then this page follows to the third session, and the overlay stays up:
  // the snapshot it received did not name the session it is waiting on.
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    thirdId,
  );
  await expect(page.locator(sel.SESSION_LOAD)).toBeVisible();
});

test("a second click during the load cannot reach the rail", async ({ page }) => {
  // Given two sessions and the overlay up (the activate frame held).
  await createSession(page);
  await holdActivate(page);
  await page.locator(sel.SESSION_ROW).first().click();
  await expect(page.locator(sel.SESSION_LOAD)).toBeVisible();

  // When another row is clicked the ordinary way: the overlay intercepts the
  // pointer, so the click never lands — Playwright's actionability check
  // refuses, and that refusal is the assertion.
  const target = page.locator(sel.SESSION_ROW).nth(1);
  await expect(target.click({ timeout: 2_000 })).rejects.toThrow();

  // Then the overlay is still up, and no second activation was sent.
  await expect(page.locator(sel.SESSION_LOAD)).toBeVisible();
});

test("a stalled activation clears the overlay at the timeout", async ({ page }) => {
  // Given the overlay up, and nothing that can ever clear it: the activate
  // frame is held, so no snapshot and no refusal will arrive.
  await createSession(page);
  await holdActivate(page);
  const before = await viewedSessionId(page);
  await page.locator(sel.SESSION_ROW).first().click();
  await expect(page.locator(sel.SESSION_LOAD)).toBeVisible();

  // Then the stall budget ends it: the overlay clears, the composer names
  // the stall, and the page is still on the session it never left.
  await expect(page.locator(sel.SESSION_LOAD)).toHaveCount(0, { timeout: 20_000 });
  await expect(page.locator(sel.COMPOSER_ERROR)).toContainText(STALL_TEXT);
  expect(await viewedSessionId(page)).toBe(before);
});

test("a session loaded from disk keeps the overlay up until it lands", async ({
  jinn,
  page,
}) => {
  // Given a session that is archived and out of memory — on disk, not in the
  // running instance — with a marker only it holds.
  const archivedId = await viewedSessionId(page);
  await sendAndWaitForEcho(page, "only the archived session holds this");
  await archiveSession(jinn, archivedId);
  await restartJinn(jinn);
  await page.goto(jinn.baseUrl);
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0, { timeout: 30_000 });

  // When picking it from the Archive picker — the disk-load path.
  await page.locator(sel.OPEN_PICKER).click();
  const row = page
    .locator(sel.PICKER_ROW)
    .filter({ hasText: "only the archived session holds this" });
  await expect(row).toBeVisible();

  // When picking it — the disk-load path, with the latch armed first so an
  // overlay that lives for only one frame is still seen.
  const appeared = await overlayAppearsDuring(page, () => row.click());

  // Then the overlay was raised by the pick, and it has cleared once the
  // session is in memory and its transcript is on the page.
  expect(appeared).toBe(true);
  await expect(page.locator(sel.SESSION_LOAD)).toHaveCount(0, { timeout: 15_000 });
  await expect(page.locator(sel.TRANSCRIPT)).toContainText(
    "only the archived session holds this",
  );
});
