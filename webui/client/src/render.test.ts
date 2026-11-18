//! The render tests: the four swaps, under happy-dom.
//!
//! Each test drives one `render.ts` function against a DOM built
//! from the page shell's shape and asserts the observable outcome —
//! what a connected browser would show.

import assert from "node:assert/strict";
import { test } from "node:test";

import { Window } from "happy-dom";

import * as wire from "./wire.js";
import * as render from "./render.js";

/**
 * A happy-dom window with the page shell installed as the active
 * document, plus the module-level `document` render.ts resolves
 * against.
 */
function pageShell(): Window {
  const window = new Window({ url: "http://localhost/web" });
  window.document.body.innerHTML = `
    <div id="${wire.PAGE_ROOT_ID}" ${wire.PAGE_SESSION_DATA_ATTR}="00000000-0000-0000-0000-000000000000">
      <nav id="sidebar"><h2>Sessions</h2><div id="${wire.SESSIONS_ID}"><span>old list</span></div></nav>
      <main id="main">
        <div id="titlebar"></div>
        <div id="${wire.TRANSCRIPT_WRAP_ID}">
          <div id="${wire.TRANSCRIPT_ID}" ${wire.TRANSCRIPT_CURSOR_DATA_ATTR}="0"></div>
        </div>
        <div id="${wire.CONTROLS_ID}">
          <div id="${wire.COMPOSER_ID}"><textarea id="${wire.INPUT_ID}"></textarea></div>
        </div>
        <span id="${wire.MODEL_PICKER_ID}"></span>
      </main>
    </div>`;
  // render.ts (and the browser runtime) resolve `document` globally.
  globalThis.document = window.document as unknown as typeof globalThis.document;
  return window;
}

test("appendChat inserts the fragment beforeend in order", () => {
  // Given the page shell with an empty transcript.
  const window = pageShell();

  // When appending two chat fragments in order.
  render.appendChat('<article class="entry user">one</article>');
  render.appendChat('<article class="entry assistant">two</article>');

  // Then the transcript holds both, in arrival order, inside the
  // transcript element — what a streaming reader sees.
  const transcript = window.document.getElementById(wire.TRANSCRIPT_ID);
  assert.ok(transcript, "transcript exists");
  const articles = transcript.querySelectorAll("article");
  assert.equal(articles.length, 2);
  assert.match(articles[0]!.textContent ?? "", /one/);
  assert.match(articles[1]!.textContent ?? "", /two/);
});

test("appendChat throws loudly when the transcript is missing", () => {
  // Given a page shell whose transcript was removed (a bad swap).
  const window = pageShell();
  window.document.getElementById(wire.TRANSCRIPT_ID)!.remove();

  // When appending a chat fragment.
  // Then the call throws naming the missing target — never a silent
  // no-op (the targetError class this project shipped once).
  assert.throws(
    () => render.appendChat("<article>lost</article>"),
    new RegExp(wire.TRANSCRIPT_ID),
  );
});

test("replaceTranscript swaps the content and re-bases the cursor", () => {
  // Given the page shell carrying an appended streaming entry.
  const window = pageShell();
  render.appendChat("<article>stale streaming tail</article>");

  // When the convergence push replaces the transcript.
  render.replaceTranscript('<article class="entry user">converged</article>', 4);

  // Then the stale tail is gone, the converged entry is present, and
  // the inline cursor carries the event's cursor.
  const transcript = window.document.getElementById(wire.TRANSCRIPT_ID);
  assert.ok(transcript, "transcript exists");
  assert.ok(!transcript.innerHTML.includes("stale streaming tail"));
  assert.ok(transcript.innerHTML.includes("converged"));
  assert.equal(transcript.getAttribute(wire.TRANSCRIPT_CURSOR_DATA_ATTR), "4");
});

test("replaceControls swaps the send button for the cancel fragment", () => {
  // Given the page shell with idle controls.
  const window = pageShell();

  // When replacing the controls with a busy fragment.
  render.replaceControls(
    `<div id="${wire.COMPOSER_ID}"><button data-action="${wire.ACTION_CANCEL}">Cancel</button></div>`,
  );

  // Then the composer now offers cancel — the busy swap landed.
  const controls = window.document.getElementById(wire.CONTROLS_ID);
  assert.ok(controls, "controls exist");
  assert.ok(controls.querySelector(`button[data-action="${wire.ACTION_CANCEL}"]`));
  assert.equal(controls.querySelector(`button[data-action="${wire.ACTION_SEND}"]`), null);
});

test("replaceSessions swaps the list wholesale", () => {
  // Given the page shell with an old list.
  const window = pageShell();

  // When replacing the sessions list.
  render.replaceSessions('<a class="srow">fresh row</a>');

  // Then only the fresh markup remains.
  const sessions = window.document.getElementById(wire.SESSIONS_ID);
  assert.ok(sessions, "sessions exist");
  assert.ok(!sessions.innerHTML.includes("old list"));
  assert.ok(sessions.innerHTML.includes("fresh row"));
});

test("scrollTranscriptToEnd reaches the wrap's bottom without throwing", () => {
  // Given the page shell with a tall transcript.
  pageShell();
  render.appendChat(`<article style="height:2000px">tall entry</article>`);

  // When scrolling to the end.
  // Then the call resolves (happy-dom stubs geometry; the contract
  // under test is "does not throw on the real id").
  render.scrollTranscriptToEnd();
});
