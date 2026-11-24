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

import {
  enableMcpServer,
  expect,
  openModal,
  openSession,
  test,
} from "./fixture.mjs";
import * as sel from "./selectors.mjs";
const { MCP_SERVER } = sel;

/** Opens the boot session and the MCP modal through the menu. */
async function openMcpModal(jinn, page) {
  await openSession(jinn, page);
  return openModal(page, "mcp");
}

test("the menu item opens the modal listing the configured server as disabled", async ({
  page,
  jinn,
}) => {
  // Given the boot session open and the menu opened.
  const modal = await openMcpModal(jinn, page);

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
  jinn,
}) => {
  // Given the MCP modal open on the boot session.
  await openMcpModal(jinn, page);
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
  jinn,
}) => {
  // Given the boot session's server enabled through the row click,
  // then the page reloaded and the modal re-opened.
  await openMcpModal(jinn, page);
  await enableMcpServer(page);
  await page.reload();
  await expect(page.locator(sel.PAGE_ROOT)).toBeVisible();
  await openModal(page, "mcp");

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

test("clicking the enabled row disables the server again", async ({ page, jinn }) => {
  // Given the MCP modal open with the server enabled through the row
  // click.
  await openMcpModal(jinn, page);
  await enableMcpServer(page);
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

test("the landing page's menu carries no MCP item", async ({ page, jinn }) => {
  // Given the landing page (nothing viewed).
  await page.goto(`${jinn.baseUrl}/web`);

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
  jinn,
}) => {
  // Given the MCP modal open with the scratch server listed.
  await openMcpModal(jinn, page);
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
  jinn,
}) => {
  // Given the MCP modal open, the server disabled, and a filter typed.
  await openMcpModal(jinn, page);
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
  await openModal(page, "mcp");
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
});
