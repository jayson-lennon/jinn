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
  return sessionId;
}

/**
 * Installs an init-script observer counting EventSource opens.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves once the script is registered.
 */
async function observeEventSources(page) {
  await page.addInitScript(() => {
    window.__e2eSseOpens = 0;
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
