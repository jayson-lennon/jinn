//! The session rail and the Archive picker as trees, with the picker
//! filter's matching and the kind lines that name a subagent's or a bot's
//! row.
//!
//! A forked session nests under its parent, indented by its depth, with no
//! connector characters — the tree is the terminal's, drawn this page's way.
//! A subagent's or a bot's row adds a kind line under its title, in the
//! kind's colour, and the viewed session's kind is named above the composer.
//!
//! There is no fork gesture on `/web2`, so every test seeds its child row
//! straight into the scratch store under the active session, which the
//! test renames first so the pair is findable by title.

import {
  archiveSession,
  expect,
  openSession,
  seedChildSession,
  sel,
  startBusyTurn,
  test,
} from "./fixture.mjs";
import { restartJinn } from "./harness.mjs";

/** The parent's title — the active session every test starts on. */
const PARENT_TITLE = "the parent session";
/** The child's title. Its last word is the one a filter isolates. */
const CHILD_TITLE = "forked work on needles";
/** A subagent's kind colour — the terminal's `subagent_fg`. */
const SUBAGENT = "rgb(178, 158, 240)";
/** A bot's kind colour — the terminal's `bot_fg`. */
const BOT = "rgb(255, 105, 180)";

/**
 * Renames the active session to the parent's title and seeds one child
 * under it in the store, leaving the instance to be restarted.
 *
 * @param {import('@playwright/test').Page} page the test's own page.
 * @param {{ root: string }} jinn the test's own instance.
 * @param {"fork" | "subagent" | "bot"} origin the child's kind.
 * @returns {Promise<string>} the child's id.
 */
export async function arrangeParentAndChild(page, jinn, origin = "fork") {
  await page.locator(sel.TITLE).click();
  await page.locator(sel.TITLE_EDIT).fill(PARENT_TITLE);
  await page.locator(sel.TITLE_EDIT).press("Enter");
  await expect(page.locator(sel.TITLE)).toHaveText(PARENT_TITLE);
  const parentId = await page
    .locator(sel.ACTIVE_SESSION_ROW)
    .getAttribute("data-session-id");
  const childId = seedChildSession(jinn, parentId, CHILD_TITLE, origin);
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

/**
 * Renames the active session to the parent's title, seeds one child of the
 * given kind under it in the store, and restarts the instance — the rail's
 * arrangement. It opens no modal, so the rail itself is what a test reads.
 *
 * @param {import('@playwright/test').Page} page the test's own page.
 * @param {{ root: string }} jinn the test's own instance.
 * @param {"fork" | "subagent" | "bot"} origin the child's kind.
 * @returns {Promise<{ childId: string, parentId: string }>} the pair's ids.
 */
export async function arrangeRail(page, jinn, origin) {
  await page.locator(sel.TITLE).click();
  await page.locator(sel.TITLE_EDIT).fill(PARENT_TITLE);
  await page.locator(sel.TITLE_EDIT).press("Enter");
  await expect(page.locator(sel.TITLE)).toHaveText(PARENT_TITLE);
  const parentId = await page
    .locator(sel.ACTIVE_SESSION_ROW)
    .getAttribute("data-session-id");
  const childId = seedChildSession(jinn, parentId, CHILD_TITLE, origin);
  await restartJinn(jinn);
  await expect(page.locator(sel.ROW_WRAP(childId))).toBeVisible({ timeout: 20_000 });
  return { childId, parentId };
}

/** A row's kind line, by session id. */
const kindOf = (page, id) => page.locator(`${sel.ROW_WRAP(id)} [data-testid="session-kind"]`);

test("a forked session is indented under its parent in the rail", async ({ page, jinn }) => {
  // Given a fork seeded under the active session.
  const { childId, parentId } = await arrangeRail(page, jinn, "fork");

  // When the rail lists them.
  // Then the child is one level deeper than its parent.
  await expect(page.locator(sel.ROW_WRAP(parentId))).toHaveAttribute("data-depth", "0");
  await expect(page.locator(sel.ROW_WRAP(childId))).toHaveAttribute("data-depth", "1");
  const parentPad = await page
    .locator(sel.ROW_WRAP(parentId))
    .evaluate((el) => getComputedStyle(el).paddingLeft);
  const childPad = await page
    .locator(sel.ROW_WRAP(childId))
    .evaluate((el) => getComputedStyle(el).paddingLeft);
  expect(parseFloat(childPad)).toBeGreaterThan(parseFloat(parentPad));
});

test("a child is listed right after its parent in the rail", async ({ page, jinn }) => {
  // Given a fork seeded under the active session.
  const { childId, parentId } = await arrangeRail(page, jinn, "fork");

  // When the rail's rows are read top to bottom.
  /** The index of the row carrying the given id, or -1. */
  const indexOf = (id) =>
    page
      .locator('[data-testid="session-row-wrap"]')
      .evaluateAll((rows, want) => rows.findIndex((row) => row.dataset.sessionId === want), id);
  await expect.poll(async () => indexOf(childId)).toBeGreaterThanOrEqual(0);
  const childAt = await indexOf(childId);
  const parentAt = await indexOf(parentId);

  // Then the parent is the row directly above it.
  expect(childAt).toBe(parentAt + 1);
});

test("a subagent row names its kind under the title", async ({ page, jinn }) => {
  // Given a subagent seeded under the active session.
  const { childId } = await arrangeRail(page, jinn, "subagent");

  // When the rail lists it.
  // Then its row carries a `subagent` line in the subagent colour.
  await expect(kindOf(page, childId)).toHaveText("subagent");
  expect(await kindOf(page, childId).evaluate((el) => getComputedStyle(el).color)).toBe(SUBAGENT);
});

test("a bot row names its kind under the title", async ({ page, jinn }) => {
  // Given a bot seeded under the active session.
  const { childId } = await arrangeRail(page, jinn, "bot");

  // When the rail lists it.
  // Then its row carries a `bot` line in the bot colour.
  await expect(kindOf(page, childId)).toHaveText("bot");
  expect(await kindOf(page, childId).evaluate((el) => getComputedStyle(el).color)).toBe(BOT);
});

test("a fork's row has no kind line and its title keeps its colour", async ({ page, jinn }) => {
  // Given a fork seeded under the active session.
  const { childId } = await arrangeRail(page, jinn, "fork");

  // When the rail lists it.
  // Then the row is one line: no kind line, and the title is not tinted.
  await expect(kindOf(page, childId)).toHaveCount(0);
  const titleColor = await page
    .locator(`${sel.ROW_WRAP(childId)} .session-title`)
    .evaluate((el) => getComputedStyle(el).color);
  expect(titleColor).not.toBe(SUBAGENT);
  expect(titleColor).not.toBe(BOT);
});

test("a row's title is drawn, not collapsed", async ({ page, jinn }) => {
  // Given a parent with an ordinary title, and a subagent under it — the
  // row shape that stacks a title above a kind line.
  const { childId, parentId } = await arrangeRail(page, jinn, "subagent");

  // When each title's box is measured.
  /** A title's rendered height in CSS pixels. */
  const titleHeight = (id) =>
    page
      .locator(`${sel.ROW_WRAP(id)} .session-title`)
      .evaluate((el) => el.getBoundingClientRect().height);

  // Then both titles have real height: the plain row's and the kind row's.
  // A title collapsed to zero here is how "the titles are missing, only the
  // kind lines show" presented.
  expect(await titleHeight(parentId)).toBeGreaterThan(10);
  expect(await titleHeight(childId)).toBeGreaterThan(10);
});

test("the viewed subagent session is named above the composer", async ({ page, jinn }) => {
  // Given a subagent seeded under the active session, and now viewed.
  const { childId } = await arrangeRail(page, jinn, "subagent");
  await openSession(page, childId, CHILD_TITLE);

  // When the composer's status row is read.
  const kind = page.locator('[data-testid="status-kind"]');

  // Then it names the subagent, right-aligned, in the subagent colour.
  await expect(kind).toHaveText("[subagent]");
  expect(await kind.evaluate((el) => getComputedStyle(el).color)).toBe(SUBAGENT);
  const rightEdges = await page
    .locator('[data-testid="session-status"]')
    .evaluate((row) => {
      const label = row.querySelector('[data-testid="status-kind"]');
      return [row.getBoundingClientRect().right, label.getBoundingClientRect().right];
    });
  expect(Math.abs(rightEdges[0] - rightEdges[1])).toBeLessThan(20);
});

test("the viewed bot session is named above the composer", async ({ page, jinn }) => {
  // Given a bot seeded under the active session, and now viewed.
  const { childId } = await arrangeRail(page, jinn, "bot");
  await openSession(page, childId, CHILD_TITLE);

  // When the composer's status row is read.
  const kind = page.locator('[data-testid="status-kind"]');

  // Then it names the bot, in the bot colour.
  await expect(kind).toHaveText("[bot]");
  expect(await kind.evaluate((el) => getComputedStyle(el).color)).toBe(BOT);
});

test("an ordinary idle session shows an empty status row", async ({ page }) => {
  // Given a fresh instance on its first session, idle.
  // When the page has synced.
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toBeVisible();

  // Then the row above the composer is there — it holds the transcript's
  // layout still — but says nothing: no busy text and no kind label.
  const row = page.locator('[data-testid="session-status"]');
  await expect(row).toHaveCount(1);
  await expect(page.locator('[data-testid="status-text"]')).toHaveCount(0);
  await expect(page.locator('[data-testid="status-kind"]')).toHaveCount(0);
});

test("a busy session shows the working text above the composer", async ({ page }) => {
  // Given an ordinary session.
  // When a turn starts running in it.
  await startBusyTurn(page);

  // Then the status row names the work at its left edge.
  const text = page.locator('[data-testid="status-text"]');
  await expect(text).toBeVisible();
  expect(["Working...", "Streaming..."]).toContain(await text.innerText());
});
