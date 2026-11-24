//! Tools flow — the menu item opens the tools modal seeded from the
//! registry (provider- and bot-gated, the TUI `<leader>st` seed), a
//! row click commits immediately (the mark flips without a reload,
//! deny-over-off-rows), the flip survives a page reload, and Escape
//! reverts to the open-time state (absence restored as absence).
//!
//! Pivots on the `get_time` builtin: present in every run's registry
//! regardless of MCP state — the scratch jinn accumulates state across
//! the alphabetical spec run (the MCP spec leaves the echo server
//! enabled, whose tools join the registry), so specs assert rows by
//! name, never by count.

import { expect, test } from "@playwright/test";
import { openSession } from "./nav.mjs";
import { baseUrl } from "./run.mjs";
import * as sel from "./selectors.mjs";
const { TOOLS_SPEC_TOOL: TOOL } = sel;

/** Opens the session page and the tools modal through the menu. */
async function openToolsModal(page) {
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();
  await page.locator(sel.MENU_ITEM_TOOLS).click();
  const modal = page.locator(sel.TOOLS_MODAL);
  await expect(modal).toBeVisible();
  return modal;
}

/** The session's state for one tool, read from a fresh modal pull. */
async function toolIsOn(page) {
  await openToolsModal(page);
  // The class check runs in browser context: interpolate the constant,
  // never reference `sel` inside `evaluate`.
  const offClass = sel.TOOL_ROW_OFF;
  return page
    .locator(sel.TOOL_ROW(TOOL))
    .evaluate((row, off) => !row.classList.contains(off), offClass);
}

test("the menu item opens the modal seeded with the registry tool as enabled", async ({
  page,
}) => {
  // Given the boot session open and the menu opened.
  const modal = await openToolsModal(page);

  // Then the modal renders inside its own host span (never the menu's
  // container), the builtin tool renders as a row, and the row is on —
  // the scratch config omits `[tools]`, so the session starts
  // unrestricted.
  const host = page.locator(sel.TOOLS_MODAL_HOST);
  await expect(host.locator(sel.TOOLS_MODAL)).toHaveCount(1);
  const row = page.locator(sel.TOOL_ROW(TOOL));
  await expect(row).toBeVisible();
  await expect(row).not.toHaveClass(new RegExp(sel.TOOL_ROW_OFF));
  await expect(modal.locator(sel.TOOLS_ROWS)).toContainText(TOOL);
});

test("a row click flips the mark without a reload", async ({ page }) => {
  // Given the tools modal open with the tool on.
  await openToolsModal(page);
  const row = page.locator(sel.TOOL_ROW(TOOL));
  await expect(row).not.toHaveClass(new RegExp(sel.TOOL_ROW_OFF));

  // When clicking the row — the per-click commit, no staged apply.
  await row.click();

  // Then the re-rendered modal marks the row off immediately (the
  // POST response carried the swap): the filter is now a deny filter
  // over the off-rows, the TUI confirm's semantic applied per click.
  await expect(page.locator(sel.TOOL_ROW(TOOL))).toHaveClass(
    new RegExp(sel.TOOL_ROW_OFF),
  );
});

test("the flip survives a page reload", async ({ page }) => {
  // Given the tool withheld by the previous spec (the tier runs
  // serially against one scratch jinn, so the boot session's filter
  // persists across specs).
  const on = await toolIsOn(page);
  expect(on).toBe(false);

  // When reloading the page and re-opening the modal.
  await page.reload();
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_TOOLS).click();
  await expect(page.locator(sel.TOOLS_MODAL)).toBeVisible();

  // Then the re-opened modal still marks the row off — the write went
  // through the session's live tool filter, which the reload's
  // hydration restores.
  await expect(page.locator(sel.TOOL_ROW(TOOL))).toHaveClass(
    new RegExp(sel.TOOL_ROW_OFF),
  );
});

test("dismissal with Escape reverts to the open-time state", async ({
  page,
}) => {
  // Given the modal open on a session whose filter withholds the tool
  // (the open-time state this spec stamps into the revert signal).
  await openToolsModal(page);
  const row = page.locator(sel.TOOL_ROW(TOOL));
  await expect(row).toHaveClass(new RegExp(sel.TOOL_ROW_OFF));

  // When clicking the row on (a new open-time state of "all on") and
  // dismissing with Escape.
  await row.click();
  await expect(page.locator(sel.TOOL_ROW(TOOL))).not.toHaveClass(
    new RegExp(sel.TOOL_ROW_OFF),
  );
  await page.keyboard.press("Escape");

  // Then the modal closed, and a fresh pull shows the open-time
  // state restored — the tool off again, absence and mode restored
  // verbatim from the snapshot the open pull stamped.
  await expect(page.locator(sel.TOOLS_MODAL)).toBeHidden();
  await expect(await toolIsOn(page)).toBe(false);
});

test("a second click-and-dismiss returns to the original state", async ({
  page,
}) => {
  // Given the modal open on the restored session (the tool off — the
  // open-time state), clicking the row flips it on.
  await openToolsModal(page);
  const row = page.locator(sel.TOOL_ROW(TOOL));
  await expect(row).toHaveClass(new RegExp(sel.TOOL_ROW_OFF));
  await row.click();
  await expect(page.locator(sel.TOOL_ROW(TOOL))).not.toHaveClass(
    new RegExp(sel.TOOL_ROW_OFF),
  );

  // When dismissing with Escape again.
  await page.keyboard.press("Escape");
  await expect(page.locator(sel.TOOLS_MODAL)).toBeHidden();

  // Then the round-trip returns to the start: the tool off, the
  // session where this spec found it.
  await expect(await toolIsOn(page)).toBe(false);
});

test("an MCP connection change re-fills the open modal's rows without a reload", async ({
  page,
}) => {
  // Given the tools modal open on the boot session, with the scratch
  // echo server's tools absent (the MCP spec left the server disabled)
  // and the page's identity captured so a reload would be detectable.
  const modal = await openToolsModal(page);
  const sessionId = await page
    .locator(sel.PAGE_ROOT)
    .getAttribute(sel.PAGE_SESSION_ATTR);
  await expect(page.locator(sel.TOOL_ROW("mcp__echo__echo"))).toHaveCount(0);

  // When the same session's echo server is enabled from outside the
  // modal — a POST straight at the MCP toggle route, which is what the
  // MCP modal's own row click sends. The coordinator then spawns the
  // connection, writes its runtime cell, and emits the stored event
  // the fan-out turns into the `tools` signal.
  const response = await page.request.post(
    `${baseUrl()}${sel.MCP_TOGGLE_PATH}`,
    {
      form: { sessionId, server: sel.MCP_SERVER },
      headers: { accept: "*/*" },
    },
  );
  expect(response.ok()).toBe(true);

  // Then the open modal gains the newly connected server's row in
  // place, with no navigation at all: the `toolsTick` signal fired the
  // skeleton's effect, which re-pulled the rows route and swapped
  // `#tools-rows` only — the session page is still the one the spec
  // opened, and the revert stamp was never re-rendered.
  await expect(modal.locator(sel.TOOL_ROW("mcp__echo__echo"))).toBeVisible({
    timeout: 15_000,
  });
  expect(
    await page.locator(sel.PAGE_ROOT).getAttribute(sel.PAGE_SESSION_ATTR),
  ).toBe(sessionId);

  // Cleanup: leave the server disabled again, the state the MCP spec
  // ends in and the tools spec found.
  const disable = await page.request.post(
    `${baseUrl()}${sel.MCP_TOGGLE_PATH}`,
    {
      form: { sessionId, server: sel.MCP_SERVER },
      headers: { accept: "*/*" },
    },
  );
  expect(disable.ok()).toBe(true);
  await page.keyboard.press("Escape");
});

test("the landing page's menu carries no tools item", async ({ page }) => {
  // Given the landing page (nothing viewed).
  await page.goto(`${baseUrl()}/web`);

  // When opening the hamburger menu.
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();

  // Then the tools item is not offered on the landing page — nothing
  // is viewed, and the item's data-show hides it. The sessions item
  // survives.
  await expect(page.locator(sel.MENU_ITEM_TOOLS)).not.toBeVisible();
  await expect(page.locator(sel.MENU_ITEM_SESSIONS)).toBeVisible();
});

test("typing in the filter narrows the rows with marked runs", async ({
  page,
}) => {
  // Given the tools modal open with the builtin tool listed.
  await openToolsModal(page);
  const row = page.locator(sel.TOOL_ROW(TOOL));
  await expect(row).toBeVisible();
  const query = TOOL.slice(0, 3);

  // When typing a matching prefix with real keypresses (fill()
  // dispatches no keydown, and typing must not dismiss the modal).
  await page.locator(sel.TOOLS_FILTER).click();
  await page.keyboard.type(query);

  // Then the row is still visible with a marked run — the rows route
  // filtered and highlighted it. The final keystroke's re-pull races
  // the assertion: an in-flight pull from an earlier keystroke can
  // land after it, so wait for the marks to settle at the full query
  // — not the input (the input leads the re-render; the marks are
  // what the assertion reads).
  await expect(row).toBeVisible();
  await expect(row.locator("mark").first()).toHaveText(query);

  // And Escape dismisses the modal.
  await page.keyboard.press("Escape");
  await expect(page.locator(sel.TOOLS_MODAL)).toBeHidden();
});

test("a toggle under a filter keeps the filtered view and reverts to the open-time state", async ({
  page,
}) => {
  // Given the tools modal open, a filter typed, and the tool's
  // open-time state captured (the serial tier leaves `get_time` off;
  // this spec must not assume either state).
  await openToolsModal(page);
  const row = page.locator(sel.TOOL_ROW(TOOL));
  const offBefore = await row.evaluate(
    (el) => el.classList.contains("tools-row-off"),
  );
  const query = TOOL.slice(0, 3);
  await page.locator(sel.TOOLS_FILTER).click();
  await page.keyboard.type(query);
  // The final keystroke's re-pull races the mark: an in-flight pull
  // from an earlier keystroke can land after it, so wait for the
  // marks to settle at the full query — not the input (the input
  // leads the re-render; the marks are what the assertion reads).
  await expect(row.locator("mark").first()).toHaveText(query);

  // When toggling the filtered-in row.
  await row.click();

  // Then the row re-renders flipped but still filtered in, the input
  // intact — the rows-only response rendered the filtered view and
  // never touched the revert stamp.
  await expect(row).toBeVisible();
  if (offBefore) {
    await expect(row).not.toHaveClass(/tools-row-off/);
  } else {
    await expect(row).toHaveClass(/tools-row-off/);
  }
  await expect(page.locator(sel.TOOLS_FILTER)).toHaveValue(query);

  // And Escape reverts: reopening shows the unfiltered rows back in
  // the open-time state — the snapshot survived the filtering.
  await page.keyboard.press("Escape");
  await expect(page.locator(sel.TOOLS_MODAL)).toBeHidden();
  const reopened = await openToolsModal(page);
  await expect(reopened).toBeVisible();
  await expect(page.locator(sel.TOOLS_FILTER)).toHaveValue("");
  const offRestored = await page
    .locator(sel.TOOL_ROW(TOOL))
    .evaluate((el) => el.classList.contains("tools-row-off"));
  expect(offRestored).toBe(offBefore);
});
