//! The `⋮` at each transcript entry's bottom-right carries **Fork**, the
//! terminal's `f` on the selected entry: a new session holding the history up
//! to and including that entry, which the page then moves onto.
//!
//! The fork is published as the same `SessionForkRequested` the terminal
//! sends, and the child reaches the page through the `ActiveSessionChanged`
//! snapshot every page already follows — so each test waits on the page's
//! own view, never on a reply. The child's id is minted inside the store, so
//! the page cannot wait on one; its loading overlay is cleared by the first
//! snapshot naming a session other than the fork's source.

import { entryMenu, expect, sel, sendAndWaitForEcho, startBusyTurn, test, viewedSessionId } from "./fixture.mjs";

/** Sends one message and waits for the assistant's reply to be visible. */
async function sendHello(page) {
  await page.locator(sel.DRAFT).fill("hello");
  await page.locator(sel.SEND).click();
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last()).toBeVisible();
}

test("the entry menu offers Fork under Pin", async ({ page }) => {
  // Given a transcript with an assistant reply.
  await sendHello(page);

  // When its ⋮ opens its menu.
  await entryMenu(page, "assistant").click();
  await expect(page.locator(sel.ENTRY_MENU_POPUP)).toBeVisible();

  // Then the item reads Fork and sits below Pin.
  const fork = page.locator(sel.ENTRY_MENU_FORK);
  await expect(fork).toBeVisible();
  await expect(fork).toHaveText("Fork");
  const pinBox = await page.locator(sel.ENTRY_MENU_PIN).boundingBox();
  const forkBox = await fork.boundingBox();
  expect(forkBox.y).toBeGreaterThan(pinBox.y);
});

test("forking an entry moves the page to a session holding the history up to it", async ({ page }) => {
  // Given a transcript with a user message and an assistant reply.
  await sendHello(page);
  const source = await viewedSessionId(page);

  // When the user entry's ⋮ opens its menu and Fork is picked.
  await entryMenu(page, "user").click();
  await page.locator(sel.ENTRY_MENU_FORK).click();

  // Then the page views a new session — one holding just the entry forked at.
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).not.toHaveAttribute(
    "data-session-id",
    source,
    { timeout: 10_000 },
  );
  const child = await viewedSessionId(page);
  expect(child).not.toBe(source);
  await expect(page.locator(`${sel.TRANSCRIPT} .entry`)).toHaveCount(1);
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)).toHaveCount(1);

  // And the rail still lists the source beside the child.
  await expect(page.locator(sel.ROW_WRAP(source))).toHaveCount(1);
  await expect(page.locator(sel.ROW_WRAP(child))).toHaveCount(1);
});

/**
 * Arms a latch that records whether the loading overlay ever entered the
 * DOM, then runs `act` and reports what it saw.
 *
 * A fork lands within milliseconds, so the overlay can be raised and
 * cleared between two of Playwright's polls — a `toBeVisible` that
 * auto-retries can miss the whole thing. A MutationObserver latched before
 * the click cannot: it sees the node the instant it is inserted, however
 * briefly it stays. The session-load spec uses the same latch for the same
 * reason; a helper that only asserts stays in the spec that asserts it.
 *
 * @returns {Promise<boolean>} whether the overlay appeared during `act`.
 */
async function overlayAppearsDuring(page, act) {
  await page.evaluate((selector) => {
    const seen = { ever: false };
    window.__forkOverlaySeen = seen;
    const observer = new MutationObserver(() => {
      if (document.querySelector(selector) !== null) seen.ever = true;
    });
    observer.observe(document.body, { childList: true, subtree: true });
    window.__forkOverlayObserver = observer;
  }, sel.SESSION_LOAD);
  await act();
  return page.evaluate((selector) => {
    window.__forkOverlayObserver.disconnect();
    const seen =
      window.__forkOverlaySeen.ever || document.querySelector(selector) !== null;
    delete window.__forkOverlayObserver;
    delete window.__forkOverlaySeen;
    return seen;
  }, sel.SESSION_LOAD);
}

test("forking shows the loading overlay until the fork lands", async ({ page }) => {
  // Given a transcript with a user message and an assistant reply.
  await sendHello(page);
  const source = await viewedSessionId(page);

  // When Fork is picked on the user entry — with the latch armed first, so
  // an overlay that lives for only one frame is still seen.
  await entryMenu(page, "user").click();
  const appeared = await overlayAppearsDuring(page, () =>
    page.locator(sel.ENTRY_MENU_FORK).click(),
  );

  // Then the overlay was raised by the click.
  expect(appeared).toBe(true);

  // And once the page views the child, the overlay is gone.
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).not.toHaveAttribute(
    "data-session-id",
    source,
    { timeout: 10_000 },
  );
  await expect(page.locator(sel.SESSION_LOAD)).toHaveCount(0);
});

test("Fork is offered while a turn is running", async ({ page }) => {
  // Given a session with a turn streaming.
  await startBusyTurn(page);

  // When the last entry's ⋮ opens its menu.
  await entryMenu(page, "user").click();

  // Then Fork is offered and enabled — the terminal's `f` has no busy gate.
  await expect(page.locator(sel.ENTRY_MENU_FORK)).toBeVisible();
  await expect(page.locator(sel.ENTRY_MENU_FORK)).toBeEnabled();
});
