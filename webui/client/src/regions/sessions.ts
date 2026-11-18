//! The sessions region — the sidebar's session list (client half of
//! `crates/slices/jinn-webui/src/regions/sessions.rs`).
//!
//! Owns: the row-click activation (which swaps the whole page — the
//! sessions region is the only region whose action navigates rather
//! than swaps in place), the sidebar filter input (every text change
//! re-pulls the fragment with `?q=`; matching is server-side — this
//! module never compares titles), and the SSE `sessions` swap.
//!
//! While a filter is active, a pushed `sessions` event triggers a
//! re-pull carrying the same `q` instead of trusting the pushed HTML:
//! pushed fragments render unfiltered, and swapping one in would
//! un-filter the list under the user's eyes.
//!
//! Ownership contract: this module renders what the server pushed and
//! owns nothing (see `regions/mod.ts`). The viewed session id and the
//! filter text are both read fresh from the page at handler time —
//! view-local DOM state; a page swap replaces the input and thereby
//! resets the filter.

import * as api from "../api.js";
import * as wire from "../wire.js";
import { reportFailure, targetElement, targetWithAttribute } from "../dom.js";
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
  selectors: [
    `.${wire.SESSION_ROW_CLASS}`,
    `#${wire.SESSIONS_FILTER_INPUT_ID}`,
  ],

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

  onInput(target) {
    const candidate = targetWithAttribute(target);
    if (candidate === null) {
      return false;
    }
    if (candidate.getAttribute("id") !== wire.SESSIONS_FILTER_INPUT_ID) {
      return false;
    }
    refilter();
    return true;
  },

  sseHandlers: {
    [wire.SSE_EVENTS.sessions]: (event) => {
      if (currentFilter() === null) {
        replaceSessions(event.data);
        return;
      }
      // A filter is active: the pushed fragment is unfiltered, so
      // re-pull carrying the same q rather than swapping the push in.
      refilter();
    },
  },
};

/**
 * Re-pulls the sessions fragment with the input's current text and
 * swaps it in. An empty or missing input means no filter — the
 * unfiltered list.
 */
function refilter(): void {
  api.pullSessions(currentFilter())
    .then(replaceSessions)
    .catch(reportFailure);
}

/**
 * The filter text, read fresh from the input at handler time. Null
 * when no filter is in force — the input is absent or empty.
 */
function currentFilter(): string | null {
  const input = document.getElementById(wire.SESSIONS_FILTER_INPUT_ID);
  if (input === null) {
    return null;
  }
  const holder = input as { value?: unknown };
  const value = typeof holder.value === "string" ? holder.value : "";
  return value.length > 0 ? value : null;
}

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
