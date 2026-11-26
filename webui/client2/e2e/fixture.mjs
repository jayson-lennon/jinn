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
import crypto, { randomUUID } from "node:crypto";
import path from "node:path";
import { bootJinn, restartJinn, scratchApplet, scratchSkill, teardownJinn } from "./harness.mjs";

/** How long a reply from a running jinn may take. */
const REPLY_TIMEOUT_MS = 15_000;

/** The selectors every spec shares. */
export const sel = {
  OVERLAY: '[data-testid="overlay"]',
  /** The "Loading session…" overlay a row click or an Archive pick raises. */
  SESSION_LOAD: '[data-testid="session-load-overlay"]',
  TRANSCRIPT: '[data-testid="transcript"]',
  ENTRY: (kind) => `.entry-${kind}`,
  DRAFT: '[data-testid="draft"]',
  SEND: '[data-testid="send"]',
  QUEUE: '[data-testid="queue"]',
  QUEUE_DEPTH: '[data-testid="queue-depth"]',
  CANCEL: '[data-testid="cancel"]',
  TITLE: '[data-testid="title"]',
  TITLE_EDIT: '[data-testid="title-edit"]',
  MODEL_CHIP: '[data-testid="model-chip"]',
  MODEL_ROW: '[data-testid="model-row"]',
  MODEL_FILTER: '[data-testid="model-filter"]',
  OPEN_PICKER: '[data-testid="open-picker"]',
  PICKER_ROW: '[data-testid="picker-row"]',
  PICKER_FILTER: '[data-testid="picker-filter"]',
  /** One picker row, by its depth in the tree. */
  PICKER_ROW_AT_DEPTH: (depth) => `[data-testid="picker-row"][data-depth="${depth}"]`,
  /** One picker row, by its stored state. */
  PICKER_ROW_IN_STATE: (state) => `[data-testid="picker-row"][data-state="${state}"]`,
  MENU_BUTTON: '[data-testid="menu-button"]',
  MENU_PANEL: '[data-testid="menu-panel"]',
  MENU_SCRIM: '[data-testid="menu-scrim"]',
  MENU_ITEM_SKILLS: '[data-testid="menu-item-skills"]',
  MENU_ITEM_ARCHIVE: '[data-testid="menu-item-archive"]',
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
  MENU_ITEM_APPLETS: '[data-testid="menu-item-applets"]',
  APPLETS_MODAL: '[data-testid="applets-modal"]',
  APPLETS_FILTER: '[data-testid="applets-filter"]',
  APPLET_ROW: '[data-testid="applet-row"]',
  /** One applet row, by its id (`data-applet`). */
  APPLET_ROW_NAMED: (id) => `[data-testid="applet-row"][data-applet="${id}"]`,
  APPLET_STATUS: '[data-testid="applet-status"]',
  SESSION_ROW: '[data-testid="session-row"]',
  ACTIVE_SESSION_ROW: '.session.active[data-testid="session-row"]',
  /** One rail row's wrapper, by session id. The row button inside also
   *  carries `data-session-id`, so a bare attribute selector matches two
   *  elements; this one names the wrapper alone. */
  ROW_WRAP: (id) => `[data-testid="session-row-wrap"][data-session-id="${id}"]`,
  /** The title bar's `#`, shown only below 768px. */
  SESSIONS_BUTTON: '[data-testid="sessions-button"]',
  /** The off-canvas rail's scrim. */
  DRAWER_SCRIM: '[data-testid="drawer-scrim"]',
  RAIL: '[data-testid="rail"]',
  SESSIONS_VIEW: '[data-testid="sessions-view"]',
  LIBRARY_VIEW: '[data-testid="library-view"]',
  ROW_MENU: '[data-testid="row-menu"]',
  ROW_MENU_LIBRARY: '[data-testid="row-menu-library"]',
  ROW_MENU_ARCHIVE: '[data-testid="row-menu-archive"]',
  /** The toast region's rows: every refusal and failure the page reports. */
  TOAST: '[data-testid="toast"]',
  /** The composer's `+`, and the hidden file input behind it. */
  ATTACH: '[data-testid="attach"]',
  ATTACH_INPUT: '[data-testid="attach-input"]',
  CHIP: '[data-testid="chip"]',
  CHIP_REMOVE: '[data-testid="chip-remove"]',
  ATTACHMENT: '[data-testid="attachment"]',
  REFRESH_MODELS: '[data-testid="refresh-models"]',
  MENU_ITEM_NEW_SESSION: '[data-testid="menu-item-new-session"]',
  MENU_ITEM_SESSIONS: '[data-testid="menu-item-sessions"]',
  NEW_SESSION_MODAL: '[data-testid="new-session-modal"]',
  NEW_SESSION_PROJECT: '[data-testid="new-session-project"]',
  NEW_SESSION_LIFECYCLE: '[data-testid="new-session-lifecycle"]',
  NEW_SESSION_ARGS: '[data-testid="new-session-args"]',
  NEW_SESSION_EMPTY: '[data-testid="new-session-empty"]',
  CREATE_SESSION: '[data-testid="create-session"]',
  THEME_LIGHT: '[data-testid="theme-group-light"]',
  THEME_DARK: '[data-testid="theme-group-dark"]',
  THEME_SYSTEM: '[data-testid="theme-group-system"]',
  /** The `⋮` at every transcript entry's bottom-right, its popup, and
   *  the one item it carries today. */
  ENTRY_MENU: '[data-testid="entry-menu"]',
  ENTRY_MENU_POPUP: '[data-testid="entry-menu-popup"]',
  ENTRY_MENU_PIN: '[data-testid="entry-menu-pin"]',
  /** The pin mark a pinned entry shows at its left edge. */
  PIN_MARK: '.pin-mark',
};

/** The `⋮` of the last entry of `kind`, e.g. `entryMenu(page, "assistant")`. */
export function entryMenu(page, kind) {
  return page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY(kind)} ${sel.ENTRY_MENU}`).last();
}

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

/** The staged applet the applets spec pivots on. */
export const APPLET_ID = scratchApplet.id;
export const APPLET_TITLE = scratchApplet.title;

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
 * Creates a second session on the test's instance through `/web2`'s
 * own New-session menu item and modal, and returns its id.
 *
 * The id is the `data-session-id` of the row the new session lands on
 * in the creating page's rail, which is pushed without a reload.
 *
 * @param {import('@playwright/test').Page} page the test's own page.
 * @returns {Promise<string>} the new session's id.
 */
export async function createSession(page) {
  /** The ids the rail lists right now. */
  const rowIds = () =>
    page
      .locator('[data-testid="session-row-wrap"]')
      .evaluateAll((rows) => rows.map((row) => row.dataset.sessionId));
  const before = await rowIds();
  await openNewSession(page);
  await page.locator(sel.CREATE_SESSION).click();
  // The modal closes at once; the row lands when the push arrives.
  await expect(page.locator(sel.CREATE_SESSION)).toHaveCount(0);
  await expect
    .poll(async () => (await rowIds()).filter((id) => !before.includes(id)).length, {
      timeout: 10_000,
    })
    .toBe(1);
  return (await rowIds()).find((id) => !before.includes(id));
}

/**
 * Opens the hamburger menu through its title-bar button and waits for
 * its panel to be visible.
 */
export async function openMenu(page) {
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();
}

/** Picks a color mode from the menu's color-mode group. */
export async function pickTheme(page, mode) {
  await openMenu(page);
  await page.locator(sel[`THEME_${mode.toUpperCase()}`]).click();
}

/** Opens the New session modal through the menu and waits for its choices. */
export async function openNewSession(page) {
  await openMenu(page);
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  await expect(page.locator(sel.CREATE_SESSION)).toBeVisible();
}

/** Opens the rail's Library view. */
export async function openLibraryView(page) {
  await page.locator(sel.LIBRARY_VIEW).click();
}

/**
 * Adds a session to, or removes it from, the library through its row's `⋮`.
 *
 * @param {import('@playwright/test').Page} page the test's own page.
 * @param {string} sessionId the session whose row holds the `⋮`.
 */
export async function toggleLibraryThroughMenu(page, sessionId) {
  await page.locator(`${sel.ROW_WRAP(sessionId)} ${sel.ROW_MENU}`).click();
  await page.locator(sel.ROW_MENU_LIBRARY).click();
}

/**
 * Archives the session the page is viewing through the hamburger menu's
 * Archive session item.
 *
 * @param {import('@playwright/test').Page} page the test's own page.
 */
export async function archiveThroughMenu(page) {
  await openMenu(page);
  await page.locator(sel.MENU_ITEM_ARCHIVE).click();
}

/**
 * Archives the session whose row holds the `⋮`, through that row's menu.
 *
 * @param {import('@playwright/test').Page} page the test's own page.
 * @param {string} sessionId the session to archive.
 */
export async function archiveThroughRowMenu(page, sessionId) {
  await page.locator(`${sel.ROW_WRAP(sessionId)} ${sel.ROW_MENU}`).click();
  await page.locator(sel.ROW_MENU_ARCHIVE).click();
}

/** The id of the session the page is viewing, or null when none is. */
export async function viewedSessionId(page) {
  return page.locator(sel.ACTIVE_SESSION_ROW).getAttribute("data-session-id");
}

/** The title of the session the page is viewing, as the rail shows it. */
export async function viewedSessionTitle(page) {
  return page.locator(`${sel.ACTIVE_SESSION_ROW} .session-title`).innerText();
}

/**
 * Opens the session `sessionId`, and waits for it to become the one viewed.
 *
 * A restart lands the page on a fresh, empty session rather than the one it
 * was on — and the rail only lists sessions the instance has in memory, so
 * the one a test seeded may have no row to click. In that case it is picked
 * from the Archive picker, which reads every stored session from disk; the
 * picker's rows carry titles, not ids, so `title` names the row.
 */
export async function openSession(page, sessionId, title) {
  const row = page.locator(`${sel.ROW_WRAP(sessionId)} ${sel.SESSION_ROW}`);
  if (await row.count()) {
    await row.click();
  } else {
    await page.locator(sel.OPEN_PICKER).click();
    const pickerRow = page.locator(sel.PICKER_ROW).filter({ hasText: title });
    await expect(pickerRow).toBeVisible({ timeout: 20_000 });
    await pickerRow.click();
  }
  await expect(page.locator(sel.ACTIVE_SESSION_ROW)).toHaveAttribute(
    "data-session-id",
    sessionId,
    { timeout: 10_000 },
  );
}

/**
 * Attaches an image through the composer's `+` and waits for its chip.
 *
 * @param {import('@playwright/test').Page} page the test's own page.
 * @param {{ name: string, bytes: Buffer, mimeType: string }} image the file to attach.
 */
export async function attachImage(page, image) {
  await page.locator(sel.ATTACH_INPUT).setInputFiles({
    name: image.name,
    mimeType: image.mimeType,
    buffer: image.bytes,
  });
  await expect(page.locator(sel.CHIP).filter({ hasText: image.name })).toHaveCount(1);
}

/** The 1×1 PNG every attachment spec attaches, and its file name. */
export const TINY_PNG = {
  name: "dot.png",
  mimeType: "image/png",
  bytes: Buffer.from(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC",
    "base64",
  ),
};

/**
 * Pastes an image into the composer and waits for its chip.
 *
 * The bytes cross as a base64 string: a structured clone turns a Node
 * `Buffer` into a bare `Uint8Array`, whose `toString("base64")` is not
 * base64 at all. The browser decodes the string and builds the
 * `DataTransfer` a real clipboard would hand over.
 */
export async function pasteImage(page, image) {
  await page.evaluate(
    ({ name, mimeType, base64 }) => {
      const binary = atob(base64);
      const bytes = new Uint8Array(binary.length);
      for (let at = 0; at < binary.length; at += 1) bytes[at] = binary.charCodeAt(at);
      const transfer = new DataTransfer();
      transfer.items.add(new File([bytes], name, { type: mimeType }));
      document
        .querySelector('[data-testid="draft"]')
        .dispatchEvent(
          new ClipboardEvent("paste", {
            clipboardData: transfer,
            bubbles: true,
            cancelable: true,
          }),
        );
    },
    { name: image.name, mimeType: image.mimeType, base64: image.bytes.toString("base64") },
  );
  await expect(page.locator(sel.CHIP).filter({ hasText: image.name })).toHaveCount(1);
}

/**
 * Marks `id` archived in the test's own scratch store, with the same
 * statement jinn's store runs. This arranges the store half only, for a
 * test that needs a session *already* archived before an instance
 * restarts: the session stays loaded in memory until then. A live archive
 * through the page is `archiveThroughMenu` / `archiveThroughRowMenu`.
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
 * Seeds a session forked from `parentId` directly in the scratch store —
 * a row whose `parent_session` names its parent, and nothing else, which
 * is all the session tree reads. `/web2` has no fork gesture, so the
 * store is arranged the way a fork leaves it.
 *
 * A test that needs the row listed follows this with `restartJinn`: the
 * picker reads the store, and a running instance has already cached its
 * summaries.
 *
 * @param {{ root: string }} jinn the test's own instance.
 * @param {string} parentId the session the new one is forked from.
 * @param {string} title the child's title.
 * @returns {string} the child's id.
 */
export function seedChildSession(jinn, parentId, title) {
  if (!/^[0-9a-f-]{36}$/i.test(parentId)) {
    throw new Error(`seedChildSession: not a session id: ${parentId}`);
  }
  const id = randomUUID();
  const now = new Date().toISOString();
  execFileSync(
    "sqlite3",
    [
      path.join(jinn.root, "db", "state.db"),
      `INSERT INTO sessions (id, title, updated_at, created_at, parent_session) VALUES ('${id}', '${title.replace(/'/g, "''")}', '${now}', '${now}', '${parentId}');`,
    ],
    { stdio: ["ignore", "ignore", "pipe"] },
  );
  return id;
}

/**
 * A UUIDv7, the format jinn mints for its own ids: 48 bits of millisecond
 * timestamp, then the version and variant bits, then random. An entry id
 * must be one of these, because the session store orders a session's entries
 * by id and a v4 id (Node's `randomUUID`) does not sort after the entries
 * already there.
 *
 * Two ids minted in the same millisecond do not sort in mint order — the
 * first 48 bits are equal and ordering falls to the random tail — so a
 * caller that needs to sort after an existing id passes `afterMs`.
 */
function uuidV7(afterMs = 0) {
  const bytes = crypto.randomBytes(16);
  const ms = BigInt(Math.max(Date.now(), afterMs));
  // 48-bit big-endian millisecond timestamp across the first six bytes.
  bytes[0] = Number((ms >> 40n) & 0xffn);
  bytes[1] = Number((ms >> 32n) & 0xffn);
  bytes[2] = Number((ms >> 24n) & 0xffn);
  bytes[3] = Number((ms >> 16n) & 0xffn);
  bytes[4] = Number((ms >> 8n) & 0xffn);
  bytes[5] = Number(ms & 0xffn);
  bytes[6] = (bytes[6] & 0x0f) | 0x70; // version 7
  bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 9562 variant
  const hex = bytes.toString("hex");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** The 48-bit millisecond timestamp a UUIDv7 carries, as a number. */
function uuidV7Timestamp(id) {
  const hex = id.replace(/-/g, "").slice(0, 12);
  return Number.parseInt(hex, 16);
}

/**
 * Seeds one entry directly in the scratch store, at the end of `sessionId`'s
 * history — the two rows jinn's own save writes: the entry, and its placement
 * in the session. `/web2` has no gesture that produces an annotation entry
 * (only a web-search tool result does), so the store is arranged the way such
 * a result leaves it.
 *
 * A transient entry is never persisted, so it cannot be seeded this way; it
 * arrives over the socket instead. A test that needs the seeded entry on the
 * page follows this with `restartJinn`: a running instance has already
 * loaded the session.
 *
 * @param {{ root: string }} jinn the test's own instance.
 * @param {string} sessionId the session the entry joins.
 * @param {string} kindJson the entry's kind, in its persisted serde form —
 *   e.g. `{"Annotation":[{"url":"https://…","title":"…"}]}`. Single quotes in
 *   any literal are doubled, as SQL requires.
 * @returns {string} the entry's id.
 */
export function seedEntry(jinn, sessionId, kindJson) {
  if (!/^[0-9a-f-]{36}$/i.test(sessionId)) {
    throw new Error(`seedEntry: not a session id: ${sessionId}`);
  }
  const db = path.join(jinn.root, "db", "state.db");
  const sql = (q) =>
    execFileSync("sqlite3", [db, q], { stdio: ["ignore", "pipe", "pipe"], encoding: "utf8" });
  // The store orders a session's entries by id, so the seed's id must sort
  // after the last one already there. One millisecond later guarantees it;
  // the same millisecond would leave the order to the random tail.
  const lastId = sql(
    `SELECT entry_id FROM session_history WHERE session_id = '${sessionId}' ORDER BY entry_id DESC LIMIT 1;`,
  ).trim();
  const id = uuidV7(lastId ? uuidV7Timestamp(lastId) + 1 : 0);
  const now = new Date().toISOString();
  const ordinal = Number(
    sql(
      `SELECT COALESCE(MAX(ordinal), -1) + 1 FROM session_history WHERE session_id = '${sessionId}';`,
    ).trim(),
  );
  const escaped = kindJson.replace(/'/g, "''");
  sql(`INSERT INTO entries (id, timing, kind, context_history) VALUES ('${id}', '{"Instant":{"at":"${now}"}}', '${escaped}', '[]');`);
  sql(`INSERT INTO session_history (session_id, entry_id, ordinal, pin_position, ignored, context_override) VALUES ('${sessionId}', '${id}', ${ordinal}, NULL, FALSE, 'default');`);
  return id;
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
 * Opens the applets modal through the hamburger menu, and waits for the
 * staged applet's row to arrive.
 */
export async function openApplets(page) {
  await openMenu(page);
  await page.locator(sel.MENU_ITEM_APPLETS).click();
  await expect(page.locator(sel.APPLETS_MODAL)).toBeVisible();
  await expect(page.locator(sel.APPLET_ROW_NAMED(APPLET_ID))).toBeVisible({
    timeout: REPLY_TIMEOUT_MS,
  });
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
      textOnly: stage.textOnly === true,
      applet: stage.applet === true,
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
