//! MCP-servers flow — the menu item opens the modal listing every
//! configured server, a row click toggles that session's enablement
//! immediately, the badge reaches `running` once the coordinator
//! spawns the connection (via the mcp-status signal re-pull, no
//! reload), and a second click disables it again.
//!
//! The tier's scratch jinn.toml configures one real server:
//! `[mcp.echo]` serving the scratch luau echo tool over stdio —
//! network-free, so the coordinator's spawn actually reaches
//! `running`.

import { expect, test } from "@playwright/test";
import { openSession } from "./nav.mjs";
import { baseUrl } from "./run.mjs";
import * as sel from "./selectors.mjs";
const { MCP_SERVER } = sel;

/** Opens the session page and the MCP modal through the menu. */
async function openMcpModal(page) {
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();
  await page.locator(sel.MENU_ITEM_MCP).click();
  const modal = page.locator(sel.MCP_MODAL);
  await expect(modal).toBeVisible();
  return modal;
}

test("the menu item opens the modal listing the configured server as disabled", async ({
  page,
}) => {
  // Given the boot session open and the menu opened.
  const modal = await openMcpModal(page);

  // Then the modal renders inside its own host span (never the menu's
  // container) and the scratch server renders as one disabled row
  // whose badge reads `off` — a disabled server is not coming up, so
  // `starting` would lie about it; the boot session enables nothing.
  const host = page.locator(sel.MCP_MODAL_HOST);
  await expect(host.locator(sel.MCP_MODAL)).toHaveCount(1);
  const row = page.locator(sel.MCP_ROW(MCP_SERVER));
  await expect(row).toBeVisible();
  await expect(row).not.toHaveClass(new RegExp(sel.MCP_ROW_ENABLED));
  await expect(row.locator(`.${sel.MCP_BADGE_OFF}`)).toBeVisible();
  await expect(modal.locator(sel.MCP_ROWS)).toContainText(MCP_SERVER);
});

test("clicking the row enables the server and the badge reaches running without a reload", async ({
  page,
}) => {
  // Given the MCP modal open on the boot session.
  await openMcpModal(page);
  const row = page.locator(sel.MCP_ROW(MCP_SERVER));

  // When clicking the row — the per-click toggle, no staged apply.
  await row.click();

  // Then the re-rendered modal marks the row enabled immediately (the
  // POST response carried the swap)…
  await expect(page.locator(sel.MCP_ROW(MCP_SERVER))).toHaveClass(
    new RegExp(sel.MCP_ROW_ENABLED),
  );
  // …and the badge reaches `running` without any reload: the
  // coordinator spawns the stdio connection, writes the runtime cell,
  // emits the stored event, and the fan-out's mcp-status signal patch
  // makes the open modal re-pull its rows. Fresh-session enablement
  // spawns the connection; this is the loop the whole push seam
  // exists for.
  await expect(
    page.locator(`${sel.MCP_ROW(MCP_SERVER)} .${sel.MCP_BADGE_RUNNING}`),
  ).toBeVisible({ timeout: 15_000 });
});

test("a reload persists the enablement: re-opening shows the running badge", async ({
  page,
}) => {
  // Given the enabled boot session (the previous spec toggled it on;
  // the tier runs serially against one scratch jinn).
  await page.goto(`${baseUrl()}/web`);
  await page.locator(sel.SESSION_ROW).first().click();
  await page.waitForURL(new RegExp(sel.SESSION_PAGE_PATH));
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_MCP).click();
  await expect(page.locator(sel.MCP_MODAL)).toBeVisible();

  // Then the re-opened modal renders the persisted enabled set with
  // the live connection's `running` badge — enablement and status
  // survive the reload (session state + the coordinator's respawn on
  // SessionLoadCompleted).
  await expect(page.locator(sel.MCP_ROW(MCP_SERVER))).toHaveClass(
    new RegExp(sel.MCP_ROW_ENABLED),
  );
  await expect(
    page.locator(`${sel.MCP_ROW(MCP_SERVER)} .${sel.MCP_BADGE_RUNNING}`),
  ).toBeVisible({ timeout: 15_000 });
});

test("clicking the enabled row disables the server again", async ({ page }) => {
  // Given the MCP modal open with the server still enabled from the
  // earlier specs (the tier runs serially against one scratch jinn,
  // so the boot session's set persists across specs).
  await openMcpModal(page);
  const row = page.locator(sel.MCP_ROW(MCP_SERVER));
  await expect(row).toHaveClass(new RegExp(sel.MCP_ROW_ENABLED));

  // When clicking the row again.
  await row.click();

  // Then the re-rendered modal clears the enabled mark — the toggle
  // is a true XOR, and the session is back where it started.
  await expect(page.locator(sel.MCP_ROW(MCP_SERVER))).not.toHaveClass(
    new RegExp(sel.MCP_ROW_ENABLED),
  );
});

test("the landing page's menu carries no MCP item", async ({ page }) => {
  // Given the landing page (nothing viewed).
  await page.goto(`${baseUrl()}/web`);

  // When opening the hamburger menu.
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();

  // Then the MCP item is not offered on the landing page — nothing
  // is viewed, and the item's data-show hides it (the session-invariant
  // menu ships every item; the viewed-session signal gates the
  // session-scoped ones). The sessions item survives.
  await expect(page.locator(sel.MENU_ITEM_MCP)).not.toBeVisible();
  await expect(page.locator(sel.MENU_ITEM_SESSIONS)).toBeVisible();
});

test("typing in the filter narrows the rows with marked runs", async ({
  page,
}) => {
  // Given the MCP modal open with the scratch server listed.
  await openMcpModal(page);
  const row = page.locator(sel.MCP_ROW(sel.MCP_SERVER));
  await expect(row).toBeVisible();
  const query = sel.MCP_SERVER.slice(0, 2);

  // When typing a matching prefix with real keypresses (fill()
  // dispatches no keydown, and typing must not dismiss the modal).
  await page.locator(sel.MCP_FILTER).click();
  await page.keyboard.type(query);

  // Then the row is still visible with a marked run — the server
  // filtered and highlighted it.
  await expect(row).toBeVisible();
  await expect(row.locator("mark").first()).toHaveText(query);

  // And the input keeps what was typed.
  await expect(page.locator(sel.MCP_FILTER)).toHaveValue(query);

  // And Escape dismisses the modal.
  await page.keyboard.press("Escape");
  await expect(page.locator(sel.MCP_MODAL)).toBeHidden();
});

test("a toggle under a filter keeps the filtered view with the input intact", async ({
  page,
}) => {
  // Given the MCP modal open, the server disabled, and a filter typed.
  await openMcpModal(page);
  const row = page.locator(sel.MCP_ROW(sel.MCP_SERVER));
  const query = sel.MCP_SERVER.slice(0, 2);
  await page.locator(sel.MCP_FILTER).click();
  await page.keyboard.type(query);
  await expect(row.locator("mark").first()).toHaveText(query);

  // When toggling the filtered-in row (the scratch server starts
  // disabled; the click enables it).
  await row.click();

  // Then the row re-renders enabled but still filtered in, the input
  // intact — the toggle response rendered the view the user was
  // looking at.
  await expect(row).toHaveClass(/mcp-row-enabled/);
  await expect(page.locator(sel.MCP_FILTER)).toHaveValue(query);

  // And Escape dismisses; reopening shows the unfiltered rows with
  // the enablement kept (the filter is view-local).
  await page.keyboard.press("Escape");
  await expect(page.locator(sel.MCP_MODAL)).toBeHidden();
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_MCP).click();
  const reopened = page.locator(sel.MCP_MODAL);
  await expect(reopened).toBeVisible();
  // The reopen's fresh skeleton carries an empty filter; the bound
  // signal still holds the closed view's query, so clear it by typing
  // select-all + delete through the real input — the same path a user
  // would take — before asserting the reset.
  await page.locator(sel.MCP_FILTER).click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("Delete");
  await expect(page.locator(sel.MCP_FILTER)).toHaveValue("", {
    timeout: 10_000,
  });
  await expect(page.locator(sel.MCP_ROW(sel.MCP_SERVER))).toHaveClass(
    /mcp-row-enabled/,
  );

  // Cleanup: the serial tier's later specs pivot on the disabled
  // state this file's earlier specs restore, so disable again (the
  // click is the XOR; the view filter is gone in the reopened modal).
  await page.locator(sel.MCP_ROW(sel.MCP_SERVER)).click();
  await expect(page.locator(sel.MCP_ROW(sel.MCP_SERVER))).not.toHaveClass(
    /mcp-row-enabled/,
  );
});
