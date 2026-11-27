//! The context item in each transcript entry's `⋮`: the force-include /
//! force-exclude toggle, and the dimmed look of an excluded entry.
//!
//! The item is a strict two-way toggle — it never returns an entry to the
//! default, which stays the terminal's `r` — and its label names the action
//! it will take, read from the state the server reports. A toggle lands as
//! an ordinary entry upsert, so each test waits on the page's own state
//! rather than on a reply.

import { entryMenu, expect, openSession, seedEntry, sel, sendAndWaitForEcho, test, viewedSessionId, viewedSessionTitle } from "./fixture.mjs";
import { restartJinn } from "./harness.mjs";

/** Waits for the last entry of `kind` to carry the `excluded` class. */
async function expectExcluded(page, kind) {
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY(kind)}`).last()).toHaveClass(
    /excluded/,
    { timeout: 5_000 },
  );
}

/** Waits for the last entry of `kind` to carry no `excluded` class. */
async function expectIncluded(page, kind) {
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY(kind)}`).last()).not.toHaveClass(
    /excluded/,
    { timeout: 5_000 },
  );
}

test("an unpinned entry offers the context item, and a pinned one does not", async ({
  page,
}) => {
  // Given a transcript with a user entry and an assistant reply.
  await sendAndWaitForEcho(page, "hello");

  // Then the user entry's menu carries the context item.
  await entryMenu(page, "user").click();
  await expect(page.locator(sel.ENTRY_MENU_CONTEXT)).toBeVisible();
  await page.keyboard.press("Escape");

  // When the assistant entry is pinned.
  await entryMenu(page, "assistant").click();
  await page.locator(sel.ENTRY_MENU_PIN).click();
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last()).toHaveClass(
    /pinned/,
    { timeout: 5_000 },
  );

  // Then its menu carries the pin item alone — a pin overrides exclusion,
  // so the context item would silently do nothing.
  await entryMenu(page, "assistant").click();
  await expect(page.locator(sel.ENTRY_MENU_PIN)).toBeVisible();
  await expect(page.locator(sel.ENTRY_MENU_CONTEXT)).toHaveCount(0);
});

test("excluding an entry dims it, and including it restores it", async ({
  page,
}) => {
  // Given a transcript with a user entry in context, reading as included.
  await sendAndWaitForEcho(page, "hello");
  await expectIncluded(page, "user");

  // When its ⋮ offers Exclude from context and it is picked.
  await entryMenu(page, "user").click();
  await expect(page.locator(sel.ENTRY_MENU_CONTEXT)).toHaveText("Exclude from context");
  await page.locator(sel.ENTRY_MENU_CONTEXT).click();

  // Then the entry is drawn as excluded, and the menu is closed.
  await expectExcluded(page, "user");
  await expect(page.locator(sel.ENTRY_MENU_POPUP)).toHaveCount(0);

  // When its ⋮ opens again, the item now names the other action, and it is
  // picked.
  await entryMenu(page, "user").click();
  await expect(page.locator(sel.ENTRY_MENU_CONTEXT)).toHaveText("Include in context");
  await page.locator(sel.ENTRY_MENU_CONTEXT).click();

  // Then the entry is drawn as included again.
  await expectIncluded(page, "user");
});

test("an entry the context leaves out by default reads Include in context", async ({
  page,
  jinn,
}) => {
  // Given a session with one message, plus a thinking entry seeded into the
  // store — a kind the model's context never carries by default.
  await sendAndWaitForEcho(page, "a session with a thought");
  const sessionId = await viewedSessionId(page);
  const title = await viewedSessionTitle(page);
  seedEntry(jinn, sessionId, JSON.stringify({ Thinking: "let me consider this" }));
  await restartJinn(jinn);
  await page.goto(jinn.baseUrl);
  await openSession(page, sessionId, title);

  // Then the thinking entry is drawn as excluded from the start.
  const thought = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("thinking")}`).last();
  await expect(thought).toHaveClass(/excluded/, { timeout: 5_000 });

  // And its menu names the one action that makes sense: include.
  await thought.locator(sel.ENTRY_MENU).click();
  await expect(page.locator(sel.ENTRY_MENU_CONTEXT)).toHaveText("Include in context");
});
