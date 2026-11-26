//! The unread mark: a session that finished a turn while the page looked
//! elsewhere.
//!
//! A turn in a background session is run through the instance's control
//! surface, not the page — every `/web2` page follows the instance's
//! active session, so the page cannot drive a session it is not viewing.
//! The mark itself is the browser's own: what it has seen is its own
//! record, kept across reloads.

import {
  createSession,
  expect,
  rowOf,
  sel,
  sendAndWaitForEcho,
  startBackgroundTurn,
  test,
  toggleLibraryThroughMenu,
  viewedSessionId,
} from "./fixture.mjs";

/** Narrower than the 768px breakpoint, where the `#` button exists. */
const PHONE = { width: 390, height: 800 };

/**
 * An element's background colour, resolved. A class alone does not prove
 * the fill landed: the stylesheet can order a stronger rule after the
 * mark and win, which is exactly how a selected tab hid its own mark.
 */
const backgroundOf = (locator) =>
  locator.evaluate((node) => getComputedStyle(node).backgroundColor);

/** The page's `--mark` token, resolved to the `rgb()` form the computed style reports. */
async function markColour(page) {
  return page.evaluate(() => {
    const token = getComputedStyle(document.documentElement).getPropertyValue("--mark").trim();
    const n = parseInt(token.slice(1), 16);
    return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
  });
}

/** The page's `--accent` token, in the same resolved form. */
async function accentColour(page) {
  return page.evaluate(() => {
    const token = getComputedStyle(document.documentElement).getPropertyValue("--accent").trim();
    const n = parseInt(token.slice(1), 16);
    return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
  });
}

/** A tab's selection underline: its `::after`'s resolved background, or null when it has none. */
const underlineOf = (locator) =>
  locator.evaluate((node) => {
    const style = getComputedStyle(node, "::after");
    return style.content === "none" ? null : style.backgroundColor;
  });

/** What only the first session holds, to find its row by. */
const MARKER = "only the first session holds this";

/**
 * Arranges two sessions with the first in view and the second in the
 * background, and returns both ids.
 *
 * The boot session is the first. Creating the second makes it active,
 * so the page moves to it; clicking the first's row brings the page back
 * without running a turn in either.
 */
async function arrangeBackgroundSession(page) {
  const firstId = await viewedSessionId(page);
  await sendAndWaitForEcho(page, MARKER);
  const secondId = await createSession(page);
  await page.locator(sel.SESSION_ROW).filter({ hasText: MARKER }).click();
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    firstId,
  );
  return { firstId, secondId };
}

test("a background session that settles is marked unread", async ({ page, jinn }) => {
  // Given two sessions, the first in view and the second idle.
  const { secondId } = await arrangeBackgroundSession(page);

  // When the second runs a turn and settles.
  await startBackgroundTurn(jinn, secondId);

  // Then its row carries the unread mark.
  await expect(rowOf(page, secondId)).toHaveClass(/unread/, { timeout: 10_000 });
});

test("the viewed session settling is not marked", async ({ page }) => {
  // Given one session, in view and idle.
  const id = await viewedSessionId(page);

  // When it runs a turn and settles, with the page watching.
  await sendAndWaitForEcho(page, "!response");

  // Then its row carries no mark.
  await expect(rowOf(page, id)).not.toHaveClass(/unread/);
});

test("viewing a marked session clears its mark", async ({ page, jinn }) => {
  // Given a background session that settled while another was viewed.
  const { secondId } = await arrangeBackgroundSession(page);
  await startBackgroundTurn(jinn, secondId);
  await expect(rowOf(page, secondId)).toHaveClass(/unread/, { timeout: 10_000 });

  // When the page views it by clicking its row.
  await rowOf(page, secondId).click();

  // Then the mark is gone.
  await expect(rowOf(page, secondId)).not.toHaveClass(/unread/);
});

test("a mark survives a reload", async ({ page, jinn }) => {
  // Given a marked session.
  const { secondId } = await arrangeBackgroundSession(page);
  await startBackgroundTurn(jinn, secondId);
  await expect(rowOf(page, secondId)).toHaveClass(/unread/, { timeout: 10_000 });

  // When the page reloads.
  await page.reload();
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toBeVisible();

  // Then the mark is still there — the page's own record persisted.
  await expect(rowOf(page, secondId)).toHaveClass(/unread/);
});

/**
 * Arranges a phone-width page with one unread session, and returns its
 * id. The sessions are arranged at desktop width first, because the rail
 * is off-canvas on a phone and its rows cannot be clicked there; the
 * viewport changes once the mark is on.
 */
async function arrangePhoneWithUnread(page, jinn) {
  const { secondId } = await arrangeBackgroundSession(page);
  await startBackgroundTurn(jinn, secondId);
  await expect(rowOf(page, secondId)).toHaveClass(/unread/, { timeout: 10_000 });
  await page.setViewportSize(PHONE);
  return secondId;
}

test("at phone width the sessions button is marked and shows the count", async ({
  page,
  jinn,
}) => {
  // Given a phone-width page with one unread session.
  await arrangePhoneWithUnread(page, jinn);

  // Then the `#` is filled with the mark's colour, and shows one unread
  // session.
  const mark = await markColour(page);
  await expect(page.locator(sel.SESSIONS_BUTTON)).toHaveClass(/unread/);
  expect(await backgroundOf(page.locator(sel.SESSIONS_BUTTON))).toBe(mark);
  await expect(page.locator(sel.SESSIONS_BUTTON_COUNT)).toHaveText("1");
});

test("the sessions button count clears with the mark", async ({ page, jinn }) => {
  // Given a phone-width page with one unread session.
  const secondId = await arrangePhoneWithUnread(page, jinn);
  await expect(page.locator(sel.SESSIONS_BUTTON_COUNT)).toHaveText("1");

  // When the user opens the drawer and picks the marked row.
  await page.locator(sel.SESSIONS_BUTTON).click();
  await expect(page.locator(sel.RAIL)).toBeVisible();
  await rowOf(page, secondId).click();

  // Then the count is gone and the button is no longer marked.
  await expect(page.locator(sel.SESSIONS_BUTTON_COUNT)).toHaveCount(0);
  await expect(page.locator(sel.SESSIONS_BUTTON)).not.toHaveClass(/unread/);
});

test("the tab holding the unread row is marked", async ({ page, jinn }) => {
  // Given a background session in the Sessions list that settled.
  const { secondId } = await arrangeBackgroundSession(page);
  await startBackgroundTurn(jinn, secondId);
  await expect(rowOf(page, secondId)).toHaveClass(/unread/, { timeout: 10_000 });

  // Then the Sessions tab — the selected one — is filled with the mark's
  // colour, and the Library tab is not.
  const mark = await markColour(page);
  await expect(page.locator(sel.SESSIONS_VIEW)).toHaveClass(/unread/);
  expect(await backgroundOf(page.locator(sel.SESSIONS_VIEW))).toBe(mark);
  await expect(page.locator(sel.LIBRARY_VIEW)).not.toHaveClass(/unread/);
  expect(await backgroundOf(page.locator(sel.LIBRARY_VIEW))).not.toBe(mark);
});

test("an unread library member marks the Library tab and still counts", async ({
  page,
  jinn,
}) => {
  // Given a phone-width page with one unread session.
  const secondId = await arrangePhoneWithUnread(page, jinn);
  await expect(page.locator(sel.SESSIONS_BUTTON_COUNT)).toHaveText("1");

  // When that session is moved into the library.
  await page.locator(sel.SESSIONS_BUTTON).click();
  await expect(page.locator(sel.RAIL)).toBeVisible();
  await toggleLibraryThroughMenu(page, secondId);

  // Then the Library tab is marked, the Sessions tab is not, and the
  // count still includes it.
  await expect(page.locator(sel.LIBRARY_VIEW)).toHaveClass(/unread/);
  await expect(page.locator(sel.SESSIONS_VIEW)).not.toHaveClass(/unread/);
  await expect(page.locator(sel.SESSIONS_BUTTON_COUNT)).toHaveText("1");
});

test("the selected tab wears the accent underline, marked or not", async ({ page, jinn }) => {
  // Given two sessions, the Sessions tab selected and nothing unread.
  const { secondId } = await arrangeBackgroundSession(page);
  const accent = await accentColour(page);

  // Then the selected tab has the underline and the other does not.
  expect(await underlineOf(page.locator(sel.SESSIONS_VIEW))).toBe(accent);
  expect(await underlineOf(page.locator(sel.LIBRARY_VIEW))).toBeNull();

  // When a session in the selected list settles, turning that tab gold.
  await startBackgroundTurn(jinn, secondId);
  await expect(rowOf(page, secondId)).toHaveClass(/unread/, { timeout: 10_000 });
  const mark = await markColour(page);
  expect(await backgroundOf(page.locator(sel.SESSIONS_VIEW))).toBe(mark);

  // Then the underline is still there over the mark.
  expect(await underlineOf(page.locator(sel.SESSIONS_VIEW))).toBe(accent);
});

test("following a switch from another page clears the mark", async ({ page, browser, jinn }) => {
  // Given a marked session.
  const { secondId } = await arrangeBackgroundSession(page);
  await startBackgroundTurn(jinn, secondId);
  await expect(rowOf(page, secondId)).toHaveClass(/unread/, { timeout: 10_000 });

  // When another page switches the instance to it.
  const other = await browser.newPage();
  try {
    await other.goto(page.url());
    await rowOf(other, secondId).click();
  } finally {
    await other.close();
  }

  // Then this page follows, and the mark is gone.
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    secondId,
    { timeout: 10_000 },
  );
  await expect(rowOf(page, secondId)).not.toHaveClass(/unread/);
});
