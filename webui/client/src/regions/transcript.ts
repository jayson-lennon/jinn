//! The transcript region — the session's transcript (client half of
//! `crates/slices/jinn-webui/src/regions/transcript.rs`).
//!
//! Owns: the append/replace swaps, the cursor re-base on convergence
//! pushes, and the optimistic user entry — region-owned on purpose:
//! the optimistic entry exists solely for this region (the send flow
//! appends it; the region's own swaps reconcile it away), so it lives
//! here and not in a shared prim module.
//!
//! Ownership contract: this module renders what the server pushed and
//! owns nothing (see `regions/mod.ts`). The optimistic entry is
//! view-local DOM state, not application state — the next server push
//! covering it deletes it; nothing survives a reload.

import * as wire from "../wire.js";
import { byId, escapeText } from "../dom.js";
import type { RegionModule } from "./mod.js";

/**
 * The data attribute marking an entry the client rendered locally
 * (optimistic) rather than receiving from the server. Its presence
 * is the dedupe signal for {@link appendChat}.
 */
const OPTIMISTIC_ATTR = "data-webui-optimistic";

/** The transcript region's client surface. */
export const transcriptRegion: RegionModule = {
  id: "transcript",
  // No delegation selectors: the transcript renders pushes and has no
  // interactive surface of its own today.
  selectors: [],

  sseHandlers: {
    [wire.SSE_EVENTS.chat]: (event) => {
      // Empty data is the trailing cursor-advance frame of a
      // catch-up — nothing to append.
      if (event.data.length > 0) {
        appendChat(event.data);
        scrollTranscriptToEnd();
      }
    },
    [wire.SSE_EVENTS.transcriptFull]: (event) => {
      replaceTranscript(event.data, Number(event.lastEventId || "0"));
      scrollTranscriptToEnd();
    },
  },
};

/**
 * Appends one streamed chat fragment to the transcript. The HTML is
 * server-rendered and server-escaped; the client never interpolates
 * user text into it. When the server's fragment covers the tail
 * optimistic entry (its user text arrived again), the optimistic
 * copy is dropped first so the entry never doubles.
 *
 * @param html - The server-rendered entry markup.
 * @throws Error when the transcript container is missing.
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
 * reconnect resumes correctly. A wholesale replace also discards any
 * optimistic tail entry — the server's render is canonical from here.
 *
 * @param html - The full server-rendered transcript markup.
 * @param cursor - The event's cursor (0 on convergence pushes).
 * @throws Error when the transcript container is missing.
 */
export function replaceTranscript(html: string, cursor: number): void {
  const transcript = byId(wire.TRANSCRIPT_ID);
  transcript.innerHTML = html;
  transcript.setAttribute(wire.TRANSCRIPT_CURSOR_DATA_ATTR, String(cursor));
}

/**
 * Appends the just-sent user entry immediately — the send endpoint
 * accepts the message long before the fan-out's first stream token,
 * and an invisible message reads as a dead input. Carries
 * {@link OPTIMISTIC_ATTR}; the region's own swaps reconcile it (see
 * {@link appendChat} / {@link replaceTranscript}).
 *
 * @param _sessionId - The session the entry belongs to (present but
 * unused today; kept so callers state the target explicitly).
 * @param text - The user's message, verbatim (escaped here — the
 * optimistic entry bypasses the server's escaping).
 * @throws Error when the transcript container is missing.
 */
export function appendOptimisticUserEntry(_sessionId: string, text: string): void {
  const html =
    `<article class="entry user" ${OPTIMISTIC_ATTR}>` +
    `<div class="who">user</div><div class="body">${escapeText(text)}</div></article>`;
  byId(wire.TRANSCRIPT_ID).insertAdjacentHTML("beforeend", html);
  scrollTranscriptToEnd();
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
