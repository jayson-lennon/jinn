//! The region tests: the swaps under happy-dom, plus the client half
//! of the region conformance gate.
//!
//! Each swap test drives one region module's function against a DOM
//! built from the page shell's shape and asserts the observable
//! outcome — what a connected browser would show. The gate tests
//! assert the two invariants that license the boot module's simple
//! first-match dispatch: the regions' delegation selectors are
//! pairwise disjoint, and every registered SSE handler key is a real
//! wire event name.

import assert from "node:assert/strict";
import { test } from "node:test";

import { Window } from "happy-dom";

import * as wire from "../wire.js";
import { regions, type RegionModule, type SseEventLike } from "./mod.js";
import * as sessions from "./sessions.js";
import * as transcript from "./transcript.js";
import * as composer from "./composer.js";

/**
 * The happy-dom window's Element, duck-typed to the dispatch surface
 * the delegation hands over (the test tier's Element global is
 * happy-dom's own class, not the DOM lib's).
 */
type DomElement = Parameters<NonNullable<RegionModule["onClick"]>>[0];

/**
 * A happy-dom window with the page shell installed as the active
 * document, plus the module-level `document` the region modules
 * resolve against.
 */
function pageShell(): Window {
  const window = new Window({ url: "http://localhost/web" });
  window.document.body.innerHTML = `
    <div id="${wire.PAGE_ROOT_ID}" ${wire.PAGE_SESSION_DATA_ATTR}="00000000-0000-0000-0000-000000000000">
      <nav id="sidebar"><h2>Sessions</h2>
        <input id="${wire.SESSIONS_FILTER_INPUT_ID}" type="text">
        <div id="${wire.SESSIONS_ID}"><span>old list</span></div>
      </nav>
      <main id="main">
        <div id="titlebar"><span id="${wire.MODEL_PICKER_ID}"></span></div>
        <div id="${wire.TRANSCRIPT_WRAP_ID}">
          <div id="${wire.TRANSCRIPT_ID}" ${wire.TRANSCRIPT_CURSOR_DATA_ATTR}="0"></div>
        </div>
        <div id="${wire.CONTROLS_ID}">
          <div id="${wire.COMPOSER_ID}"><textarea id="${wire.INPUT_ID}"></textarea></div>
        </div>
      </main>
    </div>`;
  // The region modules (and the browser runtime) resolve `document`
  // globally.
  globalThis.document = window.document as unknown as typeof globalThis.document;
  return window;
}

/**
 * Installs a fetch double recording every requested URL and answering
 * each with the given body. Returns the recorded URLs.
 */
function recordingFetch(body: string): { urls: string[] } {
  const urls: string[] = [];
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    urls.push(String(input));
    return new Response(body, { status: 200 });
  }) as unknown as typeof fetch;
  return { urls };
}

/** The sessions SSE event the stream would deliver. */
function sessionsEvent(data: string): SseEventLike {
  return { data, lastEventId: "0" };
}

// ── The swaps, one per region ───────────────────────────────────────

test("appendChat inserts the fragment beforeend in order", () => {
  // Given the page shell with an empty transcript.
  const window = pageShell();

  // When appending two chat fragments in order.
  transcript.appendChat('<article class="entry user">one</article>');
  transcript.appendChat('<article class="entry assistant">two</article>');

  // Then the transcript holds both, in arrival order, inside the
  // transcript element — what a streaming reader sees.
  const entries = window.document
    .getElementById(wire.TRANSCRIPT_ID)!
    .querySelectorAll("article");
  assert.equal(entries.length, 2);
  assert.match(entries[0]!.textContent ?? "", /one/);
  assert.match(entries[1]!.textContent ?? "", /two/);
});

test("appendChat throws loudly when the transcript is missing", () => {
  // Given a page shell whose transcript was removed (a bad swap).
  const window = pageShell();
  window.document.getElementById(wire.TRANSCRIPT_ID)!.remove();

  // When appending a chat fragment.
  // Then the call throws naming the missing target — never a silent
  // no-op (the targetError class this project shipped once).
  assert.throws(
    () => transcript.appendChat("<article>lost</article>"),
    new RegExp(wire.TRANSCRIPT_ID),
  );
});

test("the optimistic user entry appears immediately and the server copy replaces it", () => {
  // Given a page shell.
  const window = pageShell();

  // When the user sends "hello there" and the optimistic entry lands.
  transcript.appendOptimisticUserEntry(
    "00000000-0000-0000-0000-000000000000",
    "hello there",
  );

  // Then the entry is visible right away, unescaped text intact.
  const entries = window.document.getElementById(wire.TRANSCRIPT_ID)!;
  assert.ok(entries.innerHTML.includes("hello there"), "optimistic entry renders");

  // When the server's chat push covers the same entry.
  transcript.appendChat(
    '<article class="entry user"><div class="who">user</div><div class="body">hello there</div></article>',
  );

  // Then exactly one copy remains — the server's.
  const userEntries = entries.querySelectorAll(`article[class="entry user"]`);
  assert.equal(userEntries.length, 1, `one user entry after convergence, got ${userEntries.length}`);
  assert.equal(
    entries.querySelector("[data-webui-optimistic]"),
    null,
    "the optimistic marker is gone",
  );
});

test("the optimistic entry escapes HTML in user text", () => {
  // Given a page shell.
  const window = pageShell();

  // When the user sends markup as their message.
  transcript.appendOptimisticUserEntry(
    "00000000-0000-0000-0000-000000000000",
    "<img src=x onerror=alert(1)>",
  );

  // Then the text renders as text — no img element exists.
  const entries = window.document.getElementById(wire.TRANSCRIPT_ID)!;
  assert.equal(entries.querySelector("img"), null, "no injected element");
  assert.ok(entries.innerHTML.includes("&lt;img"), "the markup is escaped");
});

test("replaceTranscript swaps the content and re-bases the cursor", () => {
  // Given the page shell carrying an appended streaming entry.
  const window = pageShell();
  transcript.appendChat("<article>stale streaming tail</article>");

  // When the convergence push replaces the transcript.
  transcript.replaceTranscript('<article class="entry user">converged</article>', 4);

  // Then the stale tail is gone, the converged entry is present, and
  // the inline cursor carries the event's cursor.
  const entries = window.document.getElementById(wire.TRANSCRIPT_ID)!;
  assert.ok(!entries.innerHTML.includes("stale streaming tail"));
  assert.ok(entries.innerHTML.includes("converged"));
  assert.equal(entries.getAttribute(wire.TRANSCRIPT_CURSOR_DATA_ATTR), "4");
});

test("replaceControls swaps the send button for the cancel fragment", () => {
  // Given the page shell with idle controls.
  const window = pageShell();

  // When replacing the controls with a busy fragment.
  composer.replaceControls(
    `<div id="${wire.COMPOSER_ID}"><button data-action="${wire.ACTION_CANCEL}">Cancel</button></div>`,
  );

  // Then the composer now offers cancel — the busy swap landed.
  const controls = window.document.getElementById(wire.CONTROLS_ID)!;
  assert.ok(controls.querySelector(`button[data-action="${wire.ACTION_CANCEL}"]`));
  assert.equal(controls.querySelector(`button[data-action="${wire.ACTION_SEND}"]`), null);
});

test("replaceSessions swaps the list wholesale", () => {
  // Given the page shell with an old list.
  const window = pageShell();

  // When replacing the sessions list.
  sessions.replaceSessions('<a class="srow">fresh row</a>');

  // Then only the fresh markup remains.
  const list = window.document.getElementById(wire.SESSIONS_ID)!;
  assert.ok(!list.innerHTML.includes("old list"));
  assert.ok(list.innerHTML.includes("fresh row"));
});

test("scrollTranscriptToEnd reaches the wrap's bottom without throwing", () => {
  // Given the page shell with a tall transcript.
  pageShell();
  transcript.appendChat(`<article style="height:2000px">tall entry</article>`);

  // When scrolling to the end.
  // Then the call resolves (happy-dom stubs geometry; the contract
  // under test is "does not throw on the real id").
  transcript.scrollTranscriptToEnd();
});

// ── The client half of the region conformance gate ──────────────────

test("the regions delegation selectors are pairwise disjoint", () => {
  // Given every registered region's selectors.
  const claimed: { region: string; selector: string }[] = regions().flatMap(
    (region) => region.selectors.map((selector) => ({ region: region.id, selector })),
  );

  // When comparing every pair for a duplicate claim.
  const duplicates = claimed.filter((claim, index) =>
    claimed.some(
      (other, otherIndex) =>
        otherIndex !== index && other.selector === claim.selector,
    ),
  );

  // Then no selector appears in two regions — the boot module's
  // first-match dispatch is only correct while the claims are
  // disjoint.
  assert.deepEqual(
    duplicates,
    [],
    `selectors claimed by more than one region: ${duplicates.map((d) => `${d.region}: ${d.selector}`).join(", ")}`,
  );
});

test("every region sse handler key is a real wire event name", () => {
  // Given every registered region's SSE handler keys.
  const wireNames: string[] = Object.values(wire.SSE_EVENTS);
  const registered = regions().flatMap((region) =>
    Object.keys(region.sseHandlers).map((name) => ({ region: region.id, name })),
  );

  // When checking each key against the wire contract.
  // Then every key names a real SSE event — a typo'd name would
  // register a listener no server event ever fires.
  for (const entry of registered) {
    assert.ok(
      wireNames.includes(entry.name),
      `region ${entry.region} registers a handler for unknown SSE event "${entry.name}"`,
    );
  }
});

test("the registered region ids mirror the server registry one for one", () => {
  // Given the registered client regions.
  const clientIds = regions().map((region) => region.id).sort();

  // Then they are exactly the four server regions — the client and
  // server registries must not drift apart silently. (A fifth server
  // region shipping without its client half fails here.)
  assert.deepEqual(clientIds, ["composer", "model-picker", "sessions", "transcript"]);
});

test("every region selector names a wire contract value", () => {
  // Given the wire contract's runtime values (the SSE map excluded —
  // its values are event names, not DOM hooks).
  const wireValues: string[] = Object.entries(wire)
    .filter(([key]) => key !== "SSE_EVENTS")
    .map(([, value]) => String(value));

  // When checking each region's selectors.
  // Then each selector is built around a contract value — a
  // hand-spelled selector (a class or id the contract does not name)
  // would drift from the templates silently.
  for (const region of regions()) {
    for (const selector of region.selectors) {
      const stem = selector.replace(/^[#.]/, "");
      assert.ok(
        wireValues.includes(stem),
        `region ${region.id}'s selector "${selector}" does not name a contract value`,
      );
    }
  }
});

// ── The sidebar filter: pull, SSE re-pull, and swap survival ────────

test("an input event on the filter re-pulls the fragment carrying q", async () => {
  // Given the page shell and a fetch double recording the pull URLs.
  const window = pageShell();
  const { urls } = recordingFetch('<a class="srow">narrowed</a>');
  const region = regions().find((r) => r.id === "sessions")!;

  // When the filter's value changes and an input event dispatches
  // (the delegation path a keystroke, paste, or cut all fire).
  const input = window.document.getElementById(wire.SESSIONS_FILTER_INPUT_ID)!;
  (input as unknown as { value: string }).value = "fix the";
  region.onInput!(input as unknown as DomElement);

  // The pull is async; settle it.
  await new Promise((resolve) => setTimeout(resolve, 0));

  // Then the fetch carries the filter text percent-encoded.
  assert.deepEqual(urls, [`${wire.SESSIONS_ROUTE}?q=fix%20the`], `${urls}`);
});

test("a paste dispatching input re-pulls with the pasted text", async () => {
  // Given the page shell and a recording fetch.
  const window = pageShell();
  const { urls } = recordingFetch("<span>row</span>");
  const region = regions().find((r) => r.id === "sessions")!;

  // When a paste lands (happy-dom: the value changes, then input
  // dispatches — the observable contract, however the paste arrives).
  const input = window.document.getElementById(wire.SESSIONS_FILTER_INPUT_ID)!;
  (input as unknown as { value: string }).value = "archived notes";
  region.onInput!(input as unknown as DomElement);
  await new Promise((resolve) => setTimeout(resolve, 0));

  // Then the pull carries the pasted text.
  assert.deepEqual(urls, [`${wire.SESSIONS_ROUTE}?q=archived%20notes`]);
});

test("clearing the filter re-pulls the unfiltered list", async () => {
  // Given the page shell with a filter already active.
  const window = pageShell();
  const { urls } = recordingFetch("<span>everything</span>");
  const region = regions().find((r) => r.id === "sessions")!;
  const input = window.document.getElementById(wire.SESSIONS_FILTER_INPUT_ID)!;
  (input as unknown as { value: string }).value = "stale";
  region.onInput!(input as unknown as DomElement);
  await new Promise((resolve) => setTimeout(resolve, 0));

  // When the user clears the input and the input event dispatches.
  (input as unknown as { value: string }).value = "";
  region.onInput!(input as unknown as DomElement);
  await new Promise((resolve) => setTimeout(resolve, 0));

  // Then the second pull carries no q — the unfiltered list.
  assert.deepEqual(urls, [`${wire.SESSIONS_ROUTE}?q=stale`, wire.SESSIONS_ROUTE]);
});

test("a sessions push while filtered re-pulls with the same q instead of swapping the push in", async () => {
  // Given the page shell with an active filter.
  const window = pageShell();
  const { urls } = recordingFetch('<a class="srow">still filtered</a>');
  const input = window.document.getElementById(wire.SESSIONS_FILTER_INPUT_ID)!;
  (input as unknown as { value: string }).value = "fix";

  // When a sessions SSE event arrives carrying an unfiltered list.
  (sessions.sessionsRegion.sseHandlers[wire.SSE_EVENTS.sessions]!)(sessionsEvent(
    '<a class="srow">unfiltered push</a>',
  ));
  await new Promise((resolve) => setTimeout(resolve, 0));

  // Then the pushed HTML never lands — the client re-pulled with the
  // same filter, and the re-pull's result is what swapped in.
  assert.deepEqual(
    urls,
    [`${wire.SESSIONS_ROUTE}?q=fix`],
    `one re-pull, same q: ${urls}`,
  );
  const list = window.document.getElementById(wire.SESSIONS_ID)!;
  assert.ok(
    !list.innerHTML.includes("unfiltered push"),
    "the unfiltered push is not swapped in",
  );
  assert.ok(list.innerHTML.includes("still filtered"), "the re-pull swapped in");
  // And the input keeps the user's text.
  assert.equal((input as unknown as { value: string }).value, "fix");
});

test("a sessions push while unfiltered swaps the pushed fragment directly", () => {
  // Given the page shell with an empty filter.
  const window = pageShell();

  // When a sessions SSE event arrives.
  (sessions.sessionsRegion.sseHandlers[wire.SSE_EVENTS.sessions]!)(sessionsEvent(
    '<a class="srow">pushed row</a>',
  ));

  // Then the pushed fragment swaps in directly — no extra pull.
  const list = window.document.getElementById(wire.SESSIONS_ID)!;
  assert.ok(list.innerHTML.includes("pushed row"));
});

test("a sessions swap leaves the filter input and its text intact", () => {
  // Given the page shell with text typed into the filter.
  const window = pageShell();
  const input = window.document.getElementById(wire.SESSIONS_FILTER_INPUT_ID)!;
  (input as unknown as { value: string }).value = "keep me";

  // When the sessions list swaps (the #sessions innerHTML replace).
  sessions.replaceSessions('<a class="srow">fresh</a>');

  // Then the input element survives with its text — it lives outside
  // the swap target.
  const after = window.document.getElementById(wire.SESSIONS_FILTER_INPUT_ID)!;
  assert.equal(after, input, "the same input element remains");
  assert.equal((after as unknown as { value: string }).value, "keep me");
});

test("a page swap resets the filter input", () => {
  // Given the page shell with text typed into the filter.
  const window = pageShell();
  const input = window.document.getElementById(wire.SESSIONS_FILTER_INPUT_ID)!;
  (input as unknown as { value: string }).value = "about to vanish";

  // When a page swap replaces the page root (the session switch),
  // rendering a fresh shell whose input starts empty.
  const fresh = window.document.createElement("div");
  fresh.id = wire.PAGE_ROOT_ID;
  fresh.innerHTML = `
    <nav id="sidebar"><input id="${wire.SESSIONS_FILTER_INPUT_ID}" type="text">
      <div id="${wire.SESSIONS_ID}"></div></nav>`;
  window.document.getElementById(wire.PAGE_ROOT_ID)!.outerHTML = fresh.outerHTML;

  // Then the filter reads empty — view-local DOM state; the swap is
  // the reset.
  const after = window.document.getElementById(wire.SESSIONS_FILTER_INPUT_ID)!;
  assert.equal((after as unknown as { value: string }).value, "");
});

test("an input event outside the filter does not re-pull", async () => {
  // Given the page shell and a recording fetch.
  const window = pageShell();
  const { urls } = recordingFetch("<span>x</span>");
  const region = regions().find((r) => r.id === "sessions")!;

  // When an input event dispatches on the composer textarea (another
  // region's target).
  const textarea = window.document.getElementById(wire.INPUT_ID)!;
  const handled = region.onInput!(textarea as unknown as DomElement);
  await new Promise((resolve) => setTimeout(resolve, 0));

  // Then the sessions region declines it and nothing pulls.
  assert.equal(handled, false);
  assert.deepEqual(urls, [], "no fetch fired");
});
