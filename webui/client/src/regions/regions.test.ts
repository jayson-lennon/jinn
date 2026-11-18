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
import { regions } from "./mod.js";
import * as sessions from "./sessions.js";
import * as transcript from "./transcript.js";
import * as composer from "./composer.js";

/**
 * A happy-dom window with the page shell installed as the active
 * document, plus the module-level `document` the region modules
 * resolve against.
 */
function pageShell(): Window {
  const window = new Window({ url: "http://localhost/web" });
  window.document.body.innerHTML = `
    <div id="${wire.PAGE_ROOT_ID}" ${wire.PAGE_SESSION_DATA_ATTR}="00000000-0000-0000-0000-000000000000">
      <nav id="sidebar"><h2>Sessions</h2><div id="${wire.SESSIONS_ID}"><span>old list</span></div></nav>
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
