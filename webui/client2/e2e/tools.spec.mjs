//! The hamburger menu's Tools item: the modal it opens, how a click
//! commits at once and the modal stays open, that dismissing does not
//! revert, and the filter. The tools themselves come from the session's
//! registry; here the rows' content is not under test beyond what a
//! click and a filter show.

import { expect, openMenu, openTools, sel, test } from "./fixture.mjs";

/** A built-in tool every fresh session's registry holds. */
const TOOL = "get_time";

test("the menu offers a Tools item while a session is active", async ({
  page,
}) => {
  // Given an open menu.
  await openMenu(page);

  // Then it lists Tools.
  await expect(page.locator(sel.MENU_ITEM_TOOLS)).toBeVisible();
});

test("the Tools item opens the modal listing the registry's tools", async ({
  page,
}) => {
  // Given the boot session, whose registry holds the built-in tools.
  // When opening the tools modal through the menu.
  await openTools(page);

  // Then the menu closed, and the modal lists a built-in tool —
  // enabled, as a fresh session with no filter permits everything.
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
  const row = page.locator(sel.TOOL_ROW_NAMED(TOOL));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row).toHaveAttribute("data-enabled", "true");
});

test("an enabled tool shows a check mark and a disabled one a cross", async ({
  page,
}) => {
  // Given the open tools modal listing a tool, enabled.
  await openTools(page);
  const row = page.locator(sel.TOOL_ROW_NAMED(TOOL));
  await expect(row).toBeVisible({ timeout: 15_000 });

  // When clicking the row.
  await row.click();

  // Then its marker reads the cross, where an enabled row reads the
  // check — the two glyphs the tools rows use for enabled and disabled.
  await expect(row).toHaveAttribute("data-enabled", "false");
  await expect(row.locator(".picker-marker")).toHaveText("\u2717");
  await row.click();
  await expect(row.locator(".picker-marker")).toHaveText("\u2713");
});

test("clicking an enabled tool disables it and the modal stays open", async ({
  page,
}) => {
  // Given the open tools modal listing a tool, enabled.
  await openTools(page);
  const row = page.locator(sel.TOOL_ROW_NAMED(TOOL));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row).toHaveAttribute("data-enabled", "true");

  // When clicking the row.
  await row.click();

  // Then it is marked disabled, and the modal is still shown.
  await expect(row).toHaveAttribute("data-enabled", "false");
  await expect(page.locator(sel.TOOLS_MODAL)).toBeVisible();
});

test("clicking a disabled tool enables it again", async ({ page }) => {
  // Given the open tools modal, with the tool already disabled.
  await openTools(page);
  const row = page.locator(sel.TOOL_ROW_NAMED(TOOL));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.click();
  await expect(row).toHaveAttribute("data-enabled", "false");

  // When clicking the row again.
  await row.click();

  // Then it is marked enabled again.
  await expect(row).toHaveAttribute("data-enabled", "true");
});

test("Escape closes the tools modal", async ({ page }) => {
  // Given the open tools modal, with its filter focused.
  await openTools(page);

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the modal is gone.
  await expect(page.locator(sel.TOOLS_MODAL)).toBeHidden();
});

test("dismissing with Escape keeps the change — there is no revert", async ({
  page,
}) => {
  // Given the open tools modal, with a tool just disabled.
  await openTools(page);
  const row = page.locator(sel.TOOL_ROW_NAMED(TOOL));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.click();
  await expect(row).toHaveAttribute("data-enabled", "false");

  // When dismissing with Escape, then reopening the modal.
  await page.keyboard.press("Escape");
  await expect(page.locator(sel.TOOLS_MODAL)).toBeHidden();
  await openTools(page);

  // Then the tool is still disabled: the click had already committed,
  // and dismissing undid nothing.
  await expect(row).toHaveAttribute("data-enabled", "false");
});

test("clicking the backdrop closes the tools modal", async ({ page }) => {
  // Given the open tools modal.
  await openTools(page);

  // When clicking the backdrop, outside the dialog.
  await page.locator(".modal-backdrop").click({ position: { x: 5, y: 5 } });

  // Then the modal is gone.
  await expect(page.locator(sel.TOOLS_MODAL)).toBeHidden();
});

test("typing in the filter narrows the rows and marks the match", async ({
  page,
}) => {
  // Given the open tools modal listing the registry's tools, and a note
  // of how many rows the unfiltered modal shows.
  await openTools(page);
  const row = page.locator(sel.TOOL_ROW_NAMED(TOOL));
  await expect(row).toBeVisible({ timeout: 15_000 });
  const unfiltered = await page.locator(sel.TOOL_ROW).count();

  // When typing a filter the tool's name matches, with real keypresses.
  await page.locator(sel.TOOLS_FILTER).click();
  await page.keyboard.type("get_ti");

  // Then the tool is still listed with a matched run highlighted, the
  // input keeps what was typed, and the list is narrower than before —
  // the matcher is fuzzy over name and description together, so a
  // description can legitimately match too, and the exact survivors
  // are not this test's to pin.
  await expect(row).toBeVisible();
  await expect(row.locator("mark").first()).toBeVisible();
  await expect(page.locator(sel.TOOLS_FILTER)).toHaveValue("get_ti");
  expect(await page.locator(sel.TOOL_ROW).count()).toBeLessThan(unfiltered);
});
