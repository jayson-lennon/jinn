//! The Archive picker as a tree, and its filter's matching.
//!
//! A forked session nests under its parent, indented by its depth, with
//! no connector characters — the tree is the terminal's, drawn this page's
//! way. The filter is the terminal's too: whitespace-separated terms are
//! ANDed, and a child's match keeps its parent in place above it.
//!
//! There is no fork gesture on `/web2`, so every test seeds its child row
//! straight into the scratch store under the active session, which the
//! test renames first so the pair is findable by title.

import { archiveSession, expect, seedChildSession, sel, test } from "./fixture.mjs";
import { restartJinn } from "./harness.mjs";

/** The parent's title — the active session every test starts on. */
const PARENT_TITLE = "the parent session";
/** The child's title. Its last word is the one a filter isolates. */
const CHILD_TITLE = "forked work on needles";

/**
 * Renames the active session to the parent's title and seeds one child
 * under it in the store, leaving the instance to be restarted.
 *
 * @param {import('@playwright/test').Page} page the test's own page.
 * @param {{ root: string }} jinn the test's own instance.
 * @returns {Promise<string>} the child's id.
 */
export async function arrangeParentAndChild(page, jinn) {
  await page.locator(sel.TITLE).click();
  await page.locator(sel.TITLE_EDIT).fill(PARENT_TITLE);
  await page.locator(sel.TITLE_EDIT).press("Enter");
  await expect(page.locator(sel.TITLE)).toHaveText(PARENT_TITLE);
  const parentId = await page
    .locator(sel.ACTIVE_SESSION_ROW)
    .getAttribute("data-session-id");
  const childId = seedChildSession(jinn, parentId, CHILD_TITLE);
  await restartJinn(jinn);
  await page.locator(sel.OPEN_PICKER).click();
  await expect(page.locator(sel.PICKER_ROW).filter({ hasText: PARENT_TITLE })).toBeVisible({
    timeout: 20_000,
  });
  return childId;
}

test("a forked session is indented under its parent", async ({ page, jinn }) => {
  // Given a child session seeded under the active one.
  await arrangeParentAndChild(page, jinn);

  // When the picker lists them unfiltered.
  const parent = page.locator(sel.PICKER_ROW).filter({ hasText: PARENT_TITLE });
  const child = page.locator(sel.PICKER_ROW).filter({ hasText: CHILD_TITLE });

  // Then the child is one level deeper and sits right after its parent.
  await expect(parent).toHaveAttribute("data-depth", "0");
  await expect(child).toHaveAttribute("data-depth", "1");
  await expect
    .poll(async () => {
      const rows = page.locator(sel.PICKER_ROW);
      const n = await rows.count();
      for (let i = 0; i < n; i += 1) {
        if ((await rows.nth(i).getAttribute("data-depth")) === "1") return i;
      }
      return -1;
    })
    .toBe(1);
  const parentPad = await parent.evaluate((el) => getComputedStyle(el).paddingLeft);
  const childPad = await child.evaluate((el) => getComputedStyle(el).paddingLeft);
  expect(parseFloat(childPad)).toBeGreaterThan(parseFloat(parentPad));
});

test("a child's match keeps its parent listed above it", async ({ page, jinn }) => {
  // Given the picker listing a parent and its child.
  await arrangeParentAndChild(page, jinn);

  // When filtering for a word only the child's title holds.
  await page.locator(sel.PICKER_FILTER).fill("needles");

  // Then the parent is still listed, unhighlighted, above the child.
  const rows = page.locator(sel.PICKER_ROW);
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toContainText(PARENT_TITLE);
  await expect(rows.nth(0).locator("mark")).toHaveCount(0);
  await expect(rows.nth(1)).toContainText(CHILD_TITLE);
  await expect(rows.nth(1).locator("mark")).toHaveText("needles");
});

test("the picker filter reads a space as two terms", async ({ page, jinn }) => {
  // Given the picker listing a parent and its child.
  await arrangeParentAndChild(page, jinn);

  // When filtering for two words of the child's title, separated by a space.
  await page.locator(sel.PICKER_FILTER).fill("forked needles");

  // Then the child matches, and its parent rides along above it.
  const rows = page.locator(sel.PICKER_ROW);
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(1)).toContainText(CHILD_TITLE);
});

test("an archived row is dimmed and carries no 'archived' word", async ({ page, jinn }) => {
  // Given a child session seeded under the active one, then archived in
  // the store before the instance restarts.
  await page.locator(sel.TITLE).click();
  await page.locator(sel.TITLE_EDIT).fill(PARENT_TITLE);
  await page.locator(sel.TITLE_EDIT).press("Enter");
  await expect(page.locator(sel.TITLE)).toHaveText(PARENT_TITLE);
  const parentId = await page
    .locator(sel.ACTIVE_SESSION_ROW)
    .getAttribute("data-session-id");
  const childId = seedChildSession(jinn, parentId, CHILD_TITLE);
  archiveSession(jinn, childId);
  await restartJinn(jinn);
  await page.locator(sel.OPEN_PICKER).click();

  // When the picker lists it.
  const child = page
    .locator(sel.PICKER_ROW_IN_STATE("archived"))
    .filter({ hasText: CHILD_TITLE });
  await expect(child).toBeVisible({ timeout: 20_000 });

  // Then the row names its state as data and dims, with no word for it.
  await expect(child).not.toContainText("archived");
  const parent = page.locator(sel.PICKER_ROW).filter({ hasText: PARENT_TITLE });
  const childColor = await child.evaluate((el) => getComputedStyle(el).color);
  const parentColor = await parent.evaluate((el) => getComputedStyle(el).color);
  expect(childColor).not.toBe(parentColor);
});
