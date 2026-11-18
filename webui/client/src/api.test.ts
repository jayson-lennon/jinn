//! Fetch error paths: a failed action must SURFACE. The shipped
//! failure mode was silent swallowing; these tests pin that
//! api.ts's helpers throw on non-2xx statuses and that a
//! session-row click's failure is reported, never lost.

import assert from "node:assert/strict";
import { test } from "node:test";

import { Window } from "happy-dom";

import * as api from "./api.js";
import * as wire from "./wire.js";

/** Installs a fetch that answers every call with the given status. */
function failingFetch(status: number): void {
  globalThis.fetch = (async () =>
    new Response("no", { status })) as unknown as typeof fetch;
}

test("sendMessage throws on a 4xx refusal instead of returning", async () => {
  // Given a server refusing sends with 400 (the empty-text rule).
  failingFetch(400);

  // When sending a message.
  // Then the promise rejects naming the endpoint and status — the
  // caller (and the console) must see the refusal.
  await assert.rejects(
    api.sendMessage("11111111-1111-1111-1111-111111111111", "hello"),
    /\/web\/send failed: HTTP 400/,
  );
});

test("cancelTurn throws on a network failure", async () => {
  // Given a fetch that rejects outright (the network path down).
  globalThis.fetch = (async () => {
    throw new Error("connection refused");
  }) as unknown as typeof fetch;

  // When cancelling a turn.
  // Then the rejection surfaces.
  await assert.rejects(api.cancelTurn("11111111-1111-1111-1111-111111111111"), /connection refused/);
});

test("switchModel throws on a 500", async () => {
  // Given a server erroring on model switches.
  failingFetch(500);

  // When switching the model.
  // Then the rejection names the endpoint and status.
  await assert.rejects(
    api.switchModel("11111111-1111-1111-1111-111111111111", "openai/gpt-4o"),
    /\/web\/model failed: HTTP 500/,
  );
});

test("activateSession throws on a non-2xx instead of swapping the page", async () => {
  // Given a server refusing the activate.
  failingFetch(500);

  // When activating a session.
  // Then the rejection surfaces before any page swap could consume
  // garbage HTML.
  await assert.rejects(
    api.activateSession("11111111-1111-1111-1111-111111111111"),
    /failed: HTTP 500/,
  );
});

test("renameSession posts urlencoded to the session's rename path", async () => {
  // Given a fetch recording the request and answering OK.
  const seen: { url: string; init: RequestInit | undefined }[] = [];
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    seen.push({ url: String(input), init });
    return new Response("<a>list</a>", { status: 200 });
  }) as unknown as typeof fetch;

  // When renaming.
  const body = await api.renameSession("11111111-1111-1111-1111-111111111111", "New Name");

  // Then the POST went to the session-page path + id + /rename with a
  // urlencoded title field, and the body text came back.
  const request = seen[0]!;
  assert.equal(request.init?.method, "POST");
  assert.equal(
    request.url,
    `${wire.SESSION_PAGE_PATH}11111111-1111-1111-1111-111111111111/rename`,
  );
  assert.equal(
    (request.init?.headers as Record<string, string>)["content-type"],
    "application/x-www-form-urlencoded",
  );
  assert.equal(request.init?.body, "title=New%20Name");
  assert.equal(body, "<a>list</a>");
});

test("renameSession throws on a 400 refusal", async () => {
  // Given a server refusing renames with 400 (the empty-title rule).
  failingFetch(400);

  // When renaming.
  // Then the rejection names the endpoint and status.
  await assert.rejects(
    api.renameSession("11111111-1111-1111-1111-111111111111", "  "),
    /\/rename failed: HTTP 400/,
  );
});

test("a session-row click with a failing activate reports the failure, not silence", async () => {
  // Given the landing page and a 500-ing activate endpoint, with the
  // real delegation path booted.
  const window = new Window({ url: "http://localhost/web" });
  window.document.body.innerHTML = `
    <div id="${wire.PAGE_ROOT_ID}">
      <nav id="sidebar"><div id="${wire.SESSIONS_ID}">
        <a class="${wire.SESSION_ROW_CLASS}" ${wire.SESSION_ID_DATA_ATTR}="11111111-1111-1111-1111-111111111111">Only</a>
      </div></nav>
    </div>`;
  globalThis.document = window.document as unknown as typeof globalThis.document;
  failingFetch(500);
  globalThis.EventSource = class {
    close(): void {}
    addEventListener(): void {}
  } as unknown as typeof EventSource;

  const errors: unknown[] = [];
  const original = console.error;
  console.error = (...parts: unknown[]) => errors.push(parts);

  try {
    // When clicking the row (the page must stay intact).
    await import("./main.js");
    const row = window.document.querySelector(`.${wire.SESSION_ROW_CLASS}`)!;
    row.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 0));

    // Then the failure was logged loudly — nothing swallowed.
    assert.equal(errors.length, 1, `console.error called once, got ${errors.length}`);

    // And the page root survived (no swap happened).
    assert.ok(window.document.getElementById(wire.PAGE_ROOT_ID), "page intact");
  } finally {
    console.error = original;
  }
});
