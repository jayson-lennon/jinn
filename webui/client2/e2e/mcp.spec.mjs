//! The hamburger menu's MCP servers item: the modal it opens over the
//! `[mcp.<name>]` entries in `jinn.toml`, the toggle that commits at
//! once, and the badge that reaches `running` on its own. The staged
//! server is a scratch echo tool served over stdio by `jinn luau serve`.

import {
  expect,
  MCP_SERVER,
  openMcp,
  openMenu,
  sel,
  test,
} from "./fixture.mjs";

test.with({ mcp: true })("the menu offers an MCP servers item while a session is active", async ({
  page,
}) => {
  // Given an open menu.
  await openMenu(page);

  // Then it lists MCP servers.
  await expect(page.locator(sel.MENU_ITEM_MCP)).toBeVisible();
});

test.with({ mcp: true })("the MCP servers item opens the modal listing the configured server", async ({
  page,
}) => {
  // Given the boot session, whose config holds the staged server.
  // When opening the MCP modal through the menu.
  await openMcp(page);

  // Then the menu closed, and the modal lists the server — disabled and
  // reading `off`, as a fresh session has enabled nothing.
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
  const row = page.locator(sel.MCP_ROW_NAMED(MCP_SERVER));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row).toHaveAttribute("data-enabled", "false");
  await expect(row).toHaveAttribute("data-badge", "off");
});

test.with({ mcp: true })("the server's row carries its launch summary", async ({
  page,
}) => {
  // Given the open MCP modal listing the staged server.
  await openMcp(page);
  const row = page.locator(sel.MCP_ROW_NAMED(MCP_SERVER));
  await expect(row).toBeVisible({ timeout: 15_000 });

  // Then the row shows the summary: the binary's path and its args.
  await expect(row.locator(".picker-description")).toContainText(
    "luau serve",
  );
});

test.with({ mcp: true })("clicking a disabled server enables it and the modal stays open", async ({
  page,
}) => {
  // Given the open MCP modal listing the staged server, disabled.
  await openMcp(page);
  const row = page.locator(sel.MCP_ROW_NAMED(MCP_SERVER));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row).toHaveAttribute("data-enabled", "false");

  // When clicking the row.
  await row.click();

  // Then it is marked enabled, and the modal is still shown.
  await expect(row).toHaveAttribute("data-enabled", "true");
  await expect(page.locator(sel.MCP_MODAL)).toBeVisible();
});

test.with({ mcp: true })("an enabled server's badge reaches running without a reload", async ({
  page,
}) => {
  // Given the open MCP modal, with the staged server just enabled.
  await openMcp(page);
  const row = page.locator(sel.MCP_ROW_NAMED(MCP_SERVER));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.click();
  await expect(row).toHaveAttribute("data-enabled", "true");

  // Then the badge moves to `running` on its own — no reload, no
  // re-open: the runtime cell's store pushes fresh rows to the open
  // modal.
  await expect(row).toHaveAttribute("data-badge", "running", {
    timeout: 30_000,
  });
});

test.with({ mcp: true })("clicking an enabled server disables it again", async ({
  page,
}) => {
  // Given the open MCP modal, with the staged server already enabled.
  await openMcp(page);
  const row = page.locator(sel.MCP_ROW_NAMED(MCP_SERVER));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.click();
  await expect(row).toHaveAttribute("data-enabled", "true");

  // When clicking the row again.
  await row.click();

  // Then it is marked disabled again, and reads `off` again.
  await expect(row).toHaveAttribute("data-enabled", "false");
  await expect(row).toHaveAttribute("data-badge", "off");
});

test.with({ mcp: true })("Escape closes the MCP modal", async ({ page }) => {
  // Given the open MCP modal, with its filter focused.
  await openMcp(page);

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the modal is gone.
  await expect(page.locator(sel.MCP_MODAL)).toBeHidden();
});

test.with({ mcp: true })("typing in the filter narrows the rows and marks the match", async ({
  page,
}) => {
  // Given the open MCP modal listing the staged server.
  await openMcp(page);
  const row = page.locator(sel.MCP_ROW_NAMED(MCP_SERVER));
  await expect(row).toBeVisible({ timeout: 15_000 });

  // When typing a filter the server's name matches, with real keypresses.
  await page.locator(sel.MCP_FILTER).click();
  await page.keyboard.type("e2e-ec");

  // Then the server is still listed with a matched run highlighted, and
  // the input keeps what was typed. The matcher is fuzzy over name and
  // launch summary together, so the exact survivors are not this
  // test's to pin.
  await expect(row).toBeVisible();
  await expect(row.locator("mark").first()).toBeVisible();
  await expect(page.locator(sel.MCP_FILTER)).toHaveValue("e2e-ec");
});
