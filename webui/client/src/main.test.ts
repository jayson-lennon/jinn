//! The activate flow: page swap + stream rebind, the detached-node
//! gotcha that killed the htmx build.
//!
//! These tests drive `main.ts`'s real delegation path under
//! happy-dom: a click on a session row runs the activate fetch, the
//! page root is replaced, and the stream for the new session opens.
//! Fetch and EventSource are stubbed at the global boundary.

import assert from "node:assert/strict";
import { test } from "node:test";

import { Window } from "happy-dom";

import * as wire from "./wire.js";

/** The activations the stub EventSource recorded. */
const openedStreams: string[] = [];
/** The stub sources still open, so a test can assert closure. */
const liveSources: Set<StubEventSource> = new Set();

/** Minimal EventSource double: records the URL, close() marks it. */
class StubEventSource {
  url: string;
  closed = false;
  listeners = new Map<string, ((event: MessageEvent<string>) => void)[]>();

  constructor(url: string) {
    this.url = url;
    openedStreams.push(url);
    liveSources.add(this);
  }

  addEventListener(name: string, handler: (event: MessageEvent<string>) => void): void {
    const existing = this.listeners.get(name) ?? [];
    existing.push(handler);
    this.listeners.set(name, existing);
  }

  close(): void {
    this.closed = true;
    liveSources.delete(this);
  }

  /** Test hook: fires one event name with data and a cursor id. */
  emit(name: string, data: string, id: string): void {
    for (const handler of this.listeners.get(name) ?? []) {
      handler(new MessageEvent(name, { data, lastEventId: id }));
    }
  }
}

/** Installs the stubs and a landing page with one session row. */
function landing(): Window {
  const window = new Window({ url: "http://localhost/web" });
  window.document.body.innerHTML = `
    <div id="${wire.PAGE_ROOT_ID}">
      <nav id="sidebar"><div id="${wire.SESSIONS_ID}">
        <a class="${wire.SESSION_ROW_CLASS}" ${wire.SESSION_ID_DATA_ATTR}="11111111-1111-1111-1111-111111111111">First</a>
        <a class="${wire.SESSION_ROW_CLASS}" ${wire.SESSION_ID_DATA_ATTR}="22222222-2222-2222-2222-222222222222">Second</a>
      </div></nav>
      <main id="main">
        <div id="${wire.TRANSCRIPT_WRAP_ID}"><div id="${wire.TRANSCRIPT_ID}" ${wire.TRANSCRIPT_CURSOR_DATA_ATTR}="0"></div></div>
        <div id="${wire.CONTROLS_ID}"><div id="${wire.COMPOSER_ID}"><textarea id="${wire.INPUT_ID}"></textarea>
        <button data-action="${wire.ACTION_SEND}">Send</button></div></div>
        <span id="${wire.MODEL_PICKER_ID}"></span>
      </main>
    </div>`;
  globalThis.document = window.document as unknown as typeof globalThis.document;

  // The whole-page response the activate endpoint serves (the Page
  // contract), for the SECOND session.
  const secondPage = `
    <div id="${wire.PAGE_ROOT_ID}" ${wire.PAGE_SESSION_DATA_ATTR}="22222222-2222-2222-2222-222222222222">
      <nav id="sidebar"><div id="${wire.SESSIONS_ID}"></div></nav>
      <main id="main">
        <div id="${wire.TRANSCRIPT_WRAP_ID}"><div id="${wire.TRANSCRIPT_ID}" ${wire.TRANSCRIPT_CURSOR_DATA_ATTR}="0">second transcript</div></div>
        <div id="${wire.CONTROLS_ID}"><div id="${wire.COMPOSER_ID}"><textarea id="${wire.INPUT_ID}"></textarea>
        <button data-action="${wire.ACTION_SEND}">Send</button></div></div>
        <span id="${wire.MODEL_PICKER_ID}"></span>
      </main>
    </div>`;

  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.endsWith("/22222222-2222-2222-2222-222222222222/activate")) {
      return new Response(secondPage, { status: 200 });
    }
    throw new Error(`unexpected fetch: ${url}`);
  }) as typeof fetch;

  globalThis.EventSource = StubEventSource as unknown as typeof EventSource;
  return window;
}

/** Imports main.ts's initialize against the current globals. */
async function boot(): Promise<void> {
  await import("./main.js");
}

test("clicking a session row swaps the page and rebinds the stream", async () => {
  // Given the landing page with two session rows and no open stream.
  const window = landing();
  await boot();

  // When clicking the second row.
  const row = window.document.querySelectorAll(`.${wire.SESSION_ROW_CLASS}`)[1]!;
  row.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));

  // Then the page root was replaced wholesale (the WHOLE page from
  // the activate endpoint — carrying the new session id), and a
  // stream opened for the NEW session's id.
  const root = window.document.getElementById(wire.PAGE_ROOT_ID);
  assert.ok(root, "page root exists after swap");
  assert.equal(
    root.getAttribute(wire.PAGE_SESSION_DATA_ATTR),
    "22222222-2222-2222-2222-222222222222",
    "the swapped page names the new session",
  );
  assert.ok(root.innerHTML.includes("second transcript"), "the new page's transcript arrived");
  assert.deepEqual(
    openedStreams.filter((url) => url.endsWith("22222222-2222-2222-2222-222222222222")),
    [`${wire.STREAM_PATH}22222222-2222-2222-2222-222222222222`],
    "exactly one stream opened, for the new session",
  );
});

test("the pre-swap stream closes when the page swaps", async () => {
  // Given a landing page that opened its (empty-id) stream at boot,
  // then clicked through to a session.
  const window = landing();
  await boot();
  const row = window.document.querySelectorAll(`.${wire.SESSION_ROW_CLASS}`)[1]!;
  row.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));

  // When the swap completes, every source opened before the swap is
  // closed — a source writing into detached markup is the bug class.
  const allClosed = [...liveSources].every((source) => !source.closed);
  assert.ok(
    openedStreams.length > 0,
    "at least one stream opened during the flow",
  );
  assert.ok(
    [...StubSourcesOpenedBeforeSwap()].every((source) => source.closed),
    "every pre-swap source is closed after the swap",
  );
  assert.ok(allClosed || liveSources.size === 1, "at most one live stream remains");
});

test("a streamed chat event after the swap lands in the NEW page's transcript", async () => {
  // Given the swap has completed.
  const window = landing();
  await boot();
  const row = window.document.querySelectorAll(`.${wire.SESSION_ROW_CLASS}`)[1]!;
  row.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));

  // When the new session's stream emits a chat fragment.
  const source = [...liveSources].find((candidate) =>
    candidate.url.endsWith("22222222-2222-2222-2222-222222222222"),
  );
  assert.ok(source, "the new session's stream is live");
  source.emit(wire.SSE_EVENTS.chat, '<article class="entry user">fresh token</article>', "5");

  // Then the fragment lands in the CURRENT document's transcript —
  // not a detached node from the pre-swap page.
  const transcript = window.document.getElementById(wire.TRANSCRIPT_ID);
  assert.ok(transcript, "transcript exists in the live document");
  assert.ok(
    transcript.innerHTML.includes("fresh token"),
    "the streamed fragment appended into the live DOM",
  );
});

/** The sources opened before the most recent swap, for closure asserts. */
function StubSourcesOpenedBeforeSwap(): StubEventSource[] {
  // Everything except the newest one.
  return liveSources.size > 0 ? [...liveSources].slice(0, -1) : [];
}
