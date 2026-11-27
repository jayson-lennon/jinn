//! The rail's loading row: what keeps it up, and what brings it down.
//!
//! The row lives in the session list, below any rows already loaded, and
//! reads "loading sessions…". The page shows it from first paint until its
//! first snapshot lands, and again while the server's startup hydration is
//! still running — a fact the server ships on every snapshot and every
//! session-list frame, and clears only when the store announces hydration
//! has finished.
//!
//! The held tests route the page's WebSocket with `page.routeWebSocket` so
//! the page's `hello` never reaches the server. The route forwards every
//! server frame — the liveness pings included, so the connection stays open
//! and the "connecting…" overlay is gone — but with no `hello` there is no
//! snapshot, which is the pre-snapshot state a fresh page is in. The route
//! applies to sockets opened *after* it is installed, so the helper reloads
//! the page once it is in place. A WebSocket route cannot be removed once
//! installed, so releasing the hold flips a latch the route reads instead.
//!
//! The mid-hydration state — rows already pushed, more still coming — is
//! pinned by the web UI crate's unit tests (the `hydrating` flag on the
//! wire), not here: the harness waits for the server's health check before
//! a page opens, and a scratch database hydrates in milliseconds.

import { expect, sel, test, awaitConnected } from "./fixture.mjs";

/** What the loading row says, verbatim. */
const LOADING_TEXT = "loading sessions…";

/**
 * Routes the page's WebSocket so the server's frames still arrive but the
 * page's `hello` never reaches it: the socket opens (the "connecting…"
 * overlay clears) but no snapshot ever answers, so the rail stays empty and
 * its loading row stays up. Every other client frame is forwarded, so the
 * page behaves normally otherwise. Reloads the page so its socket goes
 * through the route, and waits for the connection.
 *
 * Returns the latch that lets the `hello` through again. A WebSocket route
 * cannot be removed — `page.unrouteAll` is documented for `page.route`
 * routes only — so the route stays installed and the latch is what changes.
 *
 * @returns {Promise<{ release: () => void }>} the hold's latch.
 */
async function holdHello(page) {
  let held = true;
  await page.routeWebSocket(/\/ws$/, (ws) => {
    const server = ws.connectToServer();
    // Page -> server: the hello is swallowed while held, everything else
    // passes. Once this handler is set nothing forwards on its own, so the
    // rest must be sent on explicitly.
    ws.onMessage((message) => {
      if (held && typeof message === "string" && message.includes('"hello"')) {
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
  await awaitConnected(page);
  return { release: () => { held = false; } };
}

test("the rail shows the loading row until the first snapshot lands", async ({
  page,
}) => {
  // Given a fresh page whose first snapshot is still in flight.
  const hold = await holdHello(page);

  // Then the rail shows the loading row, and nothing else: no row has
  // arrived to sit above it.
  const row = page.locator(`${sel.RAIL} ${sel.RAIL_LOADING}`);
  await expect(row).toBeVisible();
  await expect(row).toContainText(LOADING_TEXT);
  await expect(page.locator(sel.SESSION_ROW)).toHaveCount(0);

  // When the snapshot lands — the hold is released and the page reloads, so
  // its `hello` goes through and is answered.
  hold.release();
  await page.reload();
  await awaitConnected(page);

  // Then the loading row is gone, and real rows are in its place.
  await expect(page.locator(sel.RAIL_LOADING)).toHaveCount(0);
  await expect(page.locator(sel.SESSION_ROW)).not.toHaveCount(0);
});

test("the library tab shows the loading row too", async ({ page }) => {
  // Given a fresh page whose first snapshot is still in flight.
  await holdHello(page);

  // When switching to the Library tab.
  await page.locator(sel.LIBRARY_VIEW).click();

  // Then the loading row is still there: it belongs to the list, not to the
  // rows, so it shows in whichever tab is open.
  await expect(page.locator(`${sel.RAIL} ${sel.RAIL_LOADING}`)).toBeVisible();
});

test("the loading row does not flash once the page is synced", async ({
  page,
}) => {
  // Given a page that has connected and received its snapshot — and whose
  // server finished hydrating long before the health check let it open.
  await awaitConnected(page);

  // Then the loading row is absent. The client starts with `hydrating`
  // true, so this pins that the snapshot's value is what clears it — a page
  // that ignored the field would keep the row up for good.
  await expect(page.locator(sel.RAIL_LOADING)).toHaveCount(0);
});
