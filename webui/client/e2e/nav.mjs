//! Spec navigation: every spec opens the boot session fresh.
//!
//! A fresh scratch db always boots with one active session (the
//! session map's `Default` mints it), so the row is there without any
//! seeding. Serial workers make the single-session assumptions safe.

import { expect } from "@playwright/test";
import * as sel from "./selectors.mjs";
import { baseUrl } from "./run.mjs";

/**
 * Navigates to `/web` and opens the boot session's row.
 *
 * Waits until the page's SSE stream is connected before returning:
 * the busy-controls push a cancel test relies on is broadcast-only
 * (no catch-up replay), so interacting before the stream opens races
 * a missed push. An init script counts stream fetches that got an
 * `text/event-stream` response — the Datastar runtime's transport is
 * `fetch()` plus manual SSE parsing (no EventSource), so counting
 * those responses observes "a stream opened", same semantics as the
 * old EventSource counter. An observer, not an app hook.
 *
 * Also waits for the transcript catch-up to settle before returning:
 * the navigation response renders the transcript empty and the
 * stream's catch-up appends the history; the `catchupCursor` signal
 * jumps off zero when it completes (the wrap's effect answers it with
 * the one bottom-scroll). Waiting on that signal keeps tests that
 * assert on transcript content from reading the empty pre-catch-up
 * frame.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<string>} the session id now in the page root.
 */
export async function openSession(page) {
  await observeEventSources(page);
  await page.goto(`${baseUrl()}/web`);
  const row = page.locator(sel.SESSION_ROW).first();
  await expect(row).toBeVisible();
  await row.click();
  await expect(page).toHaveURL(new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}`));
  const root = page.locator(sel.PAGE_ROOT);
  await expect(root).toBeVisible();
  const sessionId = await root.getAttribute(sel.PAGE_SESSION_ATTR);
  if (!sessionId) {
    throw new Error("page root carries no session id after activating a session");
  }
  await expectSseConnected(page);
  await expectCatchupSettled(page);
  return sessionId;
}

/**
 * Installs an init-script observer that (a) counts stream fetches
 * that got a `text/event-stream` response and (b) raises a flag when
 * the bundle's `datastar-signal-patch` document event reports a
 * non-zero `catchupCursor` — the from-zero catch-up's completion
 * patch. Both are observers on the bundle's own published events, not
 * app hooks; the bundle exposes no window-level signal store, so the
 * document event is the one observable channel.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves once the script is registered.
 */
async function observeEventSources(page) {
  await page.addInitScript(() => {
    window.__e2eSseOpens = 0;
    window.__e2eCatchupSettled = false;
    const Native = window.fetch;
    if (!Native) return;
    window.fetch = (...args) =>
      Native(...args).then((response) => {
        const url = typeof args[0] === "string" ? args[0] : args[0]?.url ?? "";
        if (
          url.includes("/web/stream/") &&
          response.headers.get("content-type")?.includes("text/event-stream")
        ) {
          window.__e2eSseOpens += 1;
        }
        return response;
      });
    document.addEventListener("datastar-signal-patch", (event) => {
      const patch = event.detail ?? {};
      if (patch.catchupCursor !== undefined && patch.catchupCursor > 0) {
        window.__e2eCatchupSettled = true;
      }
    });
  });
}

/**
 * Waits until at least one stream fetch on the page has connected.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves once the stream is connected.
 */
async function expectSseConnected(page) {
  await expect
    .poll(() => page.evaluate(() => window.__e2eSseOpens ?? 0), {
      timeout: 5_000,
    })
    .toBeGreaterThan(0);
}

/**
 * Waits until the transcript catch-up has settled: the stream's
 * from-zero catch-up completed (its completion patch carried a
 * non-zero `catchupCursor`), or the wait times out benignly — a page
 * with no history to catch up (an empty boot session) emits no
 * completion patch, and every spec asserts content it creates itself,
 * so a timeout here must not fail the tier.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves once the catch-up completed (or
 *   benignly times out on a history-less page).
 */
async function expectCatchupSettled(page) {
  try {
    await expect
      .poll(() => page.evaluate(() => window.__e2eCatchupSettled ?? true), {
        timeout: 2_000,
      })
      .toBe(true);
  } catch {
    // No completion patch arrived within the window: the page had no
    // catch-up to deliver (fresh session). Nothing to wait for.
  }
}
