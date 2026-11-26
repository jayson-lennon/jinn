//! The hamburger menu's shell: where the button sits, how the panel
//! opens, and the three ways it closes. The items themselves belong to
//! the features that supply them; here the panel's content is not under
//! test, only the chrome.

import {
  expect,
  openMenu,
  openSkills,
  SKILL_FILTER,
  SKILL_NAME,
  sel,
  test,
} from "./fixture.mjs";

test("the hamburger sits in the title bar, left of the title", async ({ page }) => {
  // Given a connected page.
  // Then the menu button is the title bar's first control, and the
  // title sits to its right.
  const bar = page.locator(".title-bar");
  const button = page.locator(sel.MENU_BUTTON);
  await expect(button).toBeVisible();
  await expect(bar.locator("> *").first()).toHaveAttribute(
    "class",
    new RegExp("menu"),
  );
  const [buttonBox, titleBox] = await Promise.all([
    button.boundingBox(),
    page.locator(sel.TITLE).boundingBox(),
  ]);
  expect(buttonBox.x).toBeLessThan(titleBox.x);
});

test("the menu is closed on a fresh page", async ({ page }) => {
  // Given a page that has just connected.
  // Then the panel is not visible and the button says it is collapsed.
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
  await expect(page.locator(sel.MENU_BUTTON)).toHaveAttribute(
    "aria-expanded",
    "false",
  );
});

test("clicking the hamburger opens the panel", async ({ page }) => {
  // Given a closed menu.
  // When clicking the hamburger.
  await page.locator(sel.MENU_BUTTON).click();

  // Then the panel is visible, the scrim is behind it, and the button
  // says the menu is open.
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();
  await expect(page.locator(sel.MENU_SCRIM)).toBeVisible();
  await expect(page.locator(sel.MENU_BUTTON)).toHaveAttribute(
    "aria-expanded",
    "true",
  );
});

test("clicking the scrim closes the menu", async ({ page }) => {
  // Given an open menu.
  await openMenu(page);

  // When clicking the scrim, well away from the panel.
  await page.locator(sel.MENU_SCRIM).click({ position: { x: 900, y: 400 } });

  // Then the panel is gone.
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
});

test("Escape closes the menu", async ({ page }) => {
  // Given an open menu.
  await openMenu(page);

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the panel is gone.
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
});

test.with({ skill: true })("the menu offers a Skills item while a session is active", async ({
  page,
}) => {
  // Given an open menu.
  await openMenu(page);

  // Then it lists Skills.
  await expect(page.locator(sel.MENU_ITEM_SKILLS)).toBeVisible();
});

test.with({ skill: true })("the Skills item opens the modal listing the discovered skill", async ({
  page,
}) => {
  // Given the boot session, which discovered the staged skill.
  // When opening the skills modal through the menu.
  await openSkills(page);

  // Then the menu closed, and the modal lists the skill — enabled, as a
  // fresh session with no filter permits everything — with no source
  // badge (the skill is staged in the scratch root's global skills dir,
  // and global skills badge nothing), and the description from its
  // frontmatter.
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
  const row = page.locator(sel.SKILL_ROW_NAMED(SKILL_NAME));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row).toHaveAttribute("data-enabled", "true");
  await expect(row).toHaveAttribute("data-loaded", "false");
  await expect(row.locator(".skill-row-source")).toHaveCount(0);
  await expect(row).toContainText("scratch skill for the webui2 skills modal");
});

test.with({ skill: true })("Escape closes the skills modal", async ({ page }) => {
  // Given the open skills modal, with its filter focused.
  await openSkills(page);

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the modal is gone.
  await expect(page.locator(sel.SKILLS_MODAL)).toBeHidden();
});

test.with({ skill: true })("clicking the backdrop closes the skills modal", async ({ page }) => {
  // Given the open skills modal.
  await openSkills(page);

  // When clicking the backdrop, outside the dialog.
  await page.locator(".modal-backdrop").click({ position: { x: 5, y: 5 } });

  // Then the modal is gone.
  await expect(page.locator(sel.SKILLS_MODAL)).toBeHidden();
});

test.with({ skill: true })("typing in the filter narrows the rows and marks the match", async ({
  page,
}) => {
  // Given the open skills modal listing the staged skill.
  await openSkills(page);
  const row = page.locator(sel.SKILL_ROW_NAMED(SKILL_NAME));
  await expect(row).toBeVisible({ timeout: 15_000 });

  // When typing a filter the skill's name matches, with real keypresses.
  await page.locator(sel.SKILLS_FILTER).click();
  await page.keyboard.type(SKILL_FILTER);

  // Then the row is still listed with its matched run highlighted, the
  // input keeps what was typed, and no row that fails the filter remains.
  await expect(row).toBeVisible();
  await expect(row.locator("mark").first()).toHaveText("e2e-scra");
  await expect(page.locator(sel.SKILLS_FILTER)).toHaveValue(SKILL_FILTER);
  await expect(page.locator(sel.SKILL_ROW)).toHaveCount(1);
});

test.with({ skill: true })("clicking an enabled skill disables it", async ({ page }) => {
  // Given the open skills modal listing the staged skill, enabled.
  await openSkills(page);
  const row = page.locator(sel.SKILL_ROW_NAMED(SKILL_NAME));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row).toHaveAttribute("data-enabled", "true");

  // When clicking the row's toggle.
  await row.locator("[data-testid=\"skill-toggle\"]").click();

  // Then it is marked disabled, and the modal stays open.
  await expect(row).toHaveAttribute("data-enabled", "false");
  await expect(page.locator(sel.SKILLS_MODAL)).toBeVisible();
});

test.with({ skill: true })("clicking a disabled skill enables it again", async ({ page }) => {
  // Given the open skills modal, with the staged skill already disabled.
  await openSkills(page);
  const row = page.locator(sel.SKILL_ROW_NAMED(SKILL_NAME));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.locator("[data-testid=\"skill-toggle\"]").click();
  await expect(row).toHaveAttribute("data-enabled", "false");

  // When clicking the row's toggle again.
  await row.locator("[data-testid=\"skill-toggle\"]").click();

  // Then it is marked enabled again.
  await expect(row).toHaveAttribute("data-enabled", "true");
});

test.with({ skill: true })("the + button loads the skill into the transcript", async ({
  page,
}) => {
  // Given the open skills modal listing the staged skill, with an empty
  // transcript.
  await openSkills(page);
  const row = page.locator(sel.SKILL_ROW_NAMED(SKILL_NAME));
  await expect(row).toBeVisible({ timeout: 15_000 });

  // When clicking the row's + button.
  await row.locator("[data-testid=\"skill-load\"]").click();

  // Then the transcript shows the skill's call and its result, and the
  // modal stays open with the row now marked loaded.
  const result = page
    .locator(`${sel.TRANSCRIPT} ${sel.ENTRY("tool_result")}`)
    .filter({ hasText: SKILL_NAME });
  await expect(result).toBeVisible();
  await expect(result.locator("summary")).toContainText("success");
  await expect(page.locator(sel.SKILLS_MODAL)).toBeVisible();
  await expect(row).toHaveAttribute("data-loaded", "true");
});

test.with({ skill: true })("a loaded skill's toggle is disabled, so it cannot be switched off", async ({
  page,
}) => {
  // Given the staged skill loaded into the session.
  await openSkills(page);
  const row = page.locator(sel.SKILL_ROW_NAMED(SKILL_NAME));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.locator("[data-testid=\"skill-load\"]").click();
  await expect(row).toHaveAttribute("data-loaded", "true");

  // Then the toggle is disabled — a loaded skill's body stays pinned in
  // history, so switching it off would promise an unload that never
  // happens.
  await expect(row.locator("[data-testid=\"skill-toggle\"]")).toBeDisabled();
});

test.with({ skill: true })("a loaded skill's + button is disabled, so it cannot be loaded twice", async ({
  page,
}) => {
  // Given the staged skill loaded into the session.
  await openSkills(page);
  const row = page.locator(sel.SKILL_ROW_NAMED(SKILL_NAME));
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.locator("[data-testid=\"skill-load\"]").click();
  await expect(row).toHaveAttribute("data-loaded", "true");

  // Then the + button is disabled — the page offers no way to pin the
  // same body twice. The server still guards a repeat frame on its own.
  await expect(row.locator("[data-testid=\"skill-load\"]")).toBeDisabled();
});
