// The composer's `#` prompt popup, end to end: when it opens, what it
// holds, how it is picked from, and how it closes.
//
// Every test stages the scratch prompt template through `test.with({ prompt: true })`
// and pivots on a filter that matches it alone. The server is asked
// nothing while a token is typed — the list is shipped whole in the
// snapshot and filtered in the browser.

import {
  awaitConnected,
  createSession,
  expect,
  openPromptPopup,
  PROMPT_FILTER,
  PROMPT_NAME,
  sel,
  test as base,
} from "./fixture.mjs";

/** How long a reply from a running jinn may take — the tier's standard. */
const REPLY_TIMEOUT_MS = 15_000;

/** Every test here needs the staged prompt template. */
const test = base.with({ prompt: true });

/** The popup's highlighted row. */
const ACTIVE_ROW = '[data-testid="prompt-row"].active';

test("a hash at the start of the draft opens the popup", async ({ page }) => {
  // Given a connected page with the staged template discovered.
  // When typing a lone `#` into an empty draft.
  await openPromptPopup(page);

  // Then the popup is open and lists the staged template.
  await expect(page.locator(sel.PROMPT_POPUP)).toBeVisible();
  await expect(page.locator(sel.PROMPT_ROW_NAMED(PROMPT_NAME))).toBeVisible();
});

test("a hash after a space opens the popup", async ({ page }) => {
  // Given a connected page.
  // When typing text, a space, then `#`.
  await page.locator(sel.DRAFT).click();
  await page.keyboard.type("hello #");

  // Then the popup is open.
  await expect(page.locator(sel.PROMPT_POPUP)).toBeVisible();
});

test("a hash after a newline opens the popup", async ({ page }) => {
  // Given a connected page.
  // When typing a line, a newline, then `#`. Enter inserts the newline.
  await page.locator(sel.DRAFT).click();
  await page.keyboard.type("hello");
  await page.keyboard.press("Enter");
  await page.keyboard.type("#");

  // Then the popup is open.
  await expect(page.locator(sel.PROMPT_POPUP)).toBeVisible();
});

test("a hash inside a word never opens the popup", async ({ page }) => {
  // Given a connected page.
  // When typing `issue#42`, whose `#` does not open a word.
  await page.locator(sel.DRAFT).click();
  await page.keyboard.type("issue#42");

  // Then the popup never opens.
  await expect(page.locator(sel.PROMPT_POPUP)).toHaveCount(0);
});

test("typing after the hash narrows the rows", async ({ page }) => {
  // Given the popup open with the staged template's filter typed.
  await openPromptPopup(page, PROMPT_FILTER);

  // Then only the staged template is listed.
  await expect(page.locator(sel.PROMPT_ROW)).toHaveCount(1);
  await expect(page.locator(sel.PROMPT_ROW_NAMED(PROMPT_NAME))).toBeVisible();
});

test("the row lists the name and the description", async ({ page }) => {
  // Given the popup open on the whole list.
  await openPromptPopup(page);

  // Then the staged template's row reads `name - description`.
  const row = page.locator(sel.PROMPT_ROW_NAMED(PROMPT_NAME));
  await expect(row).toContainText(PROMPT_NAME);
  await expect(row).toContainText(" - scratch prompt for the webui2 # popup");
});

test("no prompts found when nothing matches", async ({ page }) => {
  // Given a filter no template's name matches.
  await openPromptPopup(page, "zzzz-no-such");

  // Then the popup shows its one empty-state row and no prompt rows.
  await expect(page.locator(sel.PROMPT_EMPTY)).toHaveText("no prompts found");
  await expect(page.locator(sel.PROMPT_ROW)).toHaveCount(0);
});

test("the highlight starts on the last row", async ({ page }) => {
  // Given the popup open on the whole list.
  await openPromptPopup(page);

  // Then the highlighted row is the last one — the best match, as in the terminal.
  const rows = page.locator(sel.PROMPT_ROW);
  await expect(rows.last()).toHaveClass(/active/);
  await expect(page.locator(ACTIVE_ROW)).toHaveCount(1);
});

test("arrow up moves the highlight up one row", async ({ page }) => {
  // Given the popup open on the whole list, the last row highlighted.
  await openPromptPopup(page);

  // When pressing ArrowUp once.
  await page.keyboard.press("ArrowUp");

  // Then the second-to-last row is the one highlighted.
  await expect(page.locator(sel.PROMPT_ROW).nth(-2)).toHaveClass(/active/);
  await expect(page.locator(ACTIVE_ROW)).toHaveCount(1);
});

test("tab picks the highlighted row into the draft as a hash-name", async ({ page }) => {
  // Given the popup open on the staged template alone.
  await openPromptPopup(page, PROMPT_FILTER);

  // When pressing Tab.
  await page.keyboard.press("Tab");

  // Then the draft reads the bare name, and the popup is closed.
  await expect(page.locator(sel.DRAFT)).toHaveValue(`#${PROMPT_NAME}`);
  await expect(page.locator(sel.PROMPT_POPUP)).toHaveCount(0);
});

test("escape closes the popup and leaves the draft", async ({ page }) => {
  // Given the popup open.
  await openPromptPopup(page);

  // When pressing Escape.
  await page.keyboard.press("Escape");

  // Then the popup is gone and the draft still reads `#`.
  await expect(page.locator(sel.PROMPT_POPUP)).toHaveCount(0);
  await expect(page.locator(sel.DRAFT)).toHaveValue("#");
});

test("ctrl+enter with the popup open still sends", async ({ page }) => {
  // Given the popup open with a whole template name typed after the `#`.
  await openPromptPopup(page, PROMPT_NAME);

  // When pressing Ctrl+Enter — the send key.
  await page.keyboard.press("Control+Enter");

  // Then the message is sent — the draft clears and the turn settles — and the
  // popup is gone. It did not become a pick: the send key is never intercepted.
  await expect(page.locator(sel.DRAFT)).toHaveValue("");
  await expect(page.locator(sel.PROMPT_POPUP)).toHaveCount(0);
  await expect(
    page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last(),
  ).toBeVisible({ timeout: REPLY_TIMEOUT_MS });
});

test("enter with the popup open inserts a newline, not a pick", async ({ page }) => {
  // Given the popup open on the staged template alone.
  await openPromptPopup(page, PROMPT_FILTER);

  // When pressing Enter — the newline key.
  await page.keyboard.press("Enter");

  // Then a newline was inserted after the token, and nothing was picked: the
  // draft still holds the typed filter, and the popup closed as the caret
  // left the token.
  await expect(page.locator(sel.DRAFT)).toHaveValue(`#${PROMPT_FILTER}\n`);
  await expect(page.locator(sel.PROMPT_POPUP)).toHaveCount(0);
});

test("moving the caret before the token closes the popup", async ({ page }) => {
  // Given text before the token, so there is somewhere before it to go, and
  // the popup open at the token. A caret sitting on the `#` itself still
  // counts as inside the token — the terminal's rule — so the caret has to
  // move past the `#` to the left to leave it.
  await page.locator(sel.DRAFT).click();
  await page.keyboard.type("some text #plan");
  await expect(page.locator(sel.PROMPT_POPUP)).toBeVisible();

  // When moving the caret to the start of the line with Home.
  await page.keyboard.press("Home");

  // Then the popup is gone.
  await expect(page.locator(sel.PROMPT_POPUP)).toHaveCount(0);
});

test("a caret on the hash itself stays in the completion", async ({ page }) => {
  // Given the popup open on a lone `#`.
  await openPromptPopup(page);

  // When moving the caret back onto the `#` with Home.
  await page.keyboard.press("Home");

  // Then the popup stays open: the caret is still within the token, as in
  // the terminal, where a caret on the trigger completes with no filter.
  await expect(page.locator(sel.PROMPT_POPUP)).toBeVisible();
});

test("a click away from the token closes the popup", async ({ page }) => {
  // Given text before the token with the popup open at the token — the caret
  // ends the typed token, so the click has somewhere before the `#` to land.
  await page.locator(sel.DRAFT).click();
  await page.keyboard.type("some text #plan");
  await expect(page.locator(sel.PROMPT_POPUP)).toBeVisible();

  // When clicking at the very start of the draft, before the `#`.
  await page.locator(sel.DRAFT).click({ position: { x: 1, y: 1 } });

  // Then the popup is gone.
  await expect(page.locator(sel.PROMPT_POPUP)).toHaveCount(0);
});

test("no client frame is sent while typing a token", async ({ page }) => {
  // Given a listener on every frame the page sends after it has connected.
  await awaitConnected(page);
  const sent = [];
  page.on("websocket", (ws) => ws.on("framesent", (f) => sent.push(String(f.payload))));

  // When typing a token and narrowing it.
  await openPromptPopup(page);
  await page.keyboard.type(PROMPT_FILTER);

  // Then nothing left the page for it: the list is filtered in the browser.
  expect(sent.filter((f) => f.includes('"type"'))).toEqual([]);
});

test("a session switch closes the popup and swaps the list", async ({ page }) => {
  // Given the popup open.
  await openPromptPopup(page);

  // When switching to a new session.
  await createSession(page);

  // Then the popup is gone. The draft survives the switch — it is the
  // user's — so the old `#` is still there; the new session's list is what
  // a fresh token in a cleared draft completes against.
  await expect(page.locator(sel.PROMPT_POPUP)).toHaveCount(0);
  await page.locator(sel.DRAFT).fill("");
  await openPromptPopup(page);
  await expect(page.locator(sel.PROMPT_ROW_NAMED(PROMPT_NAME))).toBeVisible();
});

// A real phone context: `tap` needs a touch-enabled page, and the fixture's
// page is a desktop one. `onPhone` scopes it to this test and stages the
// prompt the same way `with` does.
base
  .onPhone({ prompt: true })
  ("a tap on a phone picks a row without moving the caret", async ({ page }) => {
    // Given the popup open on the staged row alone.
    await openPromptPopup(page, PROMPT_FILTER);

    // When tapping the row.
    await page.locator(sel.PROMPT_ROW_NAMED(PROMPT_NAME)).tap();

    // Then the draft reads the bare name with the caret after it, and the
    // draft still holds focus.
    await expect(page.locator(sel.DRAFT)).toHaveValue(`#${PROMPT_NAME}`);
    await expect(page.locator(sel.DRAFT)).toBeFocused();
    const caret = await page.evaluate(
      () => document.querySelector('[data-testid="draft"]').selectionStart,
    );
    expect(caret).toBe(`#${PROMPT_NAME}`.length);
  });
