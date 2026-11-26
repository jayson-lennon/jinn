//! Markdown, auto-links, and the Sources block on `/web2`.
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

/** The last assistant entry, where `!markdown`'s document lands. */
function lastReply(page) {
  return page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last();
}

/** Sends `!markdown` and waits for the document to settle on the page. */
async function renderShowcase(page) {
  await sendAndWaitForEcho(page, "!markdown");
  await expect(lastReply(page).locator("h2")).toHaveText("Markdown showcase");
}

test("a bare address becomes a new-tab link", async ({ page }) => {
  await renderShowcase(page);

  // Then the plain address is one anchor, opening in a new tab.
  const link = lastReply(page).locator('a[href="https://example.com/plain"]');
  await expect(link).toHaveCount(1);
  await expect(link).toHaveAttribute("target", "_blank");
  await expect(link).toHaveAttribute("rel", "noopener noreferrer");
});

test("a trailing period stays outside the link", async ({ page }) => {
  await renderShowcase(page);

  // Then the sentence-ending address links without its period.
  await expect(lastReply(page).locator('a[href="https://example.com/dot"]')).toHaveCount(1);
  await expect(lastReply(page).locator('a[href="https://example.com/dot."]')).toHaveCount(0);
});

test("a trailing comma stays outside the link", async ({ page }) => {
  await renderShowcase(page);

  // Then the comma address links without its comma.
  await expect(lastReply(page).locator('a[href="https://example.com/comma"]')).toHaveCount(1);
  await expect(lastReply(page).locator('a[href="https://example.com/comma,"]')).toHaveCount(0);
});

test("a closing bracket after the link takes the period with it", async ({ page }) => {
  await renderShowcase(page);

  // Then the bracket address links whole and the prose keeps its bracket.
  await expect(lastReply(page).locator('a[href="https://example.com/bracket"]')).toHaveCount(1);
  await expect(lastReply(page)).toContainText("https://example.com/bracket)");
});

test("an address wrapped in brackets links and the brackets stay text", async ({ page }) => {
  await renderShowcase(page);

  // Then the bracketed address is one anchor, and `[` `]` are not part of it.
  const link = lastReply(page).locator('a[href="https://example.com/wrapped"]');
  await expect(link).toHaveCount(1);
  await expect(link).toHaveText("https://example.com/wrapped");
  await expect(lastReply(page)).toContainText("[https://example.com/wrapped]");
});

test("an address with underscores and parentheses links whole", async ({ page }) => {
  await renderShowcase(page);

  // Then the Wikipedia address is a single unbroken link.
  await expect(
    lastReply(page).locator('a[href="https://en.wikipedia.org/wiki/Foo_(bar)"]'),
  ).toHaveCount(1);
});

test("www, ftp, protocol-relative, and prefixed addresses stay text", async ({ page }) => {
  await renderShowcase(page);

  // Then none of the four is an anchor.
  for (const href of [
    "http://www.example.com",
    "ftp://files.example.com/x",
    "//cdn.example.com/x.js",
    "https://xhttps://example.com",
  ]) {
    await expect(lastReply(page).locator(`a[href="${href}"]`)).toHaveCount(0);
  }
  // And the text is all still there.
  await expect(lastReply(page)).toContainText("www.example.com");
  await expect(lastReply(page)).toContainText("ftp://files.example.com/x");
  await expect(lastReply(page)).toContainText("//cdn.example.com/x.js");
  await expect(lastReply(page)).toContainText("xhttps://example.com");
});

test("an address inside a code span stays literal", async ({ page }) => {
  await renderShowcase(page);

  // Then the code span holds the address as text and holds no anchor.
  const code = lastReply(page).locator("code", { hasText: "https://example.com/in-code" });
  await expect(code).toHaveCount(1);
  await expect(code.locator("a")).toHaveCount(0);
});

test("an address inside a code block stays literal", async ({ page }) => {
  await renderShowcase(page);

  // Then the code block holds the address as text and holds no anchor.
  const block = lastReply(page).locator("pre", { hasText: "https://example.com/in-block" });
  await expect(block).toHaveCount(1);
  await expect(block.locator("a")).toHaveCount(0);
});

test("an email becomes a mailto link that opens in place", async ({ page }) => {
  await renderShowcase(page);

  // Then the email is a mailto anchor with no target, so the mail client
  // opens instead of a new tab.
  const mail = lastReply(page).locator('a[href="mailto:dev@example.com"]');
  await expect(mail).toHaveCount(1);
  await expect(mail).not.toHaveAttribute("target");
});

test("a footnote reference and its definition render as real elements", async ({
  page,
}) => {
  await renderShowcase(page);

  // Then the reference is a superscript link and the note is in a
  // footnotes block, with no literal `[^docs]` left on the page.
  await expect(lastReply(page).locator("sup.footnote-ref a")).toHaveCount(1);
  await expect(lastReply(page).locator(".footnotes .footnote-item")).toHaveCount(1);
  await expect(lastReply(page)).not.toContainText("[^docs]:");
});

test("a task list renders disabled checkboxes, checked and unchecked", async ({
  page,
}) => {
  await renderShowcase(page);

  // Then the two items carry a checkbox each: the first checked, the second
  // not, both disabled.
  const boxes = lastReply(page).locator("input.task-list-item-checkbox");
  await expect(boxes).toHaveCount(2);
  await expect(boxes.nth(0)).toBeChecked();
  await expect(boxes.nth(1)).not.toBeChecked();
  await expect(boxes.nth(0)).toBeDisabled();
  await expect(boxes.nth(1)).toBeDisabled();
});

test("the table renders as a real table", async ({ page }) => {
  await renderShowcase(page);

  // Then the document's table is a table with its header and one row.
  const table = lastReply(page).locator("table");
  await expect(table).toHaveCount(1);
  await expect(table.locator("th")).toHaveText(["field", "meaning"]);
  await expect(table.locator("td")).toHaveText(["id", "the model's id"]);
});

test("a second entry's footnote reference jumps to its own note", async ({ page }) => {
  await renderShowcase(page);
  await renderShowcase(page);

  // Given two assistant entries, each carrying a footnote.
  const replies = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`);
  await expect(replies).toHaveCount(2);

  // When reading each entry's footnote reference and note id.
  const firstRef = await replies.nth(0).locator("sup.footnote-ref a").getAttribute("href");
  const secondRef = await replies.nth(1).locator("sup.footnote-ref a").getAttribute("href");

  // Then the two references point at different notes.
  expect(firstRef).not.toBe(secondRef);
  // And each entry holds the note its own reference names.
  await expect(replies.nth(1).locator(`[id="${secondRef.slice(1)}"]`)).toHaveCount(1);
  await expect(replies.nth(0).locator(`[id="${firstRef.slice(1)}"]`)).toHaveCount(1);
});

test("a model-written input or script renders as text, never as markup", async ({
  page,
}) => {
  // Given a user message carrying raw HTML, which the sample model echoes
  // back verbatim in its help text.
  await sendAndWaitForEcho(page, "<input type=\"text\" onfocus=\"alert(1)\"> <script>alert(2)</script>");
  await sendAndWaitForEcho(page, "!markdown");

  // Then neither element is in the transcript: the input is not an element,
  // and no script tag exists anywhere on the page.
  const userEntry = page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("user")}`).first();
  await expect(userEntry.locator("input[type=\"text\"]")).toHaveCount(0);
  await expect(page.locator(`${sel.TRANSCRIPT} script`)).toHaveCount(0);
});

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
