//! Skills flow — the menu item opens the modal listing the viewed
//! session's discovered skills, a row click toggles that session's
//! enablement immediately (no reload, persisted), the row's `+`
//! force-load pins the skill's body into context without a reload, a
//! second `+` appends the "already loaded" transient instead of a
//! duplicate pair, and an open modal re-pulls its rows when a
//! discovery scan lands (the skillsTick signal re-pull).
//!
//! The scratch tier stages one skill (`e2e-scratch-skill`) under
//! `<root>/.agents/skills/` before jinn boots — discovery scans at
//! activation, so a file written after boot is invisible. It is
//! global-scoped (no source badge) and, on the boot session's absent
//! filter, renders enabled.
//!
//! The `z` prefix is load-bearing: the tier's specs run in
//! alphabetical file order against ONE scratch jinn (serial workers,
//! shared db), and this spec's last test CREATES a second session —
//! the single-boot-session pins in `smoke`/`mobile` must have run
//! before it.

import { expect, test } from "@playwright/test";
import { openSession } from "./nav.mjs";
import { baseUrl } from "./run.mjs";
import * as sel from "./selectors.mjs";
const { SKILL } = sel;

/** Opens the session page and the skills modal through the menu. */
async function openSkillsModal(page) {
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();
  await page.locator(sel.MENU_ITEM_SKILLS).click();
  const modal = page.locator(sel.SKILL_MODAL);
  await expect(modal).toBeVisible();
  return modal;
}

test("the menu item opens the modal listing the discovered skill, enabled", async ({
  page,
}) => {
  // Given the boot session open and the menu opened.
  const modal = await openSkillsModal(page);

  // Then the modal renders inside its own host span (never the
  // menu's container), and the scratch skill renders as one row —
  // enabled (the boot session's filter is absent, which permits all)
  // with no source badge (global skills badge nothing, the TUI rule)
  // and the description from its frontmatter.
  const host = page.locator(sel.SKILL_MODAL_HOST);
  await expect(host.locator(sel.SKILL_MODAL)).toHaveCount(1);
  const row = page.locator(sel.SKILL_ROW(SKILL));
  await expect(row).toBeVisible();
  await expect(row).toHaveClass(new RegExp(sel.SKILL_ROW_ENABLED));
  await expect(row).not.toContainText("skill-source-project");
  await expect(modal.locator(sel.SKILL_ROWS)).toContainText(
    "scratch skill for the skills modal flow",
  );
});

test("clicking the row disables the skill and the modal re-renders without a reload", async ({
  page,
}) => {
  // Given the skills modal open on the boot session, the row enabled.
  await openSkillsModal(page);
  const row = page.locator(sel.SKILL_ROW(SKILL));

  // When clicking the row — the per-click toggle, no staged apply.
  await row.click();

  // Then the re-rendered modal marks the row hollow immediately (the
  // POST response carried the swap; no reload happened).
  await expect(page.locator(sel.SKILL_ROW(SKILL))).not.toHaveClass(
    new RegExp(sel.SKILL_ROW_ENABLED),
  );
});

test("a reload persists the disablement: re-opening shows the hollow row", async ({
  page,
}) => {
  // Given the disabled boot session (the previous spec toggled it
  // off; the tier runs serially against one scratch jinn).
  await openSkillsModal(page);

  // Then the re-opened modal renders the persisted filter state —
  // the toggle wrote the session's skill filter, which survives the
  // reload.
  await expect(page.locator(sel.SKILL_ROW(SKILL))).not.toHaveClass(
    new RegExp(sel.SKILL_ROW_ENABLED),
  );
});

test("clicking the hollow row re-enables the skill", async ({ page }) => {
  // Given the skills modal open with the skill still disabled from
  // the earlier specs.
  await openSkillsModal(page);
  const row = page.locator(sel.SKILL_ROW(SKILL));
  await expect(row).not.toHaveClass(new RegExp(sel.SKILL_ROW_ENABLED));

  // When clicking the row again.
  await row.click();

  // Then the re-rendered modal marks the row enabled — the toggle is
  // mode-preserving arithmetic on the filter, and the session is
  // back where it started.
  await expect(page.locator(sel.SKILL_ROW(SKILL))).toHaveClass(
    new RegExp(sel.SKILL_ROW_ENABLED),
  );
});

test("the plus button pins the skill body into context without a reload", async ({
  page,
}) => {
  // Given the skills modal open on the boot session.
  await openSkillsModal(page);

  // When clicking the row's force-load button.
  await page.locator(`${sel.SKILL_ROW(SKILL)} ${sel.SKILL_LOAD_BUTTON}`).click();

  // Then the transcript swap carries the pinned pair — the tool call
  // naming the `skill` tool and the wrapped body result — visible
  // without a reload (the POST response carried the swap).
  const transcript = page.locator(sel.TRANSCRIPT);
  await expect(transcript).toContainText("e2e-scratch-skill-body-marker", {
    timeout: 10_000,
  });
});

test("a second plus appends the already-loaded notice instead of a pair", async ({
  page,
}) => {
  // Given the loaded boot session (the previous spec pinned the
  // skill; the tier runs serially).
  await openSkillsModal(page);
  const transcript = page.locator(sel.TRANSCRIPT);
  const callsBefore = await transcript
    .locator("article.entry.tool_call")
    .count();

  // When clicking the force-load button again.
  await page.locator(`${sel.SKILL_ROW(SKILL)} ${sel.SKILL_LOAD_BUTTON}`).click();

  // Then NO second pair appends, and the muted "already loaded"
  // notice lands in the re-rendered transcript instead.
  await expect(transcript).toContainText(
    "Skill 'e2e-scratch-skill' is already loaded",
    { timeout: 10_000 },
  );
  expect(await transcript.locator("article.entry.tool_call").count()).toBe(
    callsBefore,
  );
});

test("an open modal re-pulls its rows when a fresh session's discovery lands", async ({
  page,
}) => {
  // Given a BRAND-NEW session (created through the modal, per the
  // znew-session flow) whose discovery scan is still in flight, and
  // its skills modal ALREADY open — the race the skillsTick seam
  // exists for: the modal can be opened before discovery lands.
  await openSession(page);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  const modal = page.locator(sel.SESSION_NEW_MODAL);
  await expect(modal).toBeVisible();
  await page.locator(sel.SESSION_NEW_SUBMIT).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();

  // When opening the skills modal immediately — possibly before the
  // new session's scan has written its discovered set.
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_SKILLS).click();
  await expect(page.locator(sel.SKILL_MODAL)).toBeVisible();

  // Then the scratch skill's row arrives WITHOUT closing or
  // re-opening the modal: discovery lands, `SkillsLoaded` fans out,
  // the fan-out bumps skillsTick, and the open modal's effect
  // re-pulls its rows.
  await expect(page.locator(sel.SKILL_ROW(SKILL))).toBeVisible({
    timeout: 15_000,
  });
});

test("the landing page's menu carries no skills item", async ({ page }) => {
  // Given the landing page (nothing viewed).
  await page.goto(`${baseUrl()}/web`);

  // When opening the hamburger menu.
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();

  // Then the skills item is absent from the markup — the server
  // omitted it — and the sessions item survives.
  await expect(page.locator(sel.MENU_ITEM_SKILLS)).toHaveCount(0);
  await expect(page.locator(sel.MENU_ITEM_SESSIONS)).toBeVisible();
});
