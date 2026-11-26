//! The test fixture for the webui2 e2e tier.
//!
//! `test` boots one jinn per test — its own scratch database, config
//! and data tree — opens a page on `/web2`, waits for its socket to
//! connect and its first snapshot to land, and tears the instance down
//! when the test ends, however it ends. A spec imports `test` and
//! `expect` from here, never from `@playwright/test`.
//!
//! The setup verbs reach a starting state; a spec never re-derives
//! them. Helpers that only *assert* stay in the spec that asserts them.

import { test as base, expect } from "@playwright/test";
import { execFileSync } from "node:child_process";
import path from "node:path";
import { bootJinn, restartJinn, scratchSkill, teardownJinn } from "./harness.mjs";

/** How long a reply from a running jinn may take. */
const REPLY_TIMEOUT_MS = 15_000;

/** The selectors every spec shares. */
export const sel = {
  OVERLAY: '[data-testid="overlay"]',
  TRANSCRIPT: '[data-testid="transcript"]',
  ENTRY: (kind) => `.entry-${kind}`,
  DRAFT: '[data-testid="draft"]',
  SEND: '[data-testid="send"]',
  QUEUE: '[data-testid="queue"]',
  CANCEL: '[data-testid="cancel"]',
  COMPOSER_ERROR: '[data-testid="composer-error"]',
  TITLE: '[data-testid="title"]',
  TITLE_EDIT: '[data-testid="title-edit"]',
  MODEL_CHIP: '[data-testid="model-chip"]',
  MODEL_ROW: '[data-testid="model-row"]',
  MODEL_FILTER: '[data-testid="model-filter"]',
  OPEN_PICKER: '[data-testid="open-picker"]',
  PICKER_ROW: '[data-testid="picker-row"]',
  MENU_BUTTON: '[data-testid="menu-button"]',
  MENU_PANEL: '[data-testid="menu-panel"]',
  MENU_SCRIM: '[data-testid="menu-scrim"]',
  MENU_ITEM_SKILLS: '[data-testid="menu-item-skills"]',
  MENU_ITEM_PERSONA: '[data-testid="menu-item-persona"]',
  MENU_ITEM_TOOLS: '[data-testid="menu-item-tools"]',
  MENU_ITEM_MCP: '[data-testid="menu-item-mcp"]',
  SKILLS_MODAL: '[data-testid="skills-modal"]',
  SKILLS_FILTER: '[data-testid="skills-filter"]',
  SKILL_ROW: '[data-testid="skill-row"]',
  /** One skill row, by its skill name (`data-skill`). */
  SKILL_ROW_NAMED: (name) => `[data-testid="skill-row"][data-skill="${name}"]`,
  TOOLS_MODAL: '[data-testid="tools-modal"]',
  TOOLS_FILTER: '[data-testid="tools-filter"]',
  TOOL_ROW: '[data-testid="tool-row"]',
  /** One tool row, by its tool name (`data-tool`). */
  TOOL_ROW_NAMED: (name) => `[data-testid="tool-row"][data-tool="${name}"]`,
  MCP_MODAL: '[data-testid="mcp-modal"]',
  MCP_FILTER: '[data-testid="mcp-filter"]',
  MCP_ROW: '[data-testid="mcp-row"]',
  /** One MCP row, by its server name (`data-server`). */
  MCP_ROW_NAMED: (name) => `[data-testid="mcp-row"][data-server="${name}"]`,
  PERSONA_MODAL: '[data-testid="persona-modal"]',
  PERSONA_FILTER: '[data-testid="persona-filter"]',
  PERSONA_ROW: '[data-testid="persona-row"]',
  /** One persona row, by its persona name (`data-persona`). */
  PERSONA_ROW_NAMED: (name) => `[data-testid="persona-row"][data-persona="${name}"]`,
  SESSION_ROW: '[data-testid="session-row"]',
  ACTIVE_SESSION_ROW: '.session.active[data-testid="session-row"]',
};

/** The sample provider's model — the keyless one every scratch boot configures. */
export const MODEL_NAME = "sample";
/** What the chip shows once it is selected: provider and model, joined. */
export const MODEL_LABEL = "sample/sample";
/** The second provider's model — the row a model-switch test picks. */
export const OTHER_MODEL_NAME = "second-model";
/** What the chip shows once that one is selected: provider and model, joined. */
export const OTHER_MODEL_LABEL = "second/second-model";

/**
 * Waits for the page's connection to be open: the overlay is gone and
 * the transcript is mounted.
 */
export async function awaitConnected(page) {
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0, { timeout: REPLY_TIMEOUT_MS });
}

/**
 * Types into the composer and clicks Send. The message is shown
 * optimistically, so the caller waits on the reply, not on the click.
 */
export async function send(page, text) {
  await page.locator(sel.DRAFT).fill(text);
  await page.locator(sel.SEND).click();
}

/**
 * Sends `text` and waits for the sample model's reply to settle: an
 * assistant entry is visible and the composer has left its busy state
 * (Send is back).
 *
 * The sample model answers every input instantly except `!response`,
 * which streams token by token — the only input that produces a real
 * busy window. A spec that needs to act on a running turn sends that.
 */
export async function sendAndWaitForEcho(page, text) {
  await send(page, text);
  await expect(
    page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last(),
  ).toBeVisible({ timeout: REPLY_TIMEOUT_MS });
  await expect(page.locator(sel.SEND)).toBeVisible({ timeout: REPLY_TIMEOUT_MS });
}

/** The staged skill every skills spec pivots on. */
export const SKILL_NAME = scratchSkill.name;
/** A filter that matches the staged skill's name and nothing else about it. */
export const SKILL_FILTER = "e2e-scra";

/** The staged MCP server the MCP spec pivots on. */
export const MCP_SERVER = "e2e-echo";
/** The staged block-less persona the persona spec pivots on. */
export const BLOCKLESS_PERSONA = "blockless-e2e";

/** The one prompt the sample model streams slowly. */
export const SLOW_PROMPT = "!response";

/**
 * Starts a turn that streams for about a second and returns once the
 * composer is in its busy state (Queue and Cancel are shown). The
 * caller then acts on a running session.
 */
export async function startBusyTurn(page) {
  await send(page, SLOW_PROMPT);
  await expect(page.locator(sel.CANCEL)).toBeVisible({ timeout: REPLY_TIMEOUT_MS });
}

/**
 * Creates a second session on the test's instance and returns its id.
 *
 * There is no "new session" message in `/web2`'s first cut, so the
 * session is created the way a user of the original UI creates one: a
 * second page on the same jinn drives its new-session modal, and the
 * id is read from the URL it lands on. The original UI's hooks are its
 * published contract ids.
 */
export async function createSession(jinn, browser) {
  const page = await browser.newPage();
  try {
    await page.goto(`${jinn.legacyUrl}`);
    await page.locator("#menu-button").click();
    await page.locator("#menu-item-new-session").click();
    await expect(page.locator("#session-new-modal")).toBeVisible();
    await page.locator("#session-new-submit").click();
    const prefix = "/web/session/";
    await expect(page).toHaveURL(new RegExp(`${prefix}.+`));
    const url = page.url();
    return url.slice(url.indexOf(prefix) + prefix.length).replace(/[?#].*$/, "");
  } finally {
    await page.close();
  }
}

/**
 * Marks `id` archived in the test's own scratch store, with the same
 * statement jinn's store runs. `/web2` has no archive gesture, so this
 * arranges the store half only: the session stays loaded until the
 * instance restarts. A test that needs it gone from memory follows
 * this with `restartJinn`.
 *
 * @param {{ root: string }} jinn the test's own instance.
 * @param {string} id the session to archive.
 * @returns {void}
 */
export function archiveSession(jinn, id) {
  if (!/^[0-9a-f-]{36}$/i.test(id)) {
    throw new Error(`archiveSession: not a session id: ${id}`);
  }
  execFileSync(
    "sqlite3",
    [
      path.join(jinn.root, "db", "state.db"),
      `UPDATE sessions SET archived = TRUE WHERE id = '${id}';`,
    ],
    { stdio: ["ignore", "ignore", "pipe"] },
  );
}

/**
 * Opens the hamburger menu through its title-bar button and waits for
 * the panel to be visible.
 */
export async function openMenu(page) {
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();
}

/**
 * Opens the skills modal through the hamburger menu, and waits for its
 * rows to arrive.
 */
export async function openSkills(page) {
  await openMenu(page);
  await page.locator(sel.MENU_ITEM_SKILLS).click();
  await expect(page.locator(sel.SKILLS_MODAL)).toBeVisible();
}

/** Opens the tools modal through the hamburger menu, and waits for it. */
export async function openTools(page) {
  await openMenu(page);
  await page.locator(sel.MENU_ITEM_TOOLS).click();
  await expect(page.locator(sel.TOOLS_MODAL)).toBeVisible();
}

/** Opens the MCP servers modal through the hamburger menu, and waits for it. */
export async function openMcp(page) {
  await openMenu(page);
  await page.locator(sel.MENU_ITEM_MCP).click();
  await expect(page.locator(sel.MCP_MODAL)).toBeVisible();
}

/** Opens the persona modal through the hamburger menu, and waits for it. */
export async function openPersona(page) {
  await openMenu(page);
  await page.locator(sel.MENU_ITEM_PERSONA).click();
  await expect(page.locator(sel.PERSONA_MODAL)).toBeVisible();
}

/**
 * Selects the sample model from the chip, and waits for the chip to
 * name it.
 *
 * A session boots with no model selection, and a send without one is
 * refused instead of going busy — so the page fixture selects the
 * sample model before any spec runs. The model-picker spec needs the
 * *unselected* state and so drives the chip by hand instead.
 */
export async function selectModel(page) {
  await page.locator(sel.MODEL_CHIP).click();
  // A row shows its model and its provider as two lines; the chip shows
  // them joined. Match the row by the model it names.
  const row = page.locator(sel.MODEL_ROW).filter({ hasText: MODEL_NAME }).first();
  await expect(row).toBeVisible({ timeout: REPLY_TIMEOUT_MS });
  await row.click();
  await expect(page.locator(sel.MODEL_CHIP)).toHaveText(MODEL_LABEL, { timeout: REPLY_TIMEOUT_MS });
}

/** The per-test option channel: which staged resources this test's boot stages. */
const test = base.extend({
  stage: [{}, { option: true }],
  jinn: async ({ stage }, use) => {
    const run = await bootJinn({
      skill: stage.skill === true,
      persona: stage.persona === true,
      mcp: stage.mcp === true,
    });
    await use(run);
    await teardownJinn(run);
  },
  // The page a spec receives is on `/web2`, connected, with the sample
  // model already selected on the session.
  page: async ({ jinn, page }, use) => {
    await page.goto(jinn.baseUrl);
    await awaitConnected(page);
    await selectModel(page);
    await use(page);
  },
});

/**
 * Declares one test whose boot stages exactly the resources named.
 * `test.with` is how a test asks for a staged scratch resource:
 *
 *     test.with({ skill: true })("…", async ({ page }) => { … })
 *
 * Playwright has one per-test option channel — `use` inside a
 * `describe` — so `with` wraps the single test in a describe whose
 * title is empty: the test's own title is what every reporter and
 * `--list` print, unchanged, and the staging is scoped to that test
 * alone. A test that needs nothing uses `test` itself.
 */
const withStage =
  (stage) =>
  (title, fn) => {
    test.describe("", () => {
      test.use({ stage });
      test(title, fn);
    });
  };

test.with = withStage;

export { test, expect };
