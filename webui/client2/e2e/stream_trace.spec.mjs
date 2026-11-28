// The page's stream trace (`?debug=stream`): one console line at each hop a
// streamed chunk passes through inside the browser.
//
// The server logs its own half of this path. The browser's half exists so a
// stall can be placed between two hops instead of guessed at: a chunk that
// is received and then not applied, or applied and then not rendered, names
// the link that broke. These tests prove each hop prints in a real browser,
// because a trace that silently prints nothing is worse than no trace.

import { test, expect, sel, sendAndWaitForEcho, SLOW_PROMPT, awaitConnected } from "./fixture.mjs";

/**
 * Opens the test's page with the stream trace on and returns every trace
 * line the page prints, from before the navigation: the switch is read once
 * at module load, so the flag must be on the load that matters, and a
 * listener attached after it would miss the first frames.
 *
 * A line is read from its arguments rather than its rendered text. The page
 * prints each hop as two arguments, a message and a fields object, and the
 * console's own rendering of an object is Playwright's choice and not the
 * page's; `jsonValue()` gives the object itself.
 */
async function openTraced(page, jinn) {
  // Every console event is buffered as a promise, and `readTrace` awaits
  // them all: a listener that pushed after an async `jsonValue()` could
  // still be resolving the last frames of a turn when an assertion read the
  // list, and a hop that had printed would look absent.
  const pending = [];
  page.on("console", (message) => {
    if (!message.text().includes("[stream]")) return;
    pending.push(
      (async () => {
        const args = message.args();
        const msg = await args[0]?.jsonValue().catch(() => null);
        const fields = await args[1]?.jsonValue().catch(() => null);
        return { msg: String(msg ?? ""), fields };
      })(),
    );
  });
  await page.goto(`${jinn.baseUrl}?debug=stream`);
  await awaitConnected(page);
  return () => Promise.all(pending);
}

/** Waits for every trace line the page has printed, and returns them in order. */
const readTrace = (drain) => drain();

/** The trace lines of one hop, among all the lines the page printed. */
const hop = (lines, name) => lines.filter((line) => line.msg === `[stream] ${name}`);

/**
 * What the page did with the deltas it received, for a failure message: each
 * `entry_delta` the socket took, and every hop that followed it, in order.
 * The connection's own frames come first in the trace and say nothing about
 * a turn, so they are left out.
 */
const show = (lines) =>
  JSON.stringify(
    lines.filter((line) => line.fields?.type === "entry_delta" || line.msg !== "[stream] recv").slice(0, 12),
  );

test("the trace names every frame the page receives", async ({ page, jinn }) => {
  // Given the page opened with the stream trace on.
  const drain = await openTraced(page, jinn);

  // When a turn streams token by token.
  await sendAndWaitForEcho(page, SLOW_PROMPT);

  const lines = await readTrace(drain);

  // Then the socket hop printed for at least one delta.
  const received = hop(lines, "recv");
  expect(
    received.some((line) => line.fields?.type === "entry_delta"),
    show(lines),
  ).toBe(true);
});

test("the trace names every delta the store applies", async ({ page, jinn }) => {
  // Given the page opened with the stream trace on.
  const drain = await openTraced(page, jinn);

  // When a turn streams token by token.
  await sendAndWaitForEcho(page, SLOW_PROMPT);

  const lines = await readTrace(drain);

  // Then the store hop printed, and each line carries a text length.
  const applied = hop(lines, "apply");
  expect(applied.length, show(lines)).toBeGreaterThan(0);
  expect(applied.every((line) => typeof line.fields?.to === "number")).toBe(true);
});

test("the trace names every re-render of a streaming entry", async ({ page, jinn }) => {
  // Given the page opened with the stream trace on.
  const drain = await openTraced(page, jinn);

  // When a turn streams token by token.
  await sendAndWaitForEcho(page, SLOW_PROMPT);

  const lines = await readTrace(drain);

  // Then the render hop printed, and its lengths grow: a later render drew
  // more text than an earlier one.
  const rendered = hop(lines, "render").map((line) => line.fields?.chars);
  expect(rendered.length).toBeGreaterThan(1);
  const last = rendered.at(-1);
  const first = rendered[0];
  expect(last).toBeGreaterThan(first ?? 0);
});

test("the trace is silent when the page is opened without the flag", async ({ page, jinn }) => {
  // Given the page opened with no debug flag, as every user opens it.
  const lines = [];
  page.on("console", (message) => {
    if (message.text().startsWith("[stream]")) lines.push(message.text());
  });
  await page.goto(jinn.baseUrl);
  await awaitConnected(page);

  // When a turn streams token by token.
  await sendAndWaitForEcho(page, SLOW_PROMPT);

  // Then the page printed no stream line at all.
  expect(lines).toEqual([]);
});

test("the transcript still shows the streamed reply under the trace", async ({ page, jinn }) => {
  // Given the page opened with the stream trace on.
  await openTraced(page, jinn);

  // When a turn streams token by token.
  await sendAndWaitForEcho(page, SLOW_PROMPT);

  // Then the reply is on the page: the trace observes the stream and does not disturb it.
  await expect(page.locator(`${sel.TRANSCRIPT} ${sel.ENTRY("assistant")}`).last()).toBeVisible();
});
