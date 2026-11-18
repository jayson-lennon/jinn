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

/** The canned status-wrapper fragment the refresh/switch POSTs resolve with. */
let statusResponse = "";

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
  return `<div id="${wire.MODEL_PICKER_MODAL_ID}" class="model-picker-modal" hidden>
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
          <span id="${wire.MODEL_STATUS_ID}" class="model-status">
            <span class="m">current-model</span>
            <span id="${wire.MODEL_PICKER_ID}" class="model-picker">
              <button id="${wire.MODEL_PICKER_BUTTON_ID}" type="button">current-model</button>
            </span>
          </span>
        </div>
        <span id="${wire.MODEL_PICKER_MODAL_HOST_ID}"></span>
      </main>
    </div>`;
  globalThis.document = window.document as unknown as typeof globalThis.document;

  requests.length = 0;
  pendingPulls.length = 0;
  modalResponse = modalHtml("", "");
  statusResponse = `<span id="${wire.MODEL_STATUS_ID}" class="model-status">
    <span class="m">swapped-model</span>
    <span id="${wire.MODEL_PICKER_ID}" class="model-picker">
      <button id="${wire.MODEL_PICKER_BUTTON_ID}" type="button">swapped-model</button>
    </span>
  </span>`;

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
      return Promise.resolve(new Response(statusResponse, { status: 200 }));
    }
    if (method === "POST" && url === wire.MODELS_REFRESH_PATH) {
      return Promise.resolve(new Response(statusResponse, { status: 200 }));
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
  // The button survives its own click: the modal lands in the MODAL
  // host, the button's host is untouched.
  const buttonHost = window.document.getElementById(wire.MODEL_PICKER_ID)!;
  assert.ok(
    buttonHost.innerHTML.includes(wire.MODEL_PICKER_BUTTON_ID),
    "the button's host still carries the button",
  );
  assert.ok(
    window.document.getElementById(wire.MODEL_PICKER_MODAL_HOST_ID)!.innerHTML.includes(
      wire.MODEL_PICKER_MODAL_ID,
    ),
    "the modal landed in the modal host",
  );
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
  const modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
  assert.ok(modal !== null, "the modal stays in its host (dormant markup)");
  assert.equal(modal.hidden, true, "the modal is closed after the switch");
  const status = window.document.getElementById(wire.MODEL_STATUS_ID)!;
  assert.ok(
    status.innerHTML.includes("swapped-model"),
    "the status wrapper swapped in (button label fresh)",
  );
  assert.equal(
    status.querySelectorAll(".m").length,
    1,
    "the label span swapped with the button",
  );
  assert.ok(
    (status.querySelector(".m") as unknown as { textContent: string }).textContent.includes(
      "swapped-model",
    ),
    "the .m label carries the new model",
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

test("repeated open/close cycles keep the button intact and reopen fresh", async () => {
  const window = pageShell();
  const button = window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!;

  // Two full cycles: open → Esc close → open again.
  for (const cycle of [1, 2]) {
    click(button);
    await settlePull();
    const modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
    assert.ok(modal !== null, `cycle ${cycle}: the modal rendered`);
    assert.equal(modal.hidden, false, `cycle ${cycle}: the modal is open`);
    assert.ok(
      window.document.getElementById(wire.MODEL_PICKER_ID)!.innerHTML.includes(
        wire.MODEL_PICKER_BUTTON_ID,
      ),
      `cycle ${cycle}: the button's host still carries the button`,
    );
    modelPickerRegion.onKeyDown!(el(modal), { key: "Escape" } as unknown as KeyboardEvent);
    assert.equal(modal.hidden, true, `cycle ${cycle}: Esc closed the modal`);
  }
});

test("a filter keystroke keeps the modal open across the swap", async () => {
  const window = pageShell();
  click(window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!);
  await settlePull();

  modalResponse = modalHtml("lam", rowHtml("openai/gpt-4o", false));
  const input = window.document.getElementById(wire.MODEL_PICKER_FILTER_ID)!;
  (input as unknown as { value: string }).value = "lam";
  modelPickerRegion.onInput!(el(input));
  await settlePull();

  const modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
  assert.equal(modal.hidden, false, "the keystroke's settled swap kept the modal open");
  // Leave the modal closed — the module's open flag is view-local
  // state shared across tests in this file.
  modelPickerRegion.onKeyDown!(el(modal), { key: "Escape" } as unknown as KeyboardEvent);
});

test("the caret survives a filter swap", async () => {
  const window = pageShell();
  click(window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!);
  await settlePull();

  modalResponse = modalHtml("lama", rowHtml("ollama/llama3", false));
  const input = window.document.getElementById(wire.MODEL_PICKER_FILTER_ID)!;
  (input as unknown as { value: string }).value = "lama";
  const duck = input as unknown as { selectionStart?: number | null };
  duck.selectionStart = 2; // The caret sits mid-word before the keystroke's pull.
  modelPickerRegion.onInput!(el(input));
  await settlePull();

  const fresh = window.document.getElementById(wire.MODEL_PICKER_FILTER_ID)! as unknown as {
    value: string;
    selectionStart?: number;
  };
  assert.equal(fresh.value, "lama", "the filter text survived the swap");
  assert.equal(
    window.document.activeElement?.id,
    wire.MODEL_PICKER_FILTER_ID,
    "the fresh input kept focus",
  );
  assert.equal(
    fresh.selectionStart,
    2,
    "the caret was restored to its pre-swap position",
  );
  // Leave the modal closed for the tests that follow.
  const modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
  modelPickerRegion.onKeyDown!(el(modal), { key: "Escape" } as unknown as KeyboardEvent);
});

test("refresh shows the spinner until the models push resolves it", async () => {
  const window = pageShell();
  click(window.document.getElementById(wire.MODEL_PICKER_BUTTON_ID)!);
  await settlePull();

  // The spinner state is asserted against the button CURRENTLY in the
  // modal host — every settled swap replaces the node, so a stale
  // reference would test a detached element.
  const spinnerOn = (): { disabled: boolean; busy: string | null } => {
    const element = window.document.getElementById(wire.MODEL_PICKER_REFRESH_ID)!;
    const duck = element as unknown as {
      hasAttribute(name: string): boolean;
      getAttribute(name: string): string | null;
    };
    return { disabled: duck.hasAttribute("disabled"), busy: duck.getAttribute("aria-busy") };
  };

  click(window.document.getElementById(wire.MODEL_PICKER_REFRESH_ID)!);
  await new Promise((resolve) => setTimeout(resolve, 0));

  // The POST is in flight: spinner on.
  assert.equal(spinnerOn().disabled, true, "the refresh button disabled");
  assert.equal(spinnerOn().busy, "true", "the refresh button is busy");

  // The POST settles (the modal is open — the reply is ignored).
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(spinnerOn().disabled, true, "still spinning after the POST settled");
  assert.equal(spinnerOn().busy, "true", "still busy after the POST settled");

  // The `models` push lands: spinner off, rows re-pulled with the
  // filter intact, modal still open.
  modalResponse = modalHtml("", rowHtml("new/model", false));
  modelPickerRegion.sseHandlers[wire.SSE_EVENTS.models]!({ data: "", lastEventId: "0" });
  await settlePull();

  assert.equal(spinnerOn().disabled, false, "the push released the spinner flag");
  const modal = window.document.getElementById(wire.MODEL_PICKER_MODAL_ID) as unknown as HTMLElement;
  assert.equal(modal.hidden, false, "the modal stayed open through the push");
  const modalHost = window.document.getElementById(wire.MODEL_PICKER_MODAL_HOST_ID)!;
  assert.ok(modalHost.innerHTML.includes("new/model"), "the fresh rows landed");
  // Leave the modal closed for the tests that follow.
  modelPickerRegion.onKeyDown!(el(modal), { key: "Escape" } as unknown as KeyboardEvent);
});

test("refresh with the modal closed swaps the status wrapper", async () => {
  const window = pageShell();
  // The modal is closed (never opened): the refresh button is
  // driven by a click on a stub carrying the refresh action.
  const stub = window.document.createElement("button");
  stub.id = wire.MODEL_PICKER_REFRESH_ID;
  stub.setAttribute(wire.ACTION_DATA_ATTR, wire.ACTION_REFRESH_MODELS);
  window.document.body.appendChild(stub);

  click(stub);
  await new Promise((resolve) => setTimeout(resolve, 0));

  const status = window.document.getElementById(wire.MODEL_STATUS_ID)!;
  assert.ok(
    status.innerHTML.includes("swapped-model"),
    "the closed-modal refresh swapped the status wrapper (label fresh)",
  );
});
