//! The SSE stream subscription — one EventSource per viewed session,
//! dispatched through the region registry.
//!
//! Catch-up and reconnection are the browser's and the server's jobs:
//! `EventSource` reconnects natively carrying `Last-Event-ID`, and
//! the stream endpoint re-sends everything after that cursor before
//! going live. The client adds no cursor bookkeeping of its own —
//! the inline data-webui-cursor attribute on the transcript
//! and the native reconnect header are the whole cursor story.
//!
//! This module names no region: the event listeners come from the
//! registry's `sseHandlers` (see `regions/mod.ts`), so adding a
//! region with an SSE event never edits this file.

import * as wire from "./wire.js";
import { regions } from "./regions/mod.js";

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
 */
export function follow(sessionId: string): void {
  if (currentSource !== null) {
    currentSource.close();
  }

  const source = new EventSource(`${wire.STREAM_PATH}${encodeURIComponent(sessionId)}`);

  // One listener per event name the regions registered. A region's
  // handler resolves its own targets per event — nothing is bound to
  // pre-swap nodes.
  for (const name of new Set(regions().flatMap((region) => Object.keys(region.sseHandlers)))) {
    source.addEventListener(name, (event) => {
      const message = event as MessageEvent<string>;
      for (const region of regions()) {
        const handler = region.sseHandlers[name];
        if (handler !== undefined) {
          handler({
            data: message.data,
            lastEventId: message.lastEventId ?? "0",
          });
        }
      }
    });
  }

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
