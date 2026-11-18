//! The model-picker region's client behaviors under happy-dom:
//! open/close, server-side filter pulls, latest-wins ordering, the
//! `models` SSE push (filter + focus survive), the switch POST, and
//! the unavailable row's non-interaction.
//!
//! Each test drives the region module's handlers against a DOM built
//! from the page shell's shape and a fetch double recording the
//! requests — what a connected browser would do.

import assert from "node:assert/strict";
import { test } from "node:test";

import { Window } from "happy-dom";

import * as wire from "../wire.js";
import { modelPickerRegion } from "./model_picker.js";

/** One recorded fetch: method + url, for the tests' assertions. */
interface RecordedRequest {
  method: string;
  url: string;
}

/** The fetch double's recorded requests, most recent last. */
const requests: RecordedRequest[] = [];

/** Resolvers for in-flight GET pulls, keyed in issue order. */
const pendingPulls: ((html: string) => void)[] = [];

/** The canned modal fragment a pull resolves with (parameterized). */
let modalResponse = "";

/** The canned button fragment the refresh/switch POSTs resolve with. */
let buttonResponse = "";

/**
 * Narrows a happy-dom node to the DOM `Element` the region handlers
 * take — the two `Element` types are structurally the same node under
 * different lib roots, so the cast is the seam, not a conversion.
 */
function el(node: unknown): Element {
  return node as Element;
}

/** Builds one modal fragment over the given rows + filter echo. */
function modalHtml(filter: string, rows: string): string {
  return `<div id="${wire.MODEL_PICKER_MODAL_ID}" class="model-picker-modal">
  <div class="model-picker-modal-inner">
    <div class="model-picker-modal-head">
      <input id="${wire.MODEL_PICKER_FILTER_ID}" type="text" value="${filter}" autocomplete="off">
      <button id="${wire.MODEL_PICKER_REFRESH_ID}" ${wire.ACTION_DATA_ATTR}="${wire.ACTION_REFRESH_MODELS}">Refresh models</button>
    </div>
    <div id="${wire.MODEL_PICKER_ROWS_ID}">${rows}</div>
  </div>
</div>`;
}

/** One available row markup fixture. */
function rowHtml(modelId: string, unavailable: boolean): string {
  const unavailableAttr = unavailable ? ` ${wire.MODEL_UNAVAILABLE_ATTR}="true"` : "";
  const actionAttr = unavailable
    ? ""
    : ` ${wire.ACTION_DATA_ATTR}="${wire.ACTION_MODEL}" ${wire.MODEL_ID_DATA_ATTR}="${modelId}"`;
  return `<div${unavailableAttr}${actionAttr} class="model-row"><span class="model-row-marker">${unavailable ? "✗" : "●"}</span><span class="model-row-model">${modelId}</span></div>`;
}

/** A window with the session page shell + picker button installed. */
function pageShell(): Window {
  const window = new Window({ url: "http://localhost/web/session/00000000-0000-0000-0000-000000000000" });
  window.document.body.innerHTML = `
    <div id="${wire.PAGE_ROOT_ID}" ${wire.PAGE_SESSION_DATA_ATTR}="00000000-0000-0000-0000-000000000000">
      <main id="main">
        <div id="titlebar">
          <span id="${wire.MODEL_PICKER_ID}">
            <button id="${wire.MODEL_PICKER_BUTTON_ID}" type="button">current-model</button>
          </span>
        </div>
      </main>
    </div>`;
  globalThis.document = window.document as unknown as typeof globalThis.document;

  requests.length = 0;
  pendingPulls.length = 0;
  modalResponse = modalHtml("", "");
  buttonResponse = `<button id="${wire.MODEL_PICKER_BUTTON_ID}" type="button">swapped-model</button>`;

  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    const method = init?.method ?? "GET";
    requests.push({ method, url });
    if (method === "GET" && url.startsWith(wire.MODEL_PICKER_PREFIX)) {
      // Capture the body NOW — at request time — so a later `settle`
      // replays exactly what the server would have answered then.
      // That is what makes the stale/newer distinction observable.
      const body = modalResponse;
      return new Promise<Response>((resolve) => {
        pendingPulls.push(() => resolve(new Response(body, { status: 200 })));
      });
    }
    if (method === "POST" && url === wire.MODEL_PATH) {
      return Promise.resolve(new Response(buttonResponse, { status: 200 }));
    }
    if (method === "POST" && url === wire.MODELS_REFRESH_PATH) {
      return Promise.resolve(new Response(buttonResponse, { status: 200 }));
    }
    return Promise.resolve(new Response("", { status: 404 }));
  }) as typeof fetch;

  return window;
}

/** Settles one pending pull with the current modal response. */
async function settlePull(): Promise<void> {
  const resolve = pendingPulls.shift();
  assert.ok(resolve, "a pull is in flight");
  resolve(modalResponse);
  await new Promise((resolveTick) => setTimeout(resolveTick, 0));
}

/**
 * Clicks the element through the region's handler — the same call the
 * boot module's dispatch loop makes (`regionOf` → `onClick`). A DOM
 * `click()` alone reaches nothing: the listeners live on `document`
 * in `main.ts`, which these tests do not boot.
 */
function click(target: unknown): void {
  modelPickerRegion.onClick!(el(target));
}

test("clicking the button pulls the modal fragment and shows it", async () => {
  const window = pageShell();
  const button = window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!;

  click(button);
  await settlePull();

  const modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
  assert.equal(modal.hidden, false, "the modal is visible after the pull");
  const firstPull = requests[0];
  assert.ok(firstPull, "the button click issued a pull");
  assert.ok(
    firstPull.url.startsWith(wire.MODEL_PICKER_PREFIX),
    `the pull targets the picker prefix: ${firstPull.url}`,
  );
});

test("a filter keystroke re-pulls with the filter as a query parameter", async () => {
  const window = pageShell();
  click(window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!);
  await settlePull();

  modalResponse = modalHtml("lam", rowHtml("openai/gpt-4o", false));
  const input = window.document.getElementById(wire.MODEL_PICKER_FILTER_ID)!;
  (input as unknown as { value: string }).value = "lam";
  assert.equal(modelPickerRegion.onInput!(el(input)), true, "the region claims the input");
  await settlePull();

  const filterPulls = requests.filter(
    (request) =>
      request.method === "GET" && request.url.includes(`${wire.MODEL_FILTER_QUERY_PARAM}=lam`),
  );
  assert.equal(filterPulls.length, 1, `one filtered pull: ${JSON.stringify(requests)}`);
  const modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
  assert.ok(modal.innerHTML.includes("openai/gpt-4o"), "the narrowed rows swapped in");
});

test("a stale pull response never clobbers a newer one (latest-wins)", async () => {
  const window = pageShell();
  click(window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!);
  await settlePull();

  // The open pull landed (the input exists in the DOM). Two rapid
  // keystrokes now issue two pulls — both in flight, none answered.
  modalResponse = modalHtml("a", rowHtml("first/older", false));
  const input = window.document.getElementById(wire.MODEL_PICKER_FILTER_ID)!;
  (input as unknown as { value: string }).value = "a";
  modelPickerRegion.onInput!(el(input));

  modalResponse = modalHtml("ab", rowHtml("second/newer", false));
  (input as unknown as { value: string }).value = "ab";
  modelPickerRegion.onInput!(el(input));

  // Two pulls are in flight ("a", "ab"). They answer out of order:
  // the FIRST pull to answer is the one for the OLDER keystroke —
  // already stale by issue time (a newer request superseded it) —
  // and must not render. The modal is re-resolved per settle: the
  // swap replaces the node, so the LIVE modal is the assertion's
  // subject, never a detached one.
  await settlePull();
  let modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
  assert.ok(
    !modal.innerHTML.includes("first/older"),
    "the stale response must not render",
  );

  // The newest pull answers last — its rows are the ones shown.
  await settlePull();
  modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
  assert.ok(
    modal.innerHTML.includes("second/newer"),
    "the newer response's rows are shown",
  );
});

test("the models push re-pulls with the typed filter and keeps focus", async () => {
  const window = pageShell();
  click(window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!);
  await settlePull();

  modalResponse = modalHtml("ll", rowHtml("ollama/llama3", false));
  const input = window.document.getElementById(wire.MODEL_PICKER_FILTER_ID)!;
  (input as unknown as { value: string }).value = "ll";
  modelPickerRegion.onInput!(el(input));
  await settlePull();
  (input as unknown as { focus(): void }).focus();

  // When the models signal arrives (empty payload — a signal).
  modelPickerRegion.sseHandlers[wire.SSE_EVENTS.models]!({
    data: "",
    lastEventId: "0",
  });
  await settlePull();

  // Then the re-pull carried the SAME filter and the rows swapped in.
  const lastPull = [...requests]
    .reverse()
    .find((request) => request.method === "GET");
  assert.ok(
    lastPull?.url.includes(`${wire.MODEL_FILTER_QUERY_PARAM}=ll`),
    `the push re-pulled with the typed filter: ${lastPull?.url}`,
  );
  const modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
  assert.equal(modal.hidden, false, "the modal stayed open");
  assert.ok(modal.innerHTML.includes("ollama/llama3"), "the fresh rows landed");
  assert.equal(
    window.document.activeElement?.id,
    wire.MODEL_PICKER_FILTER_ID,
    "the filter input kept focus across the push",
  );
});

test("clicking an available row posts the switch and closes the modal", async () => {
  const window = pageShell();
  // The modal (with the row) is canned BEFORE the open pull so the
  // pull's request-time capture serves the row markup.
  modalResponse = modalHtml("", rowHtml("openai/gpt-4o", false));
  click(window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!);
  await settlePull();

  const row = window.document.getElementById(wire.MODEL_PICKER_ROWS_ID)!.firstElementChild!;
  click(row);
  await new Promise((resolve) => setTimeout(resolve, 0));

  const switchPost = requests.find(
    (request) => request.method === "POST" && request.url === wire.MODEL_PATH,
  );
  assert.ok(switchPost, "the switch POST fired");
  assert.equal(
    window.document.getElementById(wire.MODEL_PICKER_MODAL_ID),
    null,
    "the modal is gone after the switch (the button swap replaced it)",
  );
  const picker = window.document.getElementById(wire.MODEL_PICKER_ID)!;
  assert.ok(
    picker.innerHTML.includes("swapped-model"),
    "the button fragment swapped in",
  );
});

test("clicking an unavailable row posts nothing", async () => {
  const window = pageShell();
  modalResponse = modalHtml("", rowHtml("anthropic/claude", true));
  click(window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!);
  await settlePull();

  const row = window.document.getElementById(wire.MODEL_PICKER_ROWS_ID)!.firstElementChild!;
  click(row);
  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.ok(
    !requests.some((request) => request.method === "POST" && request.url === wire.MODEL_PATH),
    "an unavailable row must not fire the switch POST",
  );
});

test("Escape closes the open modal", async () => {
  const window = pageShell();
  click(window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!);
  await settlePull();

  const modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
  assert.equal(modal.hidden, false);
  modelPickerRegion.onKeyDown!(
    el(modal),
    { key: "Escape" } as unknown as KeyboardEvent,
  );
  assert.equal(modal.hidden, true, "Esc closed the modal");
});

test("an outside click closes the open modal; an inside click does not", async () => {
  const window = pageShell();
  click(window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!);
  await settlePull();

  const modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
  const inner = modal.firstElementChild!;

  // An inside click (a row's whitespace) must NOT close — the
  // selector dispatch consumed it (onClick ran) and the document
  // dispatch sees the target inside the modal's box.
  click(inner);
  assert.equal(
    modelPickerRegion.onDocumentClick!(el(inner)),
    false,
    "an inside click is not a dismissal",
  );
  assert.equal(modal.hidden, false, "an inside click keeps the modal open");

  // The backdrop click (the overlay itself — outside the inner box)
  // closes.
  assert.equal(
    modelPickerRegion.onDocumentClick!(el(modal)),
    true,
    "the outside click dismisses",
  );
  assert.equal(modal.hidden, true, "the outside click closed the modal");
});
