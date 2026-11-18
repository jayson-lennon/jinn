//! The composer region — send↔cancel controls + the message input
//! (client half of `crates/slices/jinn-webui/src/regions/composer.rs`).
//!
//! Owns: the action-attribute click delegation (send / cancel), the
//! Enter-to-send keydown on the textarea, the controls swap from the
//! SSE `controls` event and from POST responses, and the optimistic
//! nothing-clears-only-on-success send flow.
//!
//! Ownership contract: this module renders what the server pushed and
//! owns nothing (see `regions/mod.ts`). The text typed into the
//! textarea is view-local DOM state — a controls swap replaces it, by
//! design (the server's fragment is canonical). The viewed session id
//! is read fresh from the page at handler time; a swap can change it
//! under any closure, so nothing caches it.

import * as api from "../api.js";
import * as wire from "../wire.js";
import { reportFailure, targetWithAttribute } from "../dom.js";
import { appendOptimisticUserEntry } from "./transcript.js";
import type { RegionModule } from "./mod.js";

/** The composer region's client surface. */
export const composerRegion: RegionModule = {
  id: "composer",
  selectors: [
    `#${wire.COMPOSER_ID}`,
    `#${wire.CONTROLS_ID}`,
  ],

  onClick(target) {
    const candidate = targetWithAttribute(target);
    if (candidate === null) {
      return false;
    }
    const action = candidate.getAttribute(wire.ACTION_DATA_ATTR);
    if (action !== wire.ACTION_SEND && action !== wire.ACTION_CANCEL) {
      return false;
    }
    const sessionId = viewedSessionId();
    if (sessionId === null) {
      return true;
    }
    if (action === wire.ACTION_SEND) {
      void submit(sessionId).catch(reportFailure);
      return true;
    }
    api.cancelTurn(sessionId)
      .then(replaceControls)
      .then(() => bindComposerInput(sessionId))
      .catch(reportFailure);
    return true;
  },

  onKeyDown(target, event) {
    if (event.key !== "Enter" || event.shiftKey) {
      return false;
    }
    const candidate = targetWithAttribute(target);
    if (candidate === null || candidate.getAttribute("id") !== wire.INPUT_ID) {
      return false;
    }
    const sessionId = viewedSessionId();
    if (sessionId === null) {
      return true;
    }
    event.preventDefault();
    void submit(sessionId).catch(reportFailure);
    return true;
  },

  onPageSwap(sessionId) {
    // The swapped-in page's composer needs its Enter binding now — a
    // controls event may not arrive for a while (the first one rides
    // the next phase change). The keydown delegation is document-level
    // and survives swaps; this hook exists for parity with the old
    // per-swap rebinding and re-resolves nothing cached.
    void sessionId;
  },

  sseHandlers: {
    [wire.SSE_EVENTS.controls]: (event) => {
      replaceControls(event.data);
    },
  },
};

/**
 * The viewed session's id, read fresh from the page's data attribute.
 * Returns null on the landing page (no session).
 */
function viewedSessionId(): string | null {
  const root = document.getElementById(wire.PAGE_ROOT_ID);
  if (root === null) {
    return null;
  }
  const id = root.getAttribute(wire.PAGE_SESSION_DATA_ATTR);
  return id !== null && id.length > 0 ? id : null;
}

/**
 * Reads the composer's current text without `instanceof` (host
 * portability). Returns null when no textarea is present.
 */
function composerText(): { element: Element; value: string } | null {
  const input = document.getElementById(wire.INPUT_ID);
  if (input === null) {
    return null;
  }
  const holder = input as { value?: unknown };
  return typeof holder.value === "string"
    ? { element: input, value: holder.value }
    : null;
}

/**
 * Sends the composer's text. Reads the input fresh (the controls
 * swap replaces the textarea wholesale), clears it only on success,
 * and lets failures throw — a failed send must be visible.
 *
 * @param sessionId - The session to send into.
 */
async function submit(sessionId: string): Promise<void> {
  const composer = composerText();
  if (composer === null) {
    throw new Error(`client target #${wire.INPUT_ID} is missing from the page`);
  }
  const text = composer.value;
  if (text.trim().length === 0) {
    return;
  }
  // Optimistic nothing: the response replaces the controls (busy),
  // and the SSE `controls` event settles the race. Clear only when
  // the send was accepted, so a refusal keeps the user's text.
  const replacement = await api.sendMessage(sessionId, text);
  replaceControls(replacement);
  bindComposerInput(sessionId);
  const fresh = composerText();
  if (fresh !== null) {
    (fresh.element as unknown as { value: string }).value = "";
  }
  // Optimistic user entry (transcript-region-owned): the server
  // appends on enqueue, but the fan-out's chat push only rides the
  // first stream tokens — without this the message idles invisible
  // until the model responds. The next `transcript-full` convergence
  // push replaces the transcript wholesale.
  appendOptimisticUserEntry(sessionId, text);
}

/**
 * Replaces the composer-controls fragment — the send↔cancel swap.
 *
 * @param html - The server-rendered controls markup.
 * @throws Error when the controls container is missing.
 */
export function replaceControls(html: string): void {
  byControlsId().innerHTML = html;
}

/**
 * The Enter binding rides the document-level keydown delegation and
 * survives every swap by construction; this function exists so the
 * old rebind call sites (send success, cancel) keep a named step and
 * any future per-element binding lands in exactly one place.
 *
 * @param _sessionId - The session the composer currently targets.
 */
export function bindComposerInput(_sessionId: string): void {
  // Delegation handles it; nothing to do. See onKeyDown.
}

/** Resolves the controls container or throws naming the target. */
function byControlsId(): HTMLElement {
  const element = document.getElementById(wire.CONTROLS_ID);
  if (element === null) {
    throw new Error(`client target #${wire.CONTROLS_ID} is missing from the page`);
  }
  return element;
}
