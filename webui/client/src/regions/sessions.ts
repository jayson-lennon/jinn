//! The sessions region — the sidebar's session list plus the titlebar
//! title editor (client half of
//! `crates/slices/jinn-webui/src/regions/sessions.rs`).
//!
//! Owns: the row-click activation (which swaps the whole page — the
//! sessions region is the only region whose action navigates rather
//! than swaps in place), the sidebar filter input (every text change
//! re-pulls the fragment with `?q=`; matching is server-side — this
//! module never compares titles), the SSE `sessions` swap, and the
//! inline rename editor hosted by the titlebar's title span (the
//! server renders the span with the contract id on session pages
//! only; the landing page's "jinn" title carries no id and is inert).
//! The editor is view-local DOM: click opens a pre-filled input
//! inside the span, Enter or clicking away commits, Escape reverts,
//! and an empty-after-trim commit reverts locally without a request —
//! the server already refuses empty with a 400, and a refused rename
//! leaves the title unchanged either way.
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
//! resets the filter. The rename editor's transient input and its
//! marker attributes are view-local DOM state too.

import * as api from "../api.js";
import * as wire from "../wire.js";
import { reportFailure, targetElement, targetWithAttribute } from "../dom.js";
import type { RegionModule } from "./mod.js";

/**
 * The attribute an open editor input carries the pre-edit title in —
 * what Escape and a failed commit revert the display to. Client-local
 * (the server never renders it), the same class of view-local marker
 * as the transcript's optimistic-entry attribute.
 */
const PREV_TITLE_ATTR = "data-webui-prev-title";

/**
 * The attribute marking an input whose commit is already in flight —
 * the guard that keeps the blur-fired second commit off the wire when
 * Enter already started one.
 */
const COMMITTING_ATTR = "data-webui-committing";

/** The tab-title suffix the page shell renders (`<title> — jinn`). */
const TAB_TITLE_SUFFIX = " — jinn";

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
    `#${wire.SESSION_TITLE_ID}`,
    `.${wire.SESSION_ROW_CLASS}`,
    `#${wire.SESSIONS_FILTER_INPUT_ID}`,
  ],

  onClick(target) {
    const origin = targetElement(target);
    if (origin === null) {
      return false;
    }
    // The title editor: claimed only when the click lands inside the
    // titlebar's title span. A span holding an open input resolves as
    // handled-and-inert — every click inside an editing span (the
    // input's own clicks included) must not re-open or commit.
    const span = origin.closest(`#${wire.SESSION_TITLE_ID}`);
    if (span !== null) {
      if (span.querySelector("input") === null) {
        openTitleEditor(span);
      }
      return true;
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

  onKeyDown(target, event) {
    const candidate = targetWithAttribute(target);
    if (candidate === null) {
      return false;
    }
    const span = candidate.closest(`#${wire.SESSION_TITLE_ID}`);
    if (span === null || candidate.getAttribute(PREV_TITLE_ATTR) === null) {
      return false;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      commitTitleEditor(span);
      return true;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      revertTitleEditor(span);
      return true;
    }
    return false;
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

/**
 * Swaps the title span's text for an inline input seeded with the
 * displayed title (caret at the end — the TUI rename's seeding), and
 * binds the click-away commit on the input's blur. The span keeps its
 * contract id, so the delegation selectors keep resolving while
 * editing.
 *
 * @param span - The titlebar's title span (the `#session-title` host).
 */
export function openTitleEditor(span: Element): void {
  const current = span.textContent ?? "";
  const input = document.createElement("input");
  input.type = "text";
  input.value = current;
  input.setAttribute(PREV_TITLE_ATTR, current);
  span.textContent = "";
  span.appendChild(input);
  input.addEventListener("blur", () => {
    commitTitleEditor(span);
  });
  input.focus();
  const end = current.length;
  input.setSelectionRange(end, end);
}

/**
 * Commits the open editor: an empty-after-trim title reverts locally
 * (no request — the server refuses empty with a 400 anyway); a real
 * title posts the rename, and on success re-renders the titlebar
 * text, the browser tab title, and the sidebar from the POST's
 * sessions fragment. A refused commit reverts the display to the
 * pre-edit title and reports the failure — never a silent no-op.
 *
 * @param span - The titlebar's title span hosting the editor.
 */
export function commitTitleEditor(span: Element): void {
  const input = span.querySelector("input");
  if (input === null || input.hasAttribute(COMMITTING_ATTR)) {
    return;
  }
  input.setAttribute(COMMITTING_ATTR, "");
  const prev = input.getAttribute(PREV_TITLE_ATTR) ?? "";
  const raw = input.value;
  const title = raw.trim();
  if (title.length === 0) {
    renderTitleText(span, prev);
    return;
  }
  const sessionId = viewedSessionId();
  if (sessionId === null) {
    renderTitleText(span, prev);
    return;
  }
  api.renameSession(sessionId, raw)
    .then((html) => {
      renderTitleText(span, title);
      setTabTitle(title);
      replaceSessions(html);
    })
    .catch((error) => {
      renderTitleText(span, prev);
      reportFailure(error);
    });
}

/**
 * Closes the editor without committing — Escape. The display returns
 * to the pre-edit title.
 *
 * @param span - The titlebar's title span hosting the editor.
 */
export function revertTitleEditor(span: Element): void {
  const input = span.querySelector("input");
  if (input === null) {
    return;
  }
  renderTitleText(span, input.getAttribute(PREV_TITLE_ATTR) ?? "");
}

/**
 * Renders the title span's text, discarding any open editor input.
 *
 * @param span - The titlebar's title span.
 * @param title - The title to display.
 */
function renderTitleText(span: Element, title: string): void {
  span.textContent = title;
}

/**
 * Updates the browser tab title in the page shell's format.
 *
 * @param title - The viewed session's title.
 */
function setTabTitle(title: string): void {
  document.title = `${title}${TAB_TITLE_SUFFIX}`;
}

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

/** Resolves the sessions container or throws naming the target. */
function bySessionsId(): HTMLElement {
  const element = document.getElementById(wire.SESSIONS_ID);
  if (element === null) {
    throw new Error(`client target #${wire.SESSIONS_ID} is missing from the page`);
  }
  return element;
}
