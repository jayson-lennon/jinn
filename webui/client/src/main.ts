//! The client entry point: event delegation over the page's stable
//! regions, the page swap on session switch, and the stream rebind.
//!
//! There is no state module and none may be added. The server is
//! authoritative; the DOM and the URL are the client's only memory.
//! The viewed session's id is read from the page's data attribute
//! every time it is needed — a swap can change it under any closure,
//! so nothing may cache it.

import * as api from "./api.js";
import * as render from "./render.js";
import * as sse from "./sse.js";
import * as wire from "./wire.js";

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
 * Narrows an event target to an Element without `instanceof` — the
 * DOM globals the type system assumes are not installed in every
 * host the client's tests run under (happy-dom scopes its classes
 * per window). `closest` is the Element API actually needed; its
 * presence is the capability that matters.
 */
function targetElement(target: EventTarget | null): Element | null {
  const candidate = target as { closest?: Element["closest"] } | null;
  return typeof candidate?.closest === "function"
    ? (target as Element)
    : null;
}

/**
 * Narrows an event target to an HTMLElement without `instanceof` —
 * `getAttribute` (the action-attribute read) is the capability that
 * matters.
 */
function targetWithAttribute(target: EventTarget | null): Element | null {
  const candidate = target as { getAttribute?: Element["getAttribute"] } | null;
  return typeof candidate?.getAttribute === "function"
    ? (target as Element)
    : null;
}

/**
 * Replaces #page-root with the activated session's page and rebinds
 * everything: the old stream closes inside {@link sse.follow}, the
 * composer's button listeners re-delegate from the root (delegation
 * survives the swap by construction), and the new stream opens on
 * the new session.
 *
 * @param sessionId - The session whose page was fetched.
 * @param html - The WHOLE page HTML from the activate endpoint.
 */
function swapPage(sessionId: string, html: string): void {
  const root = document.getElementById(wire.PAGE_ROOT_ID);
  if (root === null) {
    throw new Error(`client target #${wire.PAGE_ROOT_ID} is missing from the page`);
  }
  root.outerHTML = html;
  // The page shell carries the session id and cursor; follow() closes
  // the previous EventSource and opens the new session's.
  sse.follow(sessionId, rebindComposerActions);
  history.pushState(null, "", `${wire.SESSION_PAGE_PATH}${sessionId}`);
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
async function submitComposer(sessionId: string): Promise<void> {
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
  render.replaceControls(replacement);
  rebindComposerActions(sessionId);
  const fresh = composerText();
  if (fresh !== null) {
    (fresh.element as unknown as { value: string }).value = "";
  }
}

/**
 * Wires Enter-to-send on the composer's textarea (the submit path —
 * there is no form element to submit; the button click and the Enter
 * key both funnel here).
 *
 * @param sessionId - The session the composer currently targets.
 */
function rebindComposerActions(sessionId: string): void {
  const composer = composerText();
  if (composer !== null) {
    composer.element.addEventListener("keydown", (event) => {
      const key = (event as KeyboardEvent).key;
      if (key === "Enter" && !(event as KeyboardEvent).shiftKey) {
        event.preventDefault();
        void submitComposer(sessionId).catch(reportFailure);
      }
    });
  }
}

/** Reports a failed action loudly — the anti-silent-failure rule. */
function reportFailure(error: unknown): void {
  console.error("[webui] action failed:", error);
}

/**
 * Binds the page's delegated behaviors. All listeners live on
 * elements the session-page swap does NOT destroy (document for
 * delegation) or are re-registered by {@link rebindComposerActions}
 * after each swap.
 */
export function initialize(): void {
  // Session-row clicks: delegation on document, matching the
  // contract's class hook, reading the row's session id attribute.
  document.addEventListener("click", (event) => {
    const origin = targetElement(event.target);
    if (origin === null) {
      return;
    }
    const row = origin.closest(`.${wire.SESSION_ROW_CLASS}`);
    if (row === null) {
      return;
    }
    const sessionId = row.getAttribute(wire.SESSION_ID_DATA_ATTR);
    if (sessionId === null) {
      console.error(`[webui] session row missing its ${wire.SESSION_ID_DATA_ATTR}`);
      return;
    }
    event.preventDefault();
    api.activateSession(sessionId)
      .then((html) => swapPage(sessionId, html))
      .catch(reportFailure);
  });

  // Composer button clicks (send/cancel) and the model select:
  // delegated on document because the controls and picker fragments
  // are replaced wholesale by SSE events at any moment.
  document.addEventListener("click", (event) => {
    const target = targetWithAttribute(event.target);
    if (target === null) {
      return;
    }
    const action = target.getAttribute(wire.ACTION_DATA_ATTR);
    if (action === null) {
      return;
    }
    const sessionId = viewedSessionId();
    if (sessionId === null) {
      return;
    }
    if (action === wire.ACTION_SEND) {
      event.preventDefault();
      void submitComposer(sessionId).catch(reportFailure);
      return;
    }
    if (action === wire.ACTION_CANCEL) {
      event.preventDefault();
      api.cancelTurn(sessionId)
        .then(render.replaceControls)
        .then(() => rebindComposerActions(sessionId))
        .catch(reportFailure);
    }
  });

  document.addEventListener("change", (event) => {
    const target = targetWithAttribute(event.target);
    if (target === null) {
      return;
    }
    if (target.getAttribute(wire.ACTION_DATA_ATTR) !== wire.ACTION_MODEL) {
      return;
    }
    // The model action only ever rides the contract's select; the
    // duck-type read of `value` keeps this host-agnostic (the
    // instanceof global is not installed under happy-dom).
    const select = target as { value?: string };
    const modelId = typeof select.value === "string" ? select.value : "";
    const sessionId = viewedSessionId();
    if (sessionId === null || modelId.length === 0) {
      return;
    }
    api.switchModel(sessionId, modelId)
      .then((html) => {
        document.getElementById(wire.MODEL_PICKER_ID)!.innerHTML = html;
      })
      .catch(reportFailure);
  });

  // The stream for whatever session this page renders. The landing
  // page has none; a later swap opens one.
  const sessionId = viewedSessionId();
  if (sessionId !== null) {
    sse.follow(sessionId, rebindComposerActions);
  }
}

// ES modules execute after the document is parsed, so the DOM is
// ready here — no DOMContentLoaded listener (one might miss the
// event; this cannot).
initialize();
