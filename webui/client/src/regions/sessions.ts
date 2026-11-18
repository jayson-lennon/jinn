//! The sessions region — the sidebar's session list (client half of
//! `crates/slices/jinn-webui/src/regions/sessions.rs`).
//!
//! Owns: the row-click activation (which swaps the whole page — the
//! sessions region is the only region whose action navigates rather
//! than swaps in place) and the SSE `sessions` list swap.
//!
//! Ownership contract: this module renders what the server pushed and
//! owns nothing (see `regions/mod.ts`). The viewed session id is read
//! fresh from the page at handler time.

import * as api from "../api.js";
import * as wire from "../wire.js";
import { reportFailure, targetElement } from "../dom.js";
import type { RegionModule } from "./mod.js";

/** The page-swap hook `main.ts` installs at boot. */
let onActivate: ((sessionId: string, html: string) => void) | null = null;

/**
 * Installs the page-swap callback (boot-time wiring — `main.ts` owns
 * the swap mechanics; the region only reports the intent).
 *
 * @param swap - The callback run with the new session id and page HTML.
 */
export function bindPageSwap(swap: (sessionId: string, html: string) => void): void {
  onActivate = swap;
}

/** The sessions region's client surface. */
export const sessionsRegion: RegionModule = {
  id: "sessions",
  selectors: [`.${wire.SESSION_ROW_CLASS}`],

  onClick(target) {
    const origin = targetElement(target);
    if (origin === null) {
      return false;
    }
    const row = origin.closest(`.${wire.SESSION_ROW_CLASS}`);
    if (row === null) {
      return false;
    }
    const sessionId = row.getAttribute(wire.SESSION_ID_DATA_ATTR);
    if (sessionId === null) {
      console.error(`[webui] session row missing its ${wire.SESSION_ID_DATA_ATTR}`);
      return true;
    }
    api.activateSession(sessionId)
      .then((html) => {
        if (onActivate !== null) {
          onActivate(sessionId, html);
        }
      })
      .catch(reportFailure);
    return true;
  },

  sseHandlers: {
    [wire.SSE_EVENTS.sessions]: (event) => {
      replaceSessions(event.data);
    },
  },
};

/**
 * Replaces the sessions list — the sidebar refresh.
 *
 * @param html - The server-rendered sessions markup.
 * @throws Error when the sessions container is missing.
 */
export function replaceSessions(html: string): void {
  bySessionsId().innerHTML = html;
}

/** Resolves the sessions container or throws naming the target. */
function bySessionsId(): HTMLElement {
  const element = document.getElementById(wire.SESSIONS_ID);
  if (element === null) {
    throw new Error(`client target #${wire.SESSIONS_ID} is missing from the page`);
  }
  return element;
}
