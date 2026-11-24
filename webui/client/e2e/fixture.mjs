//! The webui e2e fixture: one jinn per test, and every setup verb.
//!
//! Specs import `test` and `expect` from here, never from
//! `@playwright/test`. Each `test(...)` boots its own jinn — its own
//! database, config, and data tree under a scratch root — before the
//! body runs, and tears it down after, on pass, fail, or timeout alike.
//! Nothing a test does outlives it, so no test can observe state
//! another test created. A test arranges its own precondition through
//! the verbs below.
//!
//! The verbs are the user-visible acts a spec's "Given" names — open a
//! session, create one, open the menu or a modal, send and wait for the
//! reply, switch the viewport, toggle library membership. Each exists
//! once, here, with the race-aware waits the acts need; a spec never
//! re-derives them. Helpers that only ASSERT stay in the spec that
//! asserts. A verb that reaches the instance takes the `jinn` handle
//! first — `openSession(jinn, page)` — so a spec never spells a base
//! URL; a verb that only drives the page takes the page alone.
//!
//! A test that needs a staged scratch resource (the block-less persona,
//! the scratch skill, the scratch applet) declares it on the test:
//!
//!     test("…", { stage: { persona: true } }, async ({ page, jinn }) => { … })
//!
//! The resource is written into the scratch root before jinn boots —
//! discovery scans at activation, so a file written after boot is
//! invisible — and the test reads its name from `staged`.

import { test as base, expect } from "@playwright/test";
import { bootJinn, teardownJinn, staged } from "./harness.mjs";
import * as sel from "./selectors.mjs";

export { test, expect, staged };

/** The model the tier's scratch config defines (`sel.MODEL_ID`). */
const MODEL_ID = sel.MODEL_ID;

/**
 * Boots one jinn per test. The `jinn` fixture is the instance handle:
 * `baseUrl` for a spec that drives the page by URL, `root` for a spec
 * that reads the instance's own files. The `auto` fixture is what
 * makes the boot happen for every test, including those that never
 * name `jinn` — a test that only uses `page` still gets its own
 * instance, and the config has no `baseURL`, so a spec cannot reach
 * any instance but its own.
 */
const test = base.extend({
  stage: [ {}, { option: true } ],
  jinn: [
    async ({ stage }, use) => {
      const run = await bootJinn({
        persona: stage.persona === true,
        skill: stage.skill === true,
        applet: stage.applet === true,
      });
      // A boot that fails AFTER the process is up must tear its own
      // instance down: Playwright only runs a fixture's teardown for a
      // fixture that was set up successfully, so an error thrown before
      // `use` would otherwise leave the jinn alive.
      try {
        await switchBootModel(run);
      } catch (error) {
        await teardownJinn(run);
        throw error;
      }
      await use(run);
      await teardownJinn(run);
    },
    { auto: true },
  ],
});

/**
 * Declares one test whose boots stage exactly the resources named.
 * `test.with` is how a test asks for a staged scratch resource:
 *
 *     test.with({ applet: true })("…", async ({ page, jinn }) => { … })
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

/** The `test` every spec imports; `with` stages resources per test. */
test.with = withStage;

/**
 * Puts the boot session on the sample model over the web surface.
 *
 * A fresh db has no `last_model`, so the boot session would otherwise
 * sit on `__no_provider__` and the picker flows would have nothing to
 * be "current". The switch goes through the same POST the page's own
 * delegation issues, so it is the user's path, not a back door.
 *
 * The bind accepting is not the registry being ready: the provider
 * registry fills its model rows asynchronously after the web surface is
 * up, and a switch that lands before then is refused with "not in the
 * available model rows" — a few boots in forty under per-test boots,
 * where nothing else delays the first request. So the switch waits for
 * the picker to list the model first — a poll on the registry's own
 * answer, not a sleep.
 *
 * @param {{ baseUrl: string }} run the instance handle.
 * @returns {Promise<void>} resolves once the switch landed.
 */
async function switchBootModel(run) {
  const sessionsHtml = await fetch(`${run.baseUrl}/web/sessions`).then((r) => r.text());
  const sessionId = /data-session-id="([^"]+)"/.exec(sessionsHtml)?.[1];
  if (!sessionId) throw new Error("no session row in the freshly booted sessions list");
  await awaitModelRows(run, sessionId);
  const res = await fetch(`${run.baseUrl}/web/model`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ session_id: sessionId, provider_id: MODEL_ID }),
  });
  if (!res.ok) throw new Error(`model switch failed: ${res.status} ${await res.text()}`);
}

/**
 * Waits until the boot session's picker lists the tier's model — the
 * registry's own readiness answer. Bounded; a registry that never
 * fills its rows is a boot failure, not a slow one.
 *
 * @param {{ baseUrl: string }} run the instance handle.
 * @param {string} sessionId the boot session.
 * @returns {Promise<void>} resolves once the row is listed.
 */
async function awaitModelRows(run, sessionId) {
  const pickerUrl = `${run.baseUrl}${sel.MODEL_PICKER_PREFIX}${sessionId}`;
  const deadline = Date.now() + 15_000;
  for (;;) {
    const listed = await fetch(pickerUrl)
      .then((r) => (r.ok ? r.text() : ""))
      .then((html) => html.includes(`data-webui-model="${MODEL_ID}"`));
    if (listed) return;
    if (Date.now() > deadline) {
      throw new Error(`the model picker never listed ${MODEL_ID} for the boot session`);
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

// ── Observers ───────────────────────────────────────────────────────

/**
 * Installs the page observers every navigation wait reads:
 *
 * - a count of stream fetches that got a `text/event-stream` response
 *   (the Datastar runtime's transport is `fetch()` plus manual SSE
 *   parsing — no EventSource — so counting those responses observes
 *   "a stream opened"); and
 *
 * - a count of `datastar-signal-patch` events whose patch carries the
 *   `catchupCursor` key — the from-zero catch-up's completion and the
 *   wrap's answering reset both carry it. The signal's VALUE is not
 *   visible to a document listener (Datastar applies the reset before
 *   the listener runs), so the count is the observable, not the value.
 *
 * Observers on the bundle's own published events, not app hooks.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 */
async function observeStreams(page) {
  await page.addInitScript(() => {
    window.__e2eSseOpens = 0;
    window.__e2eCatchupPatches = 0;
    const Native = window.fetch;
    if (!Native) return;
    window.fetch = (...args) =>
      Native(...args).then((response) => {
        const url = typeof args[0] === "string" ? args[0] : args[0]?.url ?? "";
        if (
          url.includes("/web/stream/") &&
          response.headers.get("content-type")?.includes("text/event-stream")
        ) {
          window.__e2eSseOpens += 1;
        }
        return response;
      });
    document.addEventListener("datastar-signal-patch", (event) => {
      if ((event.detail ?? {}).catchupCursor !== undefined) {
        window.__e2eCatchupPatches += 1;
      }
    });
  });
}

/**
 * Waits until the page's stream is connected: at least one stream
 * fetch on the page got an event-stream response. Several pushes are
 * broadcast-only (no catch-up replay), so interacting before the
 * stream opens races a missed push.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 */
async function expectStreamConnected(page) {
  await expect
    .poll(() => page.evaluate(() => window.__e2eSseOpens ?? 0), { timeout: 5_000 })
    .toBeGreaterThan(0);
}

/**
 * Waits for the stream's from-zero catch-up to complete — only when
 * there is one.
 *
 * A session page reaches the browser two ways. A full page load renders
 * the settled history inline and stamps the transcript's cursor with
 * that count, so the stream starts AT the end and catches up nothing.
 * A swap (a session-row click, a create) lands a page root whose
 * transcript is empty and whose cursor is zero; the stream then appends
 * the whole history, and only a test that reads the transcript before
 * that lands sees the empty pre-catch-up frame.
 *
 * So the wait is conditioned on what the page says about itself: the
 * cursor attribute on the transcript the swap landed. Zero means the
 * history is still to arrive and the test waits for the completion
 * patch; non-zero means the page already carries everything and there
 * is nothing to wait for. A swapped-in session with no history at all
 * emits no completion patch — the wait would stall — so it is bounded
 * by whether the stream announced a catch-up at all: the completion
 * and the reset both carry the `catchupCursor` key, and the count of
 * such patches is what is polled.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @param {{ swappedIn: boolean }} how the page root landed.
 */
async function expectCatchupSettled(page, { swappedIn }) {
  if (!swappedIn) return;
  const cursor = Number(
    (await page.locator(sel.TRANSCRIPT).getAttribute(sel.TRANSCRIPT_CURSOR)) ?? "0",
  );
  if (cursor > 0) return;
  await expect
    .poll(() => page.evaluate(() => window.__e2eCatchupPatches ?? 0), {
      timeout: 2_000,
    })
    .toBeGreaterThan(0);
}

// ── Session navigation ──────────────────────────────────────────────

/**
 * Opens a session by clicking its row — the user's path — and waits
 * for the page swap to land, the page's stream to connect, and, when
 * the swap left the transcript to catch up, for that catch-up to
 * complete. Returns the session id now in the page root.
 *
 * The row click activates the session instance-wide. A test that wants
 * to VIEW a session without activating it uses `viewSession`.
 *
 * @param {{ baseUrl: string }} jinn the test's instance handle.
 * @param {import("@playwright/test").Page} page the browser page.
 * @param {string} [id] the session to open; the boot session when
 *   omitted.
 * @returns {Promise<string>} the session id now in the page root.
 */
export async function openSession(jinn, page, id) {
  await observeStreams(page);
  await page.goto(`${jinn.baseUrl}/web`);
  const row =
    id === undefined ? page.locator(sel.SESSION_ROW).first() : page.locator(sel.SESSION_ROW_BY_ID(id));
  await expect(row).toBeVisible();
  await row.click();
  await expect(page).toHaveURL(new RegExp(`${sel.SESSION_PAGE_PATH.replace("/", "/")}`));
  const root = page.locator(sel.PAGE_ROOT);
  await expect(root).toBeVisible();
  const sessionId = await root.getAttribute(sel.PAGE_SESSION_ATTR);
  if (!sessionId) {
    throw new Error("page root carries no session id after activating a session");
  }
  await expectStreamConnected(page);
  await expectCatchupSettled(page, { swappedIn: true });
  return sessionId;
}

/**
 * Views a session by loading its page URL — no row click, so the
 * session is NOT activated instance-wide. The page renders the settled
 * history inline; the stream connects and has nothing to catch up.
 *
 * @param {{ baseUrl: string }} jinn the test's instance handle.
 * @param {import("@playwright/test").Page} page the browser page.
 * @param {string} id the session to view.
 * @returns {Promise<void>} resolves once the page root is up and the
 *   stream is connected.
 */
export async function viewSession(jinn, page, id) {
  await observeStreams(page);
  await page.goto(`${jinn.baseUrl}${sel.SESSION_PAGE_PATH}${id}`);
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(sel.PAGE_SESSION_ATTR, id);
  await expectStreamConnected(page);
}

/**
 * Creates a session through the modal — menu, New session, submit —
 * and waits for the create's swap: the URL changes, the page root's id
 * changes to the new session's, and the menu closes. The swap is
 * async, so the id must CHANGE before this returns, or a caller reads
 * the pre-create session and clicks a row that is inert. Creation
 * activates the new session instance-wide.
 *
 * @param {{ baseUrl: string }} jinn the test's instance handle.
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<string>} the created session's id.
 */
export async function createSession(jinn, page) {
  const before = page.url();
  const root = page.locator(sel.PAGE_ROOT);
  const beforeId = await root.getAttribute(sel.PAGE_SESSION_ATTR);
  await page.locator(sel.MENU_BUTTON).click();
  await page.locator(sel.MENU_ITEM_NEW_SESSION).click();
  await expect(page.locator(sel.SESSION_NEW_MODAL)).toBeVisible();
  await page.locator(sel.SESSION_NEW_SUBMIT).click();
  await expect(page).not.toHaveURL(before);
  await expect(root).not.toHaveAttribute(sel.PAGE_SESSION_ATTR, beforeId ?? "");
  await expect(page.locator(sel.MENU_PANEL)).toBeHidden();
  const createdId = await root.getAttribute(sel.PAGE_SESSION_ATTR);
  if (!createdId) {
    throw new Error("create did not navigate; page root carries no session id");
  }
  await expect(page).toHaveURL(new RegExp(`${sel.SESSION_PAGE_PATH}${createdId}`));
  return createdId;
}

/**
 * Switches the viewed session to `id` by clicking its row and waits
 * for the swap to land. The row of the session already viewed is
 * inert, so switching to it is a no-op with no swap to wait for.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @param {string} id the session to switch to.
 * @returns {Promise<void>} resolves once the page shows `id`.
 */
export async function switchToSession(page, id) {
  const current = await page.locator(sel.PAGE_ROOT).getAttribute(sel.PAGE_SESSION_ATTR);
  if (current === id) return;
  await page.locator(sel.SESSION_ROW_BY_ID(id)).click();
  await expect(page).toHaveURL(new RegExp(`${sel.SESSION_PAGE_PATH}${id}`));
  await expect(page.locator(sel.PAGE_ROOT)).toHaveAttribute(sel.PAGE_SESSION_ATTR, id);
}

// ── Menu and modals ─────────────────────────────────────────────────

/**
 * Opens the hamburger menu and waits for its panel.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves once the panel is visible.
 */
export async function openMenu(page) {
  await page.locator(sel.MENU_BUTTON).click();
  await expect(page.locator(sel.MENU_PANEL)).toBeVisible();
}

/**
 * Opens one of the menu's modals — menu, the item, the modal — and
 * waits for the modal's overlay. `kind` names the modal by its menu
 * item: `"mcp"`, `"persona"`, `"tools"`, `"skills"`, `"applets"`.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @param {"mcp"|"persona"|"tools"|"skills"|"applets"} kind the modal.
 * @returns {Promise<import("@playwright/test").Locator>} the modal's
 *   overlay locator.
 */
export async function openModal(page, kind) {
  const modals = {
    mcp: { item: sel.MENU_ITEM_MCP, modal: sel.MCP_MODAL },
    persona: { item: sel.MENU_ITEM_PERSONA, modal: sel.PERSONA_MODAL },
    tools: { item: sel.MENU_ITEM_TOOLS, modal: sel.TOOLS_MODAL },
    skills: { item: sel.MENU_ITEM_SKILLS, modal: sel.SKILL_MODAL },
    applets: { item: sel.MENU_ITEM_APPLETS, modal: sel.APPLET_MODAL },
  };
  const { item, modal } = modals[kind];
  await openMenu(page);
  await page.locator(item).click();
  const overlay = page.locator(modal);
  await expect(overlay).toBeVisible();
  return overlay;
}

/**
 * Enables the tier's one MCP server for the viewed session through the
 * row click — the user's path — and waits for the connection to reach
 * `running`: the coordinator spawns the stdio connection, writes the
 * runtime cell, and the mcp-status signal patch re-pulls the open
 * modal's rows. The boot session enables nothing, so a test that needs
 * the server enabled arranges it here.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves once the row is enabled and its
 *   badge reads `running`.
 */
export async function enableMcpServer(page) {
  const row = page.locator(sel.MCP_ROW(sel.MCP_SERVER));
  await expect(row).not.toHaveClass(new RegExp(sel.MCP_ROW_ENABLED));
  await row.click();
  await expect(row).toHaveClass(new RegExp(sel.MCP_ROW_ENABLED));
  await expect(row.locator(`.${sel.MCP_BADGE_RUNNING}`)).toBeVisible({
    timeout: 15_000,
  });
}

/**
 * Withholds the `get_time` builtin for the viewed session through its
 * row click in the tools modal — the user's path, and the only one the
 * UI has (the write goes through the session's live tool filter). A
 * fresh session restricts nothing, so a test that needs the tool off
 * arranges it here. Resolves once the re-rendered modal marks the row
 * off.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves once the row is marked off.
 */
export async function withholdTool(page) {
  const row = page.locator(sel.TOOL_ROW(sel.TOOLS_SPEC_TOOL));
  await expect(row).not.toHaveClass(new RegExp(sel.TOOL_ROW_OFF));
  await row.click();
  await expect(row).toHaveClass(new RegExp(sel.TOOL_ROW_OFF));
}

// ── Composer ────────────────────────────────────────────────────────

/**
 * Selects the tier's one model for the viewed session — a session
 * created through the modal boots with no selection, and a send
 * without one errors instantly instead of going busy.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @returns {Promise<void>} resolves once the switch landed.
 */
export async function selectModel(page) {
  await page.locator(sel.PICKER_BUTTON).click();
  await expect(page.locator(sel.PICKER_MODAL)).toBeVisible();
  await page.locator(sel.PICKER_ROW_MODEL_SPAN(MODEL_ID)).click();
  await expect(page.locator(sel.PICKER_BUTTON)).toHaveText(MODEL_ID);
}

/**
 * Sends `text` and waits for the reply — the sample backend echoes
 * "sample response". Resolves once the turn's assistant entry is
 * visible AND the turn has settled (the send control is back), so a
 * following send cannot land on a busy session.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @param {string} text the prompt text.
 * @returns {Promise<void>} resolves once the reply landed and the turn
 *   settled.
 */
export async function sendAndWaitForEcho(page, text) {
  await page.locator(sel.INPUT).fill(text);
  await page.locator(sel.SEND_BUTTON).click();
  await expect(
    page
      .locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`)
      .filter({ hasText: "sample response" })
      .last(),
  ).toBeVisible({ timeout: 15_000 });
  await expect(page.locator(sel.SEND_BUTTON)).toBeVisible({ timeout: 15_000 });
}

// ── Viewport ────────────────────────────────────────────────────────

/** The phone viewport the mobile specs pivot on. */
export const PHONE = { width: 390, height: 844 };

/**
 * Switches the viewport — `PHONE` for the mobile layout, `DESKTOP` to
 * go back — and waits a frame so the layout has reflowed before the
 * caller reads geometry.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @param {{ width: number, height: number }} size the viewport size.
 * @returns {Promise<void>} resolves once the size is applied.
 */
export async function setViewport(page, size) {
  await page.setViewportSize(size);
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
}

/** The desktop viewport the config boots every page with. */
export const DESKTOP = { width: 1280, height: 800 };

// ── Library ─────────────────────────────────────────────────────────

/**
 * Toggles `id`'s library membership through its row's kebab menu and
 * clicks the entry. Resolves once the entry has been clicked; the
 * caller asserts the move.
 *
 * @param {import("@playwright/test").Page} page the browser page.
 * @param {string} id the session whose row to toggle.
 * @returns {Promise<void>} resolves once the POST has been clicked.
 */
export async function toggleLibrary(page, id) {
  await page.locator(sel.ROW_MENU_IN(id)).click();
  const entry = page
    .locator(sel.ROW_MENU_POPUP)
    .filter({ has: page.locator(sel.MENU_ENTRY(sel.ACTION_LIBRARY_TOGGLE)) })
    .first();
  await expect(entry).toBeVisible();
  await page.locator(sel.MENU_ENTRY(sel.ACTION_LIBRARY_TOGGLE)).first().click();
}
