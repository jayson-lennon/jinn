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
 * Appends one streamed chat fragment to the transcript. The HTML is
 * server-rendered and server-escaped; the client never interpolates
 * user text into it.
 *
 * @param html - The server-rendered entry markup.
 * @throws Error when #transcript is missing.
 */
export function appendChat(html: string): void {
  byId(wire.TRANSCRIPT_ID).insertAdjacentHTML("beforeend", html);
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
