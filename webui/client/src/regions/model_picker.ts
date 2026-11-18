//! The model-picker region — the titlebar button and its modal
//! (client half of `crates/slices/jinn-webui/src/regions/model_picker.rs`).
//!
//! Owns:
//! - the button click → pull the modal fragment into the picker
//!   container and show it,
//! - the filter input → one GET pull per keystroke, each carrying the
//!   current filter; responses settle latest-wins (a stale response is
//!   discarded — a newer request's result must never be clobbered),
//! - the refresh button → POST the refresh action, then await the
//!   `models` SSE event; the typed filter and focus survive the push,
//! - a row click → the existing switch POST; the button fragment swap
//!   closes the modal,
//! - Esc and outside-click → close.
//!
//! The `models` SSE event is a **signal, not rows**: the handler
//! re-pulls the modal with the current filter text, so the server
//! stays authoritative for every list the page shows. The only
//! view-local state is the in-flight-request bookkeeping latest-wins
//! needs (permitted by the region ownership contract) — no shared
//! store, no cached session id.
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

/** The model-picker region's client surface. */
export const modelPickerRegion: RegionModule = {
  id: "model-picker",
  selectors: [`#${wire.MODEL_PICKER_ID}`],

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
    pullRows(typeof input.value === "string" ? input.value : "");
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
      const modal = byModal();
      if (modal === null || modal.hidden) {
        return;
      }
      // Signal, not rows: re-pull with the filter the user typed. The
      // input element is server-rendered into the fragment; its value
      // survives because we echo the text back as the pull's filter
      // and the server stamps it into `value="…"`.
      const filter = byFilterValue();
      pullRows(filter, { preserveFocus: true });
    },
  },
};

/** Opens the modal: pulls the fragment (unfiltered) and shows it. */
function openModal(): void {
  pullRows("", { openOnSettle: true });
}

/** Closes the modal and drops any in-flight filter pull's authority. */
function closeModal(): void {
  const modal = byModal();
  if (modal !== null) {
    modal.hidden = true;
  }
}

/**
 * Pulls the modal fragment with the given filter, honoring latest-wins:
 * the response renders only if no newer request was issued meanwhile.
 */
function pullRows(
  filter: string,
  options: { openOnSettle?: boolean; preserveFocus?: boolean } = {},
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
      byPickerId().innerHTML = html;
      const modal = byModal();
      if (modal !== null) {
        if (options.openOnSettle) {
          modal.hidden = false;
        }
        if (options.preserveFocus) {
          byFilter()?.focus();
        }
      }
    })
    .catch(reportFailure);
}

/**
 * The refresh action: POSTs the refresh command (returning the current
 * button fragment immediately) and keeps the modal open with its
 * filter; the rows update when the `models` SSE push lands.
 */
function refreshModels(button: HTMLElement): void {
  const sessionId = viewedSessionId();
  if (sessionId === null) {
    return;
  }
  button.setAttribute("disabled", "disabled");
  api
    .refreshModels(sessionId)
    .then((html) => {
      button.removeAttribute("disabled");
      // The response is the titlebar button fragment — swap the
      // button's own markup (the parent span), not the modal.
      const buttonHost = document.getElementById(wire.MODEL_PICKER_ID);
      if (buttonHost !== null) {
        buttonHost.innerHTML = html;
      }
    })
    .catch((error: unknown) => {
      button.removeAttribute("disabled");
      reportFailure(error);
    });
}

/**
 * The switch action: POSTs the row's model and swaps the response (the
 * re-rendered button) into the picker container; the modal closes — a
 * stale modal must not survive the switch it caused.
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
      byPickerId().innerHTML = html;
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

/** Resolves the picker container or throws naming the target. */
function byPickerId(): HTMLElement {
  const element = document.getElementById(wire.MODEL_PICKER_ID);
  if (element === null) {
    throw new Error(`client target #${wire.MODEL_PICKER_ID} is missing from the page`);
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
