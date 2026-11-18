//! The model-picker region — the titlebar button, its modal, and the
//! two hosts they swap into (client half of
//! `crates/slices/jinn-webui/src/regions/model_picker.rs`).
//!
//! Owns:
//! - the button click → pull the modal fragment into the **modal
//!   host** (`#model-picker-modal-host`, a sibling span in the page
//!   shell) and show it — the button's own host is never touched, so
//!   opening the modal cannot destroy the button that opened it,
//! - the filter input → one GET pull per keystroke, each carrying the
//!   current filter; responses settle latest-wins (a stale response is
//!   discarded — a newer request's result must never be clobbered),
//!   and each settled swap re-shows the modal while it is open and
//!   restores the filter's focus + caret (the server renders the input
//!   `hidden`, so an unhiding pass is part of every settle),
//! - the refresh button → POST the refresh action with a spinner
//!   (`disabled` + `aria-busy`) that stays until the `models` SSE
//!   event lands; the typed filter and focus survive the push,
//! - a row click → the existing switch POST; the modal closes,
//! - Esc and outside-click → close.
//!
//! The `models` SSE event is a **signal, not rows**: the handler
//! re-pulls the modal with the current filter text, so the server
//! stays authoritative for every list the page shows. The only
//! view-local state is the bookkeeping the view needs (the in-flight
//! sequence latest-wins requires, the open flag the re-show requires,
//! the refresh-in-flight flag the spinner requires) — permitted by the
//! region ownership contract; no shared store, no cached session id.
//!
//! Ownership contract: this module renders what the server pushed;
//! everything else is view-local DOM state (see `regions/mod.ts`).

import * as api from "../api.js";
import * as wire from "../wire.js";
import { reportFailure, targetElement, targetWithAttribute } from "../dom.js";
import type { RegionModule } from "./mod.js";

/**
 * The modal pull sequence — bumped on every issued request. A
 * response applying a stale sequence is discarded; only the newest
 * issued request may render. This is the latest-wins gate.
 */
let pullSequence = 0;

/** Whether this module issued the in-flight switch POST. */
let switching = false;

/**
 * Whether the modal is conceptually open — the intent the swap-settle
 * applies. The server renders every modal fragment `hidden`, so the
 * client's open flag is what keeps a filter re-pull from closing the
 * popup it serves.
 */
let modalOpen = false;

/** Whether this module issued the in-flight refresh POST (spinner). */
let refreshing = false;

/** The model-picker region's client surface. */
export const modelPickerRegion: RegionModule = {
  id: "model-picker",
  selectors: [`#${wire.MODEL_PICKER_ID}`, `#${wire.MODEL_PICKER_MODAL_HOST_ID}`],

  onClick(target) {
    const candidate = targetWithAttribute(target);
    const action = candidate?.getAttribute(wire.ACTION_DATA_ATTR);
    if (action === wire.ACTION_MODEL) {
      switchModel(candidate as HTMLElement);
      return true;
    }
    if (action === wire.ACTION_REFRESH_MODELS) {
      refreshModels(candidate as HTMLElement);
      return true;
    }
    const element = candidate ?? targetElement(target);
    if (element?.id === wire.MODEL_PICKER_BUTTON_ID) {
      openModal();
      return true;
    }
    return false;
  },

  onInput(target) {
    const element = targetElement(target);
    if (element?.id !== wire.MODEL_PICKER_FILTER_ID) {
      return false;
    }
    // The duck-type read of `value` keeps this host-agnostic (the
    // instanceof global is not installed under happy-dom).
    const input = element as { value?: unknown };
    pullRows(typeof input.value === "string" ? input.value : "", {
      preserveFocus: true,
      caret: caretOf(element),
    });
    return true;
  },

  onKeyDown(_target, event) {
    if (event.key !== "Escape") {
      return false;
    }
    const modal = byModal();
    if (modal === null || modal.hidden) {
      return false;
    }
    closeModal();
    return true;
  },

  onDocumentClick(target) {
    const modal = byModal();
    if (modal === null || modal.hidden) {
      return false;
    }
    // An outside click (the target sits outside the modal's inner
    // box) closes — the backdrop is the click's actual target.
    const inner = modal.firstElementChild;
    if (inner !== null && inner.contains(target)) {
      return false;
    }
    closeModal();
    return true;
  },

  sseHandlers: {
    [wire.SSE_EVENTS.models]() {
      // The spinner's reset condition: the cache landed (or discovery
      // finished storing one), so whatever re-render follows is fresh
      // — the re-pull below re-renders the refresh button without the
      // spinner state.
      refreshing = false;
      const modal = byModal();
      if (modal === null || modal.hidden || !modalOpen) {
        return;
      }
      // Signal, not rows: re-pull with the filter the user typed (and
      // the caret they typed it at). The input element is
      // server-rendered into the fragment; its value survives because
      // we echo the text back as the pull's filter and the server
      // stamps it into `value="…"`.
      const filter = byFilterValue();
      pullRows(filter, { preserveFocus: true, caret: caretOf(byFilter()) });
    },
  },
};

/**
 * Reads the filter input's caret position, host-agnostically: the
 * selection APIs are not installed under every test host, so their
 * presence is the capability that matters.
 */
function caretOf(input: unknown): number | null {
  const candidate = input as { selectionStart?: unknown } | null;
  return typeof candidate?.selectionStart === "number" ? candidate.selectionStart : null;
}

/** Opens the modal: pulls the fragment (unfiltered) and shows it. */
function openModal(): void {
  modalOpen = true;
  pullRows("", { openOnSettle: true, preserveFocus: true });
}

/** Closes the modal and drops any in-flight filter pull's authority. */
function closeModal(): void {
  modalOpen = false;
  const modal = byModal();
  if (modal !== null) {
    modal.hidden = true;
  }
}

/**
 * Pulls the modal fragment with the given filter, honoring latest-wins:
 * the response renders only if no newer request was issued meanwhile.
 * The fragment lands in the modal host — never the button's — and the
 * settle re-shows the modal while [`modalOpen`] holds, restoring the
 * filter's focus and (when known) its caret.
 */
function pullRows(
  filter: string,
  options: {
    openOnSettle?: boolean;
    preserveFocus?: boolean;
    caret?: number | null;
  } = {},
): void {
  const sequence = ++pullSequence;
  const sessionId = viewedSessionId();
  if (sessionId === null) {
    return;
  }
  api
    .pullModelPicker(sessionId, filter)
    .then((html) => {
      if (sequence !== pullSequence) {
        return; // A newer request superseded this one.
      }
      byModalHost().innerHTML = html;
      const modal = byModal();
      if (modal === null) {
        return;
      }
      if (modalOpen || options.openOnSettle) {
        modal.hidden = false;
      }
      if (options.preserveFocus) {
        restoreFilterFocus(options.caret ?? null);
      }
    })
    .catch(reportFailure);
}

/**
 * Focuses the filter input and restores the caret the user typed at —
 * guarded, because the selection APIs are not installed under every
 * host (the same duck-typing discipline [`caretOf`] applies).
 */
function restoreFilterFocus(caret: number | null): void {
  const input = byFilter();
  if (input === null) {
    return;
  }
  input.focus();
  if (caret !== null && typeof input.setSelectionRange === "function") {
    input.setSelectionRange(caret, caret);
  }
}

/**
 * The refresh action: POSTs the refresh command with the spinner on
 * (`disabled` + `aria-busy`) and keeps the modal open with its filter;
 * the spinner stays until the `models` SSE push lands — discovery
 * always ends in a cache write, so the push is the safe reset
 * condition. The response is the titlebar status wrapper; while the
 * modal is open it carries no new information (a refresh never
 * changes the selection) and is ignored so it cannot fight the
 * pending push.
 */
function refreshModels(button: HTMLElement): void {
  const sessionId = viewedSessionId();
  if (sessionId === null || refreshing) {
    return;
  }
  refreshing = true;
  button.setAttribute("disabled", "disabled");
  button.setAttribute("aria-busy", "true");
  api
    .refreshModels(sessionId)
    .then((html) => {
      if (!modalOpen) {
        // Closed: the spinner is invisible; swap the status wrapper so
        // the titlebar stays fresh, and release the in-flight flag —
        // the next click must not be gated on a push this closed
        // modal is not waiting for.
        refreshing = false;
        byStatusId().innerHTML = html;
      }
      // Open: keep the spinner — the models push resolves it (and
      // swaps the rows) in one pass.
    })
    .catch((error: unknown) => {
      refreshing = false;
      button.removeAttribute("disabled");
      button.removeAttribute("aria-busy");
      reportFailure(error);
    });
}

/**
 * The switch action: POSTs the row's model and swaps the response (the
 * re-rendered status wrapper — label and button together) into the
 * titlebar's status element; the modal closes (hidden — the fragment
 * stays dormant in its host, and a stale modal must not survive the
 * switch it caused).
 */
function switchModel(row: HTMLElement): void {
  if (switching || row.hasAttribute(wire.MODEL_UNAVAILABLE_ATTR)) {
    return; // Unavailable rows are non-interactive; the server refuses them anyway.
  }
  const modelId = row.getAttribute(wire.MODEL_ID_DATA_ATTR);
  const sessionId = viewedSessionId();
  if (sessionId === null || modelId === null || modelId.length === 0) {
    return;
  }
  switching = true;
  api
    .switchModel(sessionId, modelId)
    .then((html) => {
      switching = false;
      byStatusId().innerHTML = html;
      closeModal();
    })
    .catch((error: unknown) => {
      switching = false;
      reportFailure(error);
    });
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

/** Resolves the titlebar's status wrapper or throws naming the target. */
function byStatusId(): HTMLElement {
  const element = document.getElementById(wire.MODEL_STATUS_ID);
  if (element === null) {
    throw new Error(`client target #${wire.MODEL_STATUS_ID} is missing from the page`);
  }
  return element;
}

/** Resolves the modal's host or throws naming the target. */
function byModalHost(): HTMLElement {
  const element = document.getElementById(wire.MODEL_PICKER_MODAL_HOST_ID);
  if (element === null) {
    throw new Error(`client target #${wire.MODEL_PICKER_MODAL_HOST_ID} is missing from the page`);
  }
  return element;
}

/** Resolves the modal overlay, or null when the page carries none. */
function byModal(): HTMLElement | null {
  return document.getElementById(wire.MODEL_PICKER_MODAL_ID);
}

/** Resolves the modal's filter input, or null when absent. */
function byFilter(): HTMLInputElement | null {
  return document.getElementById(wire.MODEL_PICKER_FILTER_ID) as HTMLInputElement | null;
}

/** The filter's current text (empty when the input is absent). */
function byFilterValue(): string {
  return byFilter()?.value ?? "";
}
