//! Sending a message from the composer: the optimistic entry, the
//! streamed reply, and the composer returning to its idle state. And its
//! keys: Enter inserts a newline, and Ctrl+Enter or Shift+Enter sends.

import { execFileSync } from "node:child_process";
import path from "node:path";
import {
  TINY_PNG,
  attachImage,
  expect,
  sel,
  send,
  sendAndWaitForEcho,
  startBusyTurn,
  test,
} from "./fixture.mjs";

/** How long a message that must NOT arrive is given to prove it will not. */
const QUIET_MS = 400;

/** The text the server stored for a sent message, read back from the
 *  scratch store — what proves a multi-line draft reached it intact. The
 *  page renders a user entry through markdown, which folds a single line
 *  break, so the rendered text is not where the draft's own newlines show.
 *  The store is written when the turn settles, so the caller waits on the
 *  reply first. */
function storedUserText(jinn) {
  const sql = (query) =>
    execFileSync("sqlite3", [path.join(jinn.root, "db", "state.db"), query], {
      stdio: ["ignore", "pipe", "pipe"],
      encoding: "utf8",
    });
  const kind = sql("SELECT kind FROM entries WHERE kind LIKE '{\"User\":%' LIMIT 1;");
  return JSON.parse(kind).User.display;
}

test("a sent message echoes back as a settled assistant entry", async ({ page }) => {
  await sendAndWaitForEcho(page, "hello from web2");

  // Then the transcript holds the user's message and the model's reply.
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)).toContainText(
    "hello from web2",
  );
});

test("the composer clears the moment a message is sent", async ({ page }) => {
  await send(page, "gone from the box");

  // Then the draft is empty without waiting for any reply.
  await expect(page.locator(sel.DRAFT)).toHaveValue("");
});

test("an idle composer offers Send and no Cancel", async ({ page }) => {
  await expect(page.locator(sel.SEND)).toBeVisible();

  // Then the busy-state controls are absent.
  await expect(page.locator(sel.CANCEL)).toHaveCount(0);
});

/* ---- the composer's keys ----
 * Enter inserts a newline. Ctrl+Enter and Shift+Enter send — or queue,
 * while a turn runs — through the same `submit` the Send button calls.
 * Text is typed and keys are pressed, never filled: `fill` sets the
 * value without a keydown, so it cannot exercise the handler. */

test("Enter inserts a newline and does not send", async ({ page }) => {
  // Given an idle composer with one word typed.
  await page.locator(sel.DRAFT).click();
  await page.keyboard.type("first");

  // When pressing Enter.
  await page.keyboard.press("Enter");

  // Then the draft holds the word and a line break after it.
  await expect(page.locator(sel.DRAFT)).toHaveValue("first\n");
  // And nothing was sent: no user entry, once the time for one to arrive has passed.
  await page.waitForTimeout(QUIET_MS);
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)).toHaveCount(0);
});

test("Ctrl+Enter sends an idle draft", async ({ page }) => {
  // Given an idle composer with one word typed.
  await page.locator(sel.DRAFT).click();
  await page.keyboard.type("hello");

  // When pressing Ctrl+Enter.
  await page.keyboard.press("Control+Enter");

  // Then the message is sent: the draft is empty.
  await expect(page.locator(sel.DRAFT)).toHaveValue("");
});

test("Shift+Enter sends an idle draft", async ({ page }) => {
  // Given an idle composer with one word typed.
  await page.locator(sel.DRAFT).click();
  await page.keyboard.type("hello");

  // When pressing Shift+Enter.
  await page.keyboard.press("Shift+Enter");

  // Then the message is sent: the draft is empty.
  await expect(page.locator(sel.DRAFT)).toHaveValue("");
});

test("Ctrl+Enter sends a multi-line draft intact", async ({ page, jinn }) => {
  // Given an idle composer.
  await page.locator(sel.DRAFT).click();

  // When typing two lines, each started by Enter, and sending with Ctrl+Enter.
  await page.keyboard.type("one");
  await page.keyboard.press("Enter");
  await page.keyboard.type("two");
  await expect(page.locator(sel.DRAFT)).toHaveValue("one\ntwo");
  await page.keyboard.press("Control+Enter");
  await expect(
    page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last(),
  ).toBeVisible({ timeout: 15_000 });

  // Then the message the server stored is both lines, with the break between them.
  expect(storedUserText(jinn)).toBe("one\ntwo");
});

test("Shift+Enter queues behind a running turn", async ({ page }) => {
  // Given a running turn.
  await startBusyTurn(page);
  await page.locator(sel.DRAFT).click();
  await page.keyboard.type("behind");

  // When pressing Shift+Enter.
  await page.keyboard.press("Shift+Enter");

  // Then the message is queued: the depth reads one.
  await expect(page.locator(sel.QUEUE_DEPTH)).toHaveText("1", { timeout: 10_000 });
});

test("Ctrl+Enter on a newline-only draft does nothing", async ({ page }) => {
  // Given an idle composer holding only a line break.
  await page.locator(sel.DRAFT).click();
  await page.keyboard.press("Enter");

  // When pressing Ctrl+Enter.
  await page.keyboard.press("Control+Enter");

  // Then nothing was sent: no user entry, once the time for one to arrive has passed.
  await page.waitForTimeout(QUIET_MS);
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`)).toHaveCount(0);
  // And the server refused nothing: no toast.
  await expect(page.locator(sel.TOAST)).toHaveCount(0);
});

test("Shift+Enter sends an image with no text", async ({ page }) => {
  // Given one chip and an empty draft.
  await attachImage(page, TINY_PNG);

  // When pressing Shift+Enter in the draft.
  await page.locator(sel.DRAFT).click();
  await page.keyboard.press("Shift+Enter");

  // Then the image is sent: a user entry carrying it lands.
  await expect(
    page.locator(`${sel.ENTRY("user")} ${sel.ATTACHMENT}`),
  ).toHaveCount(1, { timeout: 15_000 });
});

test("the Send button sends a multi-line draft", async ({ page, jinn }) => {
  // Given an idle composer holding two lines.
  await page.locator(sel.DRAFT).fill("a\nb");

  // When clicking Send.
  await page.locator(sel.SEND).click();
  await expect(
    page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last(),
  ).toBeVisible({ timeout: 15_000 });

  // Then the message the server stored is both lines, with the break between them.
  expect(storedUserText(jinn)).toBe("a\nb");
});

test("the title editor still commits on Enter", async ({ page }) => {
  // Given the title editor open.
  await page.locator(sel.TITLE).click();

  // When typing a new title and pressing Enter.
  await page.locator(sel.TITLE_EDIT).fill("renamed by enter");
  await page.locator(sel.TITLE_EDIT).press("Enter");

  // Then the new title is shown.
  await expect(page.locator(sel.TITLE)).toHaveText("renamed by enter");
});
