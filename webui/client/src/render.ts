//! Fragment insertion — the four swaps the client performs.
//!
//! One function per SSE event kind, each resolving its target by the
//! contract's id and throwing when the page lacks it. A missing
//! target must be a loud console error, never the silent
//! `targetError` no-op that shipped once: these functions are the
//! referential integrity test's runtime half.

import * as wire from "./wire.js";

/** Resolves an element by id or throws naming what the client wanted. */
function byId(id: string): HTMLElement {
  const element = document.getElementById(id);
  if (element === null) {
    throw new Error(`client target #${id} is missing from the page`);
  }
  return element;
}

/**
 * The data attribute marking an entry the client rendered locally
 * (optimistic) rather than receiving from the server. Its presence
 * is the dedupe signal for {@link appendChat}.
 */
const OPTIMISTIC_ATTR = "data-webui-optimistic";

/**
 * Renders user text as text — element.textContent, never HTML
 * interpolation. The optimistic entry bypasses the server's escaping,
 * so it must escape itself.
 */
function escapeText(text: string): string {
  const span = document.createElement("span");
  span.textContent = text;
  return span.innerHTML;
}

/**
 * Appends the just-sent user entry immediately — the send endpoint
 * accepts the message long before the fan-out's first stream token,
 * and an invisible message reads as a dead input. Carries
 * {@link OPTIMISTIC_ATTR}; the next server push that covers the same
 * entry reconciles it (see {@link appendChat}).
 *
 * @param sessionId - The session the entry belongs to (present but
 * unused today; kept so callers state the target explicitly).
 * @param text - The user's message, verbatim.
 * @throws Error when #transcript is missing.
 */
export function appendOptimisticUserEntry(_sessionId: string, text: string): void {
  const html =
    `<article class="entry user" ${OPTIMISTIC_ATTR}>` +
    `<div class="who">user</div><div class="body">${escapeText(text)}</div></article>`;
  byId(wire.TRANSCRIPT_ID).insertAdjacentHTML("beforeend", html);
  byId(wire.TRANSCRIPT_WRAP_ID).scrollTo({ top: Number.MAX_SAFE_INTEGER });
}

/**
 * Appends one streamed chat fragment to the transcript. The HTML is
 * server-rendered and server-escaped; the client never interpolates
 * user text into it. When the server's fragment covers the tail
 * optimistic entry (its user text arrived again), the optimistic
 * copy is dropped first so the entry never doubles.
 *
 * @param html - The server-rendered entry markup.
 * @throws Error when #transcript is missing.
 */
export function appendChat(html: string): void {
  const transcript = byId(wire.TRANSCRIPT_ID);
  const optimistic = transcript.querySelector(`[${OPTIMISTIC_ATTR}]`);
  if (optimistic !== null && html.includes(`class="entry user"`)) {
    optimistic.remove();
  }
  transcript.insertAdjacentHTML("beforeend", html);
}

/**
 * Replaces the whole transcript — the server's convergence push.
 * Re-bases the inline cursor attribute on the event's cursor so a
 * reconnect resumes correctly.
 *
 * @param html - The full server-rendered transcript markup.
 * @param cursor - The event's cursor (0 on convergence pushes).
 * @throws Error when #transcript is missing.
 */
export function replaceTranscript(html: string, cursor: number): void {
  const transcript = byId(wire.TRANSCRIPT_ID);
  // A wholesale replace also discards any optimistic tail entry —
  // the server's render is canonical from here.
  transcript.innerHTML = html;
  transcript.setAttribute(wire.TRANSCRIPT_CURSOR_DATA_ATTR, String(cursor));
}

/**
 * Replaces the composer-controls fragment — the send↔cancel swap.
 *
 * @param html - The server-rendered controls markup.
 * @throws Error when #composer-controls is missing.
 */
export function replaceControls(html: string): void {
  byId(wire.CONTROLS_ID).innerHTML = html;
}

/**
 * Replaces the sessions list.
 *
 * @param html - The server-rendered sessions markup.
 * @throws Error when #sessions is missing.
 */
export function replaceSessions(html: string): void {
  byId(wire.SESSIONS_ID).innerHTML = html;
}

/**
 * Scrolls the transcript wrap to its bottom — called after chat
 * appends so a reader keeps watching the stream tail.
 *
 * @throws Error when the transcript wrap is missing.
 */
export function scrollTranscriptToEnd(): void {
  byId(wire.TRANSCRIPT_WRAP_ID).scrollTo({ top: Number.MAX_SAFE_INTEGER });
}
