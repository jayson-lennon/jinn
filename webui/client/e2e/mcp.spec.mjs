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
  // whose badge starts at `starting` — the enabled-but-unreported
  // state; the boot session enables nothing.
  const host = page.locator(sel.MCP_MODAL_HOST);
  await expect(host.locator(sel.MCP_MODAL)).toHaveCount(1);
  const row = page.locator(sel.MCP_ROW(MCP_SERVER));
  await expect(row).toBeVisible();
  await expect(row).not.toHaveClass(new RegExp(sel.MCP_ROW_ENABLED));
  await expect(
    row.locator(`.${sel.MCP_BADGE_STARTING}`),
  ).toBeVisible();
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

  // Then the MCP item is absent from the markup — the server omitted
  // it, and the sessions item survives.
  await expect(page.locator(sel.MENU_ITEM_MCP)).toHaveCount(0);
  await expect(page.locator(sel.MENU_ITEM_SESSIONS)).toBeVisible();
});
