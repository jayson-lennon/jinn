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
 * Narrows a happy-dom node to the DOM `Element` the region handlers
 * take — the two `Element` types are structurally the same node under
 * different lib roots, so the cast is the seam, not a conversion.
 */
function el(node: unknown): Element {
  return node as Element;
}

/**
 * The hidden flag of the element with the given id, read host-
 * agnostically: `getElementById` resolves to the DOM lib's `Element`
 * in this tier, and happy-dom's HTMLElement carries `hidden` as a
 * reflected property — the duck-typed read is the seam (the same
 * discipline the `value` reads apply).
 */
function hiddenOf(window: Window, id: string): boolean {
  const candidate = window.document.getElementById(id) as unknown as {
    hidden?: unknown;
  };
  return candidate.hidden === true;
}

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
        <div id="titlebar"><span id="${wire.MENU_ID}"><button id="${wire.MENU_BUTTON_ID}" type="button" aria-expanded="false">☰</button><span id="${wire.MENU_PANEL_ID}" hidden><button id="${wire.MENU_ITEM_SESSIONS_ID}" ${wire.ACTION_DATA_ATTR}="${wire.ACTION_MENU_SESSIONS}">Sessions</button></span></span><span id="${wire.SESSION_TITLE_ID}" class="t">Contract Title</span><span id="${wire.MODEL_PICKER_ID}"></span></div>
        <div id="${wire.TRANSCRIPT_WRAP_ID}">
          <div id="${wire.TRANSCRIPT_ID}" ${wire.TRANSCRIPT_CURSOR_DATA_ATTR}="0"></div>
        </div>
        <div id="${wire.CONTROLS_ID}">
          <div id="${wire.COMPOSER_ID}"><textarea id="${wire.INPUT_ID}"></textarea></div>
        </div>
      </main>
      <div id="${wire.DRAWER_SCRIM_ID}" hidden></div>
      <div id="${wire.MENU_SCRIM_ID}" hidden></div>
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

  // Then they are exactly the six server regions — the client and
  // server registries must not drift apart silently. (A server region
  // shipping without its client half fails here.)
  assert.deepEqual(clientIds, [
    "composer",
    "menu",
    "model-picker",
    "session-new",
    "sessions",
    "transcript",
  ]);
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

// ── The menu: open, item close, drawer class, Escape at any focus ───

test("clicking the hamburger opens the panel", () => {
  // Given the page shell with the menu closed (the server ships the
  // panel hidden).
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  assert.equal(hiddenOf(window, wire.MENU_PANEL_ID), true, "the panel boots hidden");

  // When the click delegation claims the hamburger button.
  const button = window.document.getElementById(wire.MENU_BUTTON_ID)!;
  assert.equal(region.onClick!(el(button)), true, "the menu region claims it");

  // Then the panel is visible and the button's state reflects it.
  assert.equal(hiddenOf(window, wire.MENU_PANEL_ID), false, "the panel opened");
  assert.equal(
    window.document.getElementById(wire.MENU_BUTTON_ID)!.getAttribute("aria-expanded"),
    "true",
    "aria-expanded flipped to true",
  );
});

test("clicking the Sessions item closes the menu", () => {
  // Given the page shell with the menu open.
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  const button = window.document.getElementById(wire.MENU_BUTTON_ID)!;
  region.onClick!(el(button));
  assert.equal(
    hiddenOf(window, wire.MENU_PANEL_ID),
    false,
    "the menu started open",
  );

  // When clicking the "Sessions" item.
  const item = window.document.getElementById(wire.MENU_ITEM_SESSIONS_ID)!;
  assert.equal(region.onClick!(el(item)), true, "the menu region claims it");

  // Then the panel is closed again.
  assert.equal(
    hiddenOf(window, wire.MENU_PANEL_ID),
    true,
    "the menu closed",
  );
});

test("the Sessions item toggles the drawer-open class on the page root", () => {
  // Given the page shell with the menu open and the drawer closed.
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  const root = window.document.getElementById(wire.PAGE_ROOT_ID)!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));

  // When activating the item.
  region.onClick!(el(window.document.getElementById(wire.MENU_ITEM_SESSIONS_ID)!));

  // Then the drawer-open class is present on #page-root — the drawer's
  // state hook, carried by the page root (never body) so a page swap
  // resets it structurally.
  assert.ok(
    root.classList.contains(wire.DRAWER_OPEN_CLASS),
    "the drawer-open class is on the page root",
  );
});

test("activating the Sessions item shows the scrim with the drawer", () => {
  // Given the page shell with the menu open and the scrim hidden (the
  // server ships it hidden).
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));
  assert.equal(hiddenOf(window, wire.DRAWER_SCRIM_ID), true, "the scrim boots hidden");

  // When activating the item.
  region.onClick!(el(window.document.getElementById(wire.MENU_ITEM_SESSIONS_ID)!));

  // Then the scrim is shown exactly while the drawer is open.
  assert.equal(
    hiddenOf(window, wire.DRAWER_SCRIM_ID),
    false,
    "the scrim shows with the drawer",
  );
});

test("clicking the scrim closes the drawer and hides the scrim", () => {
  // Given the page shell with the drawer open (scrim showing).
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  const root = window.document.getElementById(wire.PAGE_ROOT_ID)!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));
  region.onClick!(el(window.document.getElementById(wire.MENU_ITEM_SESSIONS_ID)!));

  // When the click delegation claims the scrim itself.
  const scrim = window.document.getElementById(wire.DRAWER_SCRIM_ID)!;
  assert.equal(region.onClick!(el(scrim)), true, "the menu region claims the scrim");

  // Then the drawer-open class is gone and the scrim hides again.
  assert.ok(
    !root.classList.contains(wire.DRAWER_OPEN_CLASS),
    "the drawer closed on the scrim click",
  );
  assert.equal(hiddenOf(window, wire.DRAWER_SCRIM_ID), true, "the scrim hid");
});

test("a second Sessions activation removes the drawer-open class", () => {
  // Given the page shell with the drawer already open.
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  const root = window.document.getElementById(wire.PAGE_ROOT_ID)!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));
  region.onClick!(el(window.document.getElementById(wire.MENU_ITEM_SESSIONS_ID)!));
  assert.ok(
    root.classList.contains(wire.DRAWER_OPEN_CLASS),
    "the drawer started open",
  );

  // When opening the menu and activating the item again.
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));
  region.onClick!(el(window.document.getElementById(wire.MENU_ITEM_SESSIONS_ID)!));

  // Then the class is gone — the hook is a toggle.
  assert.ok(
    !root.classList.contains(wire.DRAWER_OPEN_CLASS),
    "the second toggle removed the class",
  );
});

test("Escape with focus on body closes an open menu", () => {
  // Given the page shell with the menu open and focus outside every
  // selector (body — the common case the selector dispatch cannot see).
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));
  assert.equal(
    hiddenOf(window, wire.MENU_PANEL_ID),
    false,
    "the menu started open",
  );

  // When the document-level keydown hook receives Escape.
  const handled = region.onDocumentKeyDown!(
    { key: "Escape" } as unknown as KeyboardEvent,
  );

  // Then the hook consumed it and the menu closed.
  assert.equal(handled, true, "the open menu consumes Escape");
  assert.equal(
    hiddenOf(window, wire.MENU_PANEL_ID),
    true,
    "the menu closed",
  );
});

test("Escape closes an open drawer and hides the scrim", () => {
  // Given the page shell with the drawer open (menu closed).
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  const root = window.document.getElementById(wire.PAGE_ROOT_ID)!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));
  region.onClick!(el(window.document.getElementById(wire.MENU_ITEM_SESSIONS_ID)!));

  // When the document-level keydown hook receives Escape.
  const handled = region.onDocumentKeyDown!(
    { key: "Escape" } as unknown as KeyboardEvent,
  );

  // Then the hook consumed it and the drawer closed, scrim hidden.
  assert.equal(handled, true, "the open drawer consumes Escape");
  assert.ok(
    !root.classList.contains(wire.DRAWER_OPEN_CLASS),
    "the drawer closed",
  );
  assert.equal(hiddenOf(window, wire.DRAWER_SCRIM_ID), true, "the scrim hid");
});

test("Escape with menu and drawer both open closes both in one press", () => {
  // Given the page shell with the menu open AND the drawer open (the
  // item leaves the drawer open; a second hamburger press can reopen
  // the menu on top of it).
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  const root = window.document.getElementById(wire.PAGE_ROOT_ID)!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));
  region.onClick!(el(window.document.getElementById(wire.MENU_ITEM_SESSIONS_ID)!));
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));

  // When the document-level keydown hook receives Escape.
  const handled = region.onDocumentKeyDown!(
    { key: "Escape" } as unknown as KeyboardEvent,
  );

  // Then both close — the dismissal order is menu-then-drawer in one
  // pass, not one press per overlay.
  assert.equal(handled, true, "the press was consumed");
  assert.equal(
    hiddenOf(window, wire.MENU_PANEL_ID),
    true,
    "the menu closed",
  );
  assert.ok(
    !root.classList.contains(wire.DRAWER_OPEN_CLASS),
    "the drawer closed",
  );
});

test("Escape when the menu is closed is declined", () => {
  // Given the page shell with the menu closed.
  pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;

  // When the document-level keydown hook receives Escape.
  const handled = region.onDocumentKeyDown!(
    { key: "Escape" } as unknown as KeyboardEvent,
  );

  // Then the hook declines — a closed menu must never fight another
  // region's Escape consumer (the rename editor, the picker modal).
  assert.equal(handled, false, "a closed menu declines Escape");
});

test("opening the menu shows the menu scrim with the slide-in panel", () => {
  // Given the page shell with both scrims booting hidden (the server
  // ships them hidden).
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  assert.equal(hiddenOf(window, wire.MENU_SCRIM_ID), true, "the menu scrim boots hidden");

  // When the hamburger opens the menu.
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));

  // Then the panel is visible and the menu scrim shows with it.
  assert.equal(
    hiddenOf(window, wire.MENU_PANEL_ID),
    false,
    "the panel opened",
  );
  assert.equal(
    hiddenOf(window, wire.MENU_SCRIM_ID),
    false,
    "the menu scrim shows with the panel",
  );
});

test("clicking the menu scrim closes the panel and hides the menu scrim", () => {
  // Given the page shell with the menu open (menu scrim showing).
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));

  // When the click delegation claims the menu scrim itself.
  const scrim = window.document.getElementById(wire.MENU_SCRIM_ID)!;
  assert.equal(region.onClick!(el(scrim)), true, "the menu region claims its scrim");

  // Then the panel closed and the menu scrim hid again.
  assert.equal(
    hiddenOf(window, wire.MENU_PANEL_ID),
    true,
    "the panel closed on the scrim click",
  );
  assert.equal(
    hiddenOf(window, wire.MENU_SCRIM_ID),
    true,
    "the menu scrim hid",
  );
});

test("the Sessions item hides the menu scrim while opening the drawer", () => {
  // Given the page shell with the menu open.
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));

  // When activating the Sessions item.
  region.onClick!(el(window.document.getElementById(wire.MENU_ITEM_SESSIONS_ID)!));

  // Then the menu scrim hid with the panel, and the drawer scrim
  // shows for the drawer.
  assert.equal(
    hiddenOf(window, wire.MENU_SCRIM_ID),
    true,
    "the menu scrim hid with the panel",
  );
  assert.equal(
    hiddenOf(window, wire.DRAWER_SCRIM_ID),
    false,
    "the drawer scrim shows with the drawer",
  );
});

test("Escape closes the menu and hides the menu scrim", () => {
  // Given the page shell with the menu open (scrim showing).
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));
  assert.equal(hiddenOf(window, wire.MENU_SCRIM_ID), false, "the scrim started showing");

  // When the document-level keydown hook receives Escape.
  const handled = region.onDocumentKeyDown!(
    { key: "Escape" } as unknown as KeyboardEvent,
  );

  // Then the hook consumed it, the panel closed, and the scrim hid.
  assert.equal(handled, true, "the open menu consumes Escape");
  assert.equal(
    hiddenOf(window, wire.MENU_PANEL_ID),
    true,
    "the menu closed",
  );
  assert.equal(
    hiddenOf(window, wire.MENU_SCRIM_ID),
    true,
    "the menu scrim hid",
  );
});

test("a page swap resets the menu scrim structurally", () => {
  // Given the page shell with the menu open (menu scrim showing).
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));
  assert.equal(hiddenOf(window, wire.MENU_SCRIM_ID), false, "the scrim started showing");

  // When a page swap replaces #page-root's outerHTML with a fresh
  // server render (which ships both scrims hidden).
  const fresh = window.document.createElement("div");
  fresh.id = wire.PAGE_ROOT_ID;
  fresh.setAttribute(wire.PAGE_SESSION_DATA_ATTR, "33333333-3333-3333-3333-333333333333");
  fresh.innerHTML =
    `<main id="main"></main><div id="${wire.DRAWER_SCRIM_ID}" hidden></div>` +
    `<div id="${wire.MENU_SCRIM_ID}" hidden></div>`;
  window.document.getElementById(wire.PAGE_ROOT_ID)!.outerHTML = fresh.outerHTML;

  // Then the menu scrim is hidden — view-local DOM state; the swap is
  // the reset (the panel ships hidden in the fresh render too).
  assert.equal(
    hiddenOf(window, wire.MENU_SCRIM_ID),
    true,
    "the menu scrim did not survive the swap",
  );
});

test("a page swap resets the drawer-open class structurally", () => {
  // Given the page shell with the drawer open.
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "menu")!;
  region.onClick!(el(window.document.getElementById(wire.MENU_BUTTON_ID)!));
  region.onClick!(el(window.document.getElementById(wire.MENU_ITEM_SESSIONS_ID)!));

  // When a page swap replaces #page-root's outerHTML with a fresh
  // server render (which never carries the class, and whose scrim
  // ships hidden).
  const fresh = window.document.createElement("div");
  fresh.id = wire.PAGE_ROOT_ID;
  fresh.setAttribute(wire.PAGE_SESSION_DATA_ATTR, "22222222-2222-2222-2222-222222222222");
  fresh.innerHTML = `<main id="main"></main><div id="${wire.DRAWER_SCRIM_ID}" hidden></div>`;
  window.document.getElementById(wire.PAGE_ROOT_ID)!.outerHTML = fresh.outerHTML;

  // Then the new page root carries no drawer-open class and the scrim
  // is hidden — view-local DOM state; the swap is the reset.
  const after = window.document.getElementById(wire.PAGE_ROOT_ID)!;
  assert.ok(
    !after.classList.contains(wire.DRAWER_OPEN_CLASS),
    "the drawer state did not survive the swap",
  );
  assert.equal(
    hiddenOf(window, wire.DRAWER_SCRIM_ID),
    true,
    "the scrim did not survive the swap",
  );
});

// ── The inline title editor ─────────────────────────────────────────

/**
 * A rename-posting fetch double: records the posted URL/body and
 * answers with the given sessions-list fragment (or refuses with the
 * given status).
 */
function renameFetch(
  posts: { url: string; body: string }[],
  ok: boolean,
  listHtml: string,
): void {
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url.includes("/rename") && init?.method === "POST") {
      posts.push({ url, body: String(init.body ?? "") });
      if (ok) {
        return new Response(listHtml, { status: 200 });
      }
      return new Response("title must not be empty", { status: 400 });
    }
    throw new Error(`unexpected fetch: ${url}`);
  }) as unknown as typeof fetch;
}

test("clicking the title span opens a pre-filled input inside it", () => {
  // Given the page shell.
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "sessions")!;

  // When the click delegation claims the title span.
  const span = window.document.getElementById(wire.SESSION_TITLE_ID)!;
  assert.equal(region.onClick!(el(span)), true, "the sessions region claims the span");

  // Then the span hosts a text input pre-filled with the displayed
  // title.
  const input = span.querySelector("input");
  assert.ok(input, "an input opened inside the span");
  assert.equal((input as unknown as { value: string }).value, "Contract Title");
});

test("clicking inside an open editor is inert — no second editor opens", () => {
  // Given the page shell with the editor already open.
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "sessions")!;
  const span = window.document.getElementById(wire.SESSION_TITLE_ID)!;
  region.onClick!(el(span));

  // When the click delegation claims the input itself (it bubbles to
  // the span).
  const input = span.querySelector("input")!;
  assert.equal(region.onClick!(el(input)), true, "handled");

  // Then there is still exactly one input — handled-and-inert.
  assert.equal(span.querySelectorAll("input").length, 1);
});

test("Enter commits the rename and updates the titlebar, tab title, and sidebar", async () => {
  // Given the page shell and a rename endpoint answering with a fresh
  // sessions list.
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "sessions")!;
  const posts: { url: string; body: string }[] = [];
  renameFetch(posts, true, `<a class="${wire.SESSION_ROW_CLASS}">Fresh Title</a>`);

  // When opening the editor, retyping, and pressing Enter (the
  // region's own keydown delegation — a plain preventDefault double
  // is the event surface the handler consumes).
  const span = window.document.getElementById(wire.SESSION_TITLE_ID)!;
  region.onClick!(el(span));
  const input = span.querySelector("input") as unknown as HTMLInputElement;
  input.value = "  Fresh Title  ";
  region.onKeyDown!(
    el(input),
    { key: "Enter", preventDefault() {} } as unknown as KeyboardEvent,
  );
  await new Promise((resolve) => setTimeout(resolve, 0));

  // Then the post went to the session's rename endpoint with the raw
  // value urlencoded, and the display shows the trimmed title.
  assert.equal(posts.length, 1, `one rename post, got ${posts.length}`);
  assert.match(posts[0]!.url, /\/web\/session\/[^/]+\/rename$/, `rename URL: ${posts[0]!.url}`);
  assert.match(
    posts[0]!.body,
    /title=%20%20Fresh%20Title%20%20/,
    `the raw value rides the body urlencoded: ${posts[0]!.body}`,
  );
  assert.equal(
    span.textContent,
    "Fresh Title",
    "the titlebar shows the trimmed committed title",
  );
  assert.equal(window.document.title, "Fresh Title — jinn", "tab title updated");
  const list = window.document.getElementById(wire.SESSIONS_ID)!;
  assert.ok(list.innerHTML.includes("Fresh Title"), "sidebar swapped from the response");
});

test("Escape reverts the editor to the pre-edit title without a request", () => {
  // Given the page shell with the editor open and retyped.
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "sessions")!;
  const posts: { url: string; body: string }[] = [];
  renameFetch(posts, true, "");
  const span = window.document.getElementById(wire.SESSION_TITLE_ID)!;
  region.onClick!(el(span));
  const input = span.querySelector("input") as unknown as HTMLInputElement;
  input.value = "Changed";

  // When pressing Escape.
  region.onKeyDown!(
    el(input),
    { key: "Escape", preventDefault() {} } as unknown as KeyboardEvent,
  );

  // Then the display reverts and nothing was posted.
  assert.equal(span.textContent, "Contract Title");
  assert.equal(posts.length, 0, "no request on Escape");
  assert.equal(span.querySelector("input"), null, "the editor closed");
});

test("an empty-after-trim commit reverts locally without a request", () => {
  // Given the page shell with the editor open holding whitespace.
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "sessions")!;
  const posts: { url: string; body: string }[] = [];
  renameFetch(posts, true, "");
  const span = window.document.getElementById(wire.SESSION_TITLE_ID)!;
  region.onClick!(el(span));
  const input = span.querySelector("input") as unknown as HTMLInputElement;
  input.value = "   ";

  // When committing with Enter.
  region.onKeyDown!(
    el(input),
    { key: "Enter", preventDefault() {} } as unknown as KeyboardEvent,
  );

  // Then the display reverts to the pre-edit title and nothing was
  // posted — a refused rename leaves the title unchanged, and the
  // server's 400 round trip is not spent.
  assert.equal(span.textContent, "Contract Title");
  assert.equal(posts.length, 0, "no request for an empty commit");
});

test("a refused commit reverts the display and reports the failure", async () => {
  // Given the page shell and a rename endpoint refusing with 400.
  const window = pageShell();
  const posts: { url: string; body: string }[] = [];
  renameFetch(posts, false, "");
  const errors: unknown[] = [];
  const original = console.error;
  console.error = (...parts: unknown[]) => errors.push(parts);

  const region = regions().find((candidate) => candidate.id === "sessions")!;
  try {
    // When committing a real title that the server refuses.
    const span = window.document.getElementById(wire.SESSION_TITLE_ID)!;
    region.onClick!(el(span));
    const input = span.querySelector("input") as unknown as HTMLInputElement;
    input.value = "Refused";
    region.onKeyDown!(
      input,
      { key: "Enter", preventDefault() {} } as unknown as KeyboardEvent,
    );
    await new Promise((resolve) => setTimeout(resolve, 0));

    // Then the display reverts to the pre-edit title and the failure
    // was logged — never a silent no-op.
    assert.equal(span.textContent, "Contract Title", "display reverted");
    assert.equal(errors.length, 1, `console.error called once, got ${errors.length}`);
    assert.ok(posts.length === 1, "the refused commit did reach the wire");
  } finally {
    console.error = original;
  }
});

test("blur (clicking away) commits the rename", async () => {
  // Given the page shell with the editor open and retyped.
  const window = pageShell();
  const region = regions().find((candidate) => candidate.id === "sessions")!;
  const posts: { url: string; body: string }[] = [];
  renameFetch(posts, true, `<a class="${wire.SESSION_ROW_CLASS}">Blurred</a>`);

  // When the input loses focus (the module binds blur on the input
  // itself, so a real dispatch reaches it).
  const span = window.document.getElementById(wire.SESSION_TITLE_ID)!;
  region.onClick!(el(span));
  const input = span.querySelector("input") as unknown as HTMLInputElement;
  input.value = "Blurred";
  input.dispatchEvent(new window.Event("blur") as unknown as Event);
  await new Promise((resolve) => setTimeout(resolve, 0));

  // Then exactly one commit fired and the titlebar shows the new
  // title.
  assert.equal(posts.length, 1, `one rename post, got ${posts.length}`);
  assert.equal(span.textContent, "Blurred");
});

test("the landing page's inert title carries no editor host id", () => {
  // Given a landing page shell (no session-title span — the server
  // renders the id on session pages only).
  const window = new Window({ url: "http://localhost/web" });
  window.document.body.innerHTML = `
    <div id="${wire.PAGE_ROOT_ID}" ${wire.PAGE_SESSION_DATA_ATTR}="">
      <main id="main"><div id="titlebar"><span class="t">jinn</span></div></main>
    </div>`;
  globalThis.document = window.document as unknown as typeof globalThis.document;

  // When the click delegation sees the title (no session-title id to
  // closest() through — the landing page never carries it).
  const region = regions().find((candidate) => candidate.id === "sessions")!;
  const title = window.document.querySelector("#titlebar .t")!;
  const claimed = region.onClick!(el(title));

  // Then the region does not claim it and nothing opened.
  assert.equal(claimed, false, "the landing title is inert");
  assert.equal(window.document.querySelector("input"), null, "no editor opened");
});
