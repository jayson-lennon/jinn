//! The SSE stream subscription — one EventSource per viewed session.
//!
//! Catch-up and reconnection are the browser's and the server's jobs:
//! `EventSource` reconnects natively carrying `Last-Event-ID`, and
//! the stream endpoint re-sends everything after that cursor before
//! going live. The client adds no cursor bookkeeping of its own —
//! the inline data-webui-cursor attribute on #transcript
//! and the native reconnect header are the whole cursor story.

import * as render from "./render.js";
import * as wire from "./wire.js";

/** An open stream, kept so a session switch can close it. */
let currentSource: EventSource | null = null;

/**
 * Opens the stream for one session, closing any previous one first —
 * the page-swap rebind. The old source's listeners wrote into DOM
 * nodes the swap destroyed; leaving it open would append into
 * detached markup while the new session's stream never starts. This
 * is THE bug of the swap pattern, and closing here is the fix.
 *
 * @param sessionId - The session whose stream to subscribe.
 * @param onControls - A hook fired with the composer's session id
 * after each event, so bindings can re-resolve (the swap replaces
 * the buttons the listeners were written against).
 */
export function follow(
  sessionId: string,
  onControls: (sessionId: string) => void,
): void {
  if (currentSource !== null) {
    currentSource.close();
  }

  const source = new EventSource(`${wire.STREAM_PATH}${encodeURIComponent(sessionId)}`);

  // Chat: append one (or several, on catch-up) entry fragments.
  source.addEventListener(wire.SSE_EVENTS.chat, (event) => {
    const data = (event as MessageEvent<string>).data;
    if (data.length > 0) {
      render.appendChat(data);
      render.scrollTranscriptToEnd();
    }
  });

  // Transcript-full: the convergence push — replace wholesale.
  source.addEventListener(wire.SSE_EVENTS.transcriptFull, (event) => {
    const data = (event as MessageEvent<string>).data;
    const cursor = Number(event.lastEventId || "0");
    render.replaceTranscript(data, cursor);
    render.scrollTranscriptToEnd();
  });

  // Controls: the send↔cancel swap.
  source.addEventListener(wire.SSE_EVENTS.controls, (event) => {
    render.replaceControls((event as MessageEvent<string>).data);
    onControls(sessionId);
  });

  // Sessions: the sidebar refresh (a switch, archive, or load landed).
  source.addEventListener(wire.SSE_EVENTS.sessions, (event) => {
    render.replaceSessions((event as MessageEvent<string>).data);
  });

  // Errors: EventSource retries natively with Last-Event-ID, and the
  // server's catch-up covers the gap. Log so a dead stream is
  // discoverable; never attempt manual reconnect state here.
  source.onerror = () => {
    console.warn(`[webui] stream for ${sessionId} disconnected; reconnecting`);
  };

  currentSource = source;
}

/** Closes the open stream, if any — teardown for tests. */
export function closeStream(): void {
  if (currentSource !== null) {
    currentSource.close();
    currentSource = null;
  }
}
