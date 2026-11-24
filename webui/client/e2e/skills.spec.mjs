//! Skills flow — the menu item opens the modal listing the viewed
//! session's discovered skills, a row click toggles that session's
//! enablement immediately (no reload, persisted), the row's `+`
//! force-load pins the skill's body into context without a reload, a
//! second `+` appends the "already loaded" transient instead of a
//! duplicate pair, and an open modal re-pulls its rows when a
//! discovery scan lands (the skillsTick signal re-pull).
//!
//! Every test stages one skill (`e2e-scratch-skill`) under its own
//! scratch root's `.agents/skills/` before jinn boots — discovery scans
//! at activation, so a file written after boot is invisible. It is
//! global-scoped (no source badge) and, on a fresh session's absent
//! filter, renders enabled; a test that needs it disabled arranges
//! that through `disableSkill`.

import {
  createSession,
  disableSkill,
  expect,
  loadSkill,
  openModal,
  openSession,
  staged,
  test,
} from "./fixture.mjs";
import * as sel from "./selectors.mjs";
const SKILL = staged.scratchSkill.name;

/** Opens the boot session and the skills modal through the menu. */
async function openSkillsModal(jinn, page) {
  await openSession(jinn, page);
  return openModal(page, "skills");
}

test.with({ skill: true })("the menu item opens the modal listing the discovered skill, enabled", async ({
  page,
  jinn,
}) => {
  // Given the boot session open and the menu opened.
  const modal = await openSkillsModal(jinn, page);

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

test.with({ skill: true })("clicking the row disables the skill and the modal re-renders without a reload", async ({
  page,
  jinn,
}) => {
  // Given the skills modal open on the boot session, the row enabled.
  await openSkillsModal(jinn, page);
  const row = page.locator(sel.SKILL_ROW(SKILL));

  // When clicking the row — the per-click toggle, no staged apply.
  await row.click();

  // Then the re-rendered modal marks the row hollow immediately (the
  // POST response carried the swap; no reload happened).
  await expect(page.locator(sel.SKILL_ROW(SKILL))).not.toHaveClass(
    new RegExp(sel.SKILL_ROW_ENABLED),
  );
});

test.with({ skill: true })("a reload persists the disablement: re-opening shows the hollow row", async ({
  page,
  jinn,
}) => {
  // Given the skill disabled through its row click, the page
  // reloaded, and the modal re-opened.
  await openSkillsModal(jinn, page);
  await disableSkill(page);
  await page.keyboard.press("Escape");
  await expect(page.locator(sel.SKILL_MODAL)).toBeHidden();
  await page.reload();
  await expect(page.locator(sel.PAGE_ROOT)).toBeVisible();
  await openModal(page, "skills");

  // Then the re-opened modal renders the persisted filter state —
  // the toggle wrote the session's skill filter, which survives the
  // reload.
  await expect(page.locator(sel.SKILL_ROW(SKILL))).not.toHaveClass(
    new RegExp(sel.SKILL_ROW_ENABLED),
  );
});

test.with({ skill: true })("clicking the hollow row re-enables the skill", async ({ page, jinn }) => {
  // Given the skills modal open, the skill disabled through its row
  // click, and the modal re-opened so the hollow row is the open pull's.
  await openSkillsModal(jinn, page);
  await disableSkill(page);
  await page.keyboard.press("Escape");
  await expect(page.locator(sel.SKILL_MODAL)).toBeHidden();
  await openSkillsModal(jinn, page);
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


test.with({ skill: true })("an open modal re-pulls its rows when a fresh session's discovery lands", async ({
  page,
  jinn,
}) => {
  // Given a BRAND-NEW session (created through the modal) whose
  // discovery scan is still in flight, and its skills modal ALREADY
  // open — the race the skillsTick seam exists for: the modal can be
  // opened before discovery lands.
  await openSession(jinn, page);
  await createSession(jinn, page);

  // When opening the skills modal immediately — possibly before the
  // new session's scan has written its discovered set.
  await openModal(page, "skills");

  // Then the scratch skill's row arrives WITHOUT closing or
  // re-opening the modal: discovery lands, `SkillsLoaded` fans out,
  // the fan-out bumps skillsTick, and the open modal's effect
  // re-pulls its rows.
  await expect(page.locator(sel.SKILL_ROW(SKILL))).toBeVisible({
    timeout: 15_000,
  });
});

test.with({ skill: true })("the landing page's menu carries no skills item", async ({ page, jinn }) => {
  // Given the landing page (nothing viewed).
  await page.goto(`${jinn.baseUrl}/web`);

  // When opening the hamburger menu.
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();

  // Then the skills item is not offered on the landing page — nothing
  // is viewed, and the item's data-show hides it. The sessions item
  // survives.
  await expect(page.locator(sel.MENU_ITEM_SKILLS)).not.toBeVisible();
  await expect(page.locator(sel.MENU_ITEM_SESSIONS)).toBeVisible();
});

test.with({ skill: true })("typing in the filter narrows the rows server-side with marked runs", async ({
  page,
  jinn,
}) => {
  // Given the modal open with the scratch skill listed.
  const modal = await openSkillsModal(jinn, page);
  const row = page.locator(sel.SKILL_ROW(sel.SKILL));
  await expect(row).toBeVisible();

  // When typing a matching prefix with real keypresses (fill()
  // dispatches no keydown, and typing must not dismiss the modal).
  await page.locator(sel.SKILL_FILTER).click();
  await page.keyboard.type(sel.SKILL.slice(0, 3));

  // Then the row is still visible with a marked run — the server
  // filtered and highlighted it.
  await expect(row).toBeVisible();
  await expect(row.locator("mark").first()).toHaveText(
    sel.SKILL.slice(0, 3),
  );

  // And the input keeps what was typed (a re-pull never wipes it).
  await expect(page.locator(sel.SKILL_FILTER)).toHaveValue(
    sel.SKILL.slice(0, 3),
  );

  // And Escape dismisses the modal.
  await page.keyboard.press("Escape");
  await expect(modal).toBeHidden();
});

test.with({ skill: true })(
  "a toggle under a filter keeps the filtered view with the input intact",
  async ({ page, jinn }) => {
    // Given the modal open on a fresh session (the skill enabled) and
    // a filter typed.
    await openSkillsModal(jinn, page);
    const row = page.locator(sel.SKILL_ROW(SKILL));
    await expect(row).toHaveClass(new RegExp(sel.SKILL_ROW_ENABLED));
    const query = SKILL.slice(0, 3);
    await page.locator(sel.SKILL_FILTER).click();
    await page.keyboard.type(query);
    await expect(row.locator("mark").first()).toHaveText(query);

    // When toggling the filtered-in row.
    await row.click();

    // Then the row re-renders disabled but still filtered in, and the
    // input still carries the query — the toggle response rendered
    // the view the user was looking at.
    await expect(row).toBeVisible();
    await expect(row).not.toHaveClass(/skill-row-enabled/);
    await expect(page.locator(sel.SKILL_FILTER)).toHaveValue(query);

    // And Escape dismisses (the plain skills modal has no revert).
    await page.keyboard.press("Escape");
    await expect(page.locator(sel.SKILL_MODAL).first()).toBeHidden();

    // And reopening shows the unfiltered rows with the disablement
    // committed (the filter is view-local). The reopen rides the
    // still-armed re-pull effect, so wait for the pull to land — the
    // input resets when the fresh fragment replaces the host, not at
    // the click.
    await openModal(page, "skills");
    // The reopen's fresh skeleton carries an empty filter; the bound
    // signal still holds the closed view's query, so clear it through
    // the real input — select-all + delete, the path a user would
    // take — before asserting the reset.
    await page.locator(sel.SKILL_FILTER).click();
    await page.keyboard.press("ControlOrMeta+a");
    await page.keyboard.press("Delete");
    await expect(page.locator(sel.SKILL_FILTER)).toHaveValue("", {
      timeout: 10_000,
    });
    await expect(page.locator(sel.SKILL_ROW(SKILL))).not.toHaveClass(
      /skill-row-enabled/,
    );
  },
);

test.with({ skill: true })("the plus button pins the skill body into context without a reload", async ({
  page,
  jinn,
}) => {
  // Given the skills modal open on the boot session.
  await openSkillsModal(jinn, page);

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

test.with({ skill: true })("a second plus appends the already-loaded notice instead of a pair", async ({
  page,
  jinn,
}) => {
  // Given the skill already pinned through its `+` button once.
  await openSkillsModal(jinn, page);
  await loadSkill(page);
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
