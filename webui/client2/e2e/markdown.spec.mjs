//! Markdown, auto-links, and the Sources block on the web UI.
//!
//! The sample model's `!markdown` command streams a canned document carrying
//! every construct the renderer must handle; these specs read the page it
//! produces. Each test asserts one behaviour, so a failure names the one rule
//! that broke. An annotation entry cannot be produced by sending a message,
//! so those specs seed one straight into the scratch store and restart.

import {
  expect,
  openSession,
  sel,
  seedEntry,
  sendAndWaitForEcho,
  test,
  viewedSessionId,
  viewedSessionTitle,
} from "./fixture.mjs";
import { restartJinn } from "./harness.mjs";

test("a user message renders its markdown and links its addresses", async ({ page }) => {
  // Given a user message with markdown and a bare address.
  await sendAndWaitForEcho(page, "**bold** and https://example.com/from-user");

  // Then the user entry renders bold text and a new-tab link.
  const userEntry = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`).last();
  await expect(userEntry.locator("strong")).toHaveText("bold");
  const link = userEntry.locator('a[href="https://example.com/from-user"]');
  await expect(link).toHaveCount(1);
  await expect(link).toHaveAttribute("target", "_blank");
});

test("the Sources block collapses to its count and opens to linked titles", async ({
  page,
  jinn,
}) => {
  // Given a session with a title of its own, and an annotation entry with
  // two citations seeded in its store.
  await sendAndWaitForEcho(page, "a session with sources");
  const sessionId = await viewedSessionId(page);
  const title = await viewedSessionTitle(page);
  seedEntry(
    jinn,
    sessionId,
    JSON.stringify({
      Annotation: [
        { url: "https://example.com/source-a", title: "Source A" },
        { url: "https://example.com/source-b", title: "Source B" },
      ],
    }),
  );
  await restartJinn(jinn);
  await page.goto(jinn.baseUrl);
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0, { timeout: 20_000 });
  await openSession(page, sessionId, title);

  // Then the entry is a collapsed disclosure reading its count.
  const sources = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("annotation")}`).last();
  await expect(sources.locator("summary")).toContainText("Sources (2)");
  await expect(sources).not.toHaveAttribute("open");

  // When opening it.
  await sources.locator("summary").click();

  // Then each citation is a linked title with its address beside it.
  const titles = sources.locator("a.citation-title");
  await expect(titles).toHaveCount(2);
  await expect(titles.nth(0)).toHaveText("Source A");
  await expect(titles.nth(0)).toHaveAttribute("href", "https://example.com/source-a");
  await expect(titles.nth(0)).toHaveAttribute("target", "_blank");
  await expect(sources.locator(".citation-url").nth(0)).toHaveText(
    "https://example.com/source-a",
  );
});

test("a citation to a non-http address is never a link", async ({ page, jinn }) => {
  // Given a session with a title of its own, and an annotation whose URL is
  // a script scheme seeded in its store.
  await sendAndWaitForEcho(page, "a session with a hostile citation");
  const sessionId = await viewedSessionId(page);
  const title = await viewedSessionTitle(page);
  seedEntry(
    jinn,
    sessionId,
    JSON.stringify({
      Annotation: [{ url: "javascript:alert(1)", title: "Hostile" }],
    }),
  );
  await restartJinn(jinn);
  await page.goto(jinn.baseUrl);
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0, { timeout: 20_000 });
  await openSession(page, sessionId, title);

  // Then the title and address show as text, with no anchor.
  const sources = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("annotation")}`).last();
  await expect(sources.locator("summary")).toContainText("Sources (1)");
  await sources.locator("summary").click();
  await expect(sources.locator("a")).toHaveCount(0);
  await expect(sources.locator(".citation-title")).toHaveText("Hostile");
  await expect(sources.locator(".citation-url")).toHaveText("javascript:alert(1)");
});

/** Seeds a thinking entry holding `text` in the viewed session's store and
 *  reopens the page on it. The sample provider's `!think` streams the
 *  `<think>` tags as assistant text in this harness (the reasoning parser
 *  that would split them is a provider setting), so a real thinking entry
 *  is staged the way an annotation is: straight into the store. Its kind is
 *  a bare string on the wire — `Thinking(String)`. Returns the entry. */
async function givenThinkingEntry(page, jinn, text) {
  await sendAndWaitForEcho(page, "a session with a thinking entry");
  const sessionId = await viewedSessionId(page);
  const title = await viewedSessionTitle(page);
  seedEntry(jinn, sessionId, JSON.stringify({ Thinking: text }));
  await restartJinn(jinn);
  await page.goto(jinn.baseUrl);
  await expect(page.locator(sel.OVERLAY)).toHaveCount(0, { timeout: 20_000 });
  await openSession(page, sessionId, title);
  return page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("thinking")}`).last();
}

test("a thinking entry renders collapsed to its header", async ({ page, jinn }) => {
  // Given a thinking entry holding a sentence of its own.
  const thinking = await givenThinkingEntry(page, jinn, "the reader should not see this unclicked");

  // Then the block is one header line, and its text is nowhere on the page.
  await expect(thinking.locator('[data-testid="thinking-header"]')).toContainText("Thinking");
  await expect(thinking.locator('[data-testid="thinking-body"]')).toHaveCount(0);
  await expect(page.locator(sel.TRANSCRIPT)).not.toContainText(
    "the reader should not see this unclicked",
  );
});

test("clicking a thinking entry's header expands its text", async ({ page, jinn }) => {
  // Given a thinking entry, collapsed.
  const thinking = await givenThinkingEntry(page, jinn, "now the reader sees this");
  await expect(thinking.locator('[data-testid="thinking-body"]')).toHaveCount(0);

  // When its header is clicked.
  await thinking.locator('[data-testid="thinking-header"]').click();

  // Then the block's whole text is shown.
  await expect(thinking.locator('[data-testid="thinking-body"]')).toHaveText(
    "now the reader sees this",
  );
});
