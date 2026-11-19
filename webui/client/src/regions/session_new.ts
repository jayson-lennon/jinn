//! The new-session region — the menu item, its creation modal, and
//! the create POST's navigation (client half of
//! `crates/slices/jinn-webui/src/regions/session_new.rs`).
//!
//! Owns:
//! - the menu item's click → close the menu (the same dismiss the
//!   sessions item shows) and pull the form fragment into the
//!   **new-session modal host** (a sibling span in the page shell —
//!   the picker's modal-host precedent, a second host so the two
//!   modals can never destroy each other),
//! - the lifecycle select's change → one GET re-pull carrying the
//!   chosen name; the response replaces the modal wholesale, so the
//!   args input appears exactly when the newly selected lifecycle's
//!   setup declares parameters (the server decides, never the
//!   client),
//! - the submit button → POST the form fields; the response is the
//!   NEW session's whole page — the activation response shape — and
//!   the navigation rides the sessions region's page-swap hook, the
//!   same mechanism a row-click activation uses (`bindPageSwap`).
//!
//! The menu region's selectors own `#menu` (where the item button
//! lives), so the delegated menu-item click resolves there first;
//! this module exports {@link openNewSessionModal} and `menu.ts` forwards the
//! item's click to it. No `menu.ts` selector changes.
//!
//! The refusal state (`#session-new-empty`) renders instead of the
//! form when no projects are configured; the client treats it as an
//! inert modal — no submit wiring exists to fight.
//!
//! Ownership contract: this module renders what the server pushed;
//! everything else is view-local DOM state (the modal's presence in
//! its host span; see `regions/mod.ts`).

import * as api from "../api.js";
import * as wire from "../wire.js";
import { reportFailure, targetElement } from "../dom.js";
import type { RegionModule } from "./mod.js";

/**
 * The new-session region's client surface. The selectors claim the
 * modal host span (the fragment swaps into it) — the menu item
 * itself lives inside `#menu`, which the menu region claims; the
 * item's click arrives here through the menu region's forward, not
 * through these selectors.
 */
export const sessionNewRegion: RegionModule = {
  id: "session-new",
  selectors: [`#${wire.SESSION_NEW_MODAL_HOST_ID}`],

  onClick(target) {
    const origin = targetElement(target);
    if (origin === null) {
      return false;
    }
    if (origin.closest(`#${wire.SESSION_NEW_SUBMIT_ID}`) !== null) {
      submit();
      return true;
    }
    return false;
  },

  onChange(target) {
    const element = targetElement(target);
    if (element?.id !== wire.SESSION_NEW_LIFECYCLE_SELECT_ID) {
      return false;
    }
    // The duck-type read keeps this host-agnostic (the `instanceof`
    // global is not installed under happy-dom).
    const select = element as { value?: unknown };
    repull(typeof select.value === "string" ? select.value : "");
    return true;
  },

  onKeyDown(_target, event) {
    if (event.key !== "Escape") {
      return false;
    }
    if (byModal() === null) {
      return false;
    }
    closeModal();
    return true;
  },

  onDocumentClick(target) {
    const modal = byModal();
    if (modal === null) {
      return false;
    }
    // The click's own target must BE the modal overlay: the inner
    // card covers the form, so only the backdrop resolves here (the
    // picker modal's dismissal rule).
    if (targetElement(target) !== modal) {
      return false;
    }
    closeModal();
    return true;
  },

  sseHandlers: {},
};

/**
 * Opens the modal: closes the menu panel (the item's dismiss
 * behavior), pulls the blank-lifecycle form into the host span, and
 * focuses the project select. Exported for the menu region — the
 * item's click resolves to `#menu`'s selectors first, and the menu
 * region forwards it here (selectors untouched).
 */
export function openNewSessionModal(): void {
  closeMenuPanel();
  api
    .pullSessionNew("")
    .then((html) => {
      swapModal(html);
      byProject()?.focus();
    })
    .catch(reportFailure);
}

/**
 * Re-pulls the form for a lifecycle. The response replaces the
 * modal wholesale — the args input appears or disappears as the
 * server rendered it, and focus returns to the lifecycle select so
 * keyboard use survives the swap.
 */
function repull(lifecycle: string): void {
  api
    .pullSessionNew(lifecycle)
    .then((html) => {
      swapModal(html);
      byLifecycle()?.focus();
    })
    .catch(reportFailure);
}

/**
 * Submits the form: reads the three fields fresh from the DOM at
 * handler time (a page swap can change them under any closure), POSTs
 * them, and navigates to the answered page — the sessions region's
 * page-swap hook, the activation navigation. Refusals throw (the api
 * module's contract) and surface through {@link reportFailure}.
 */
function submit(): void {
  const project = byProject()?.value ?? "";
  const lifecycle = byLifecycle()?.value ?? "";
  const args = byArgs()?.value ?? "";
  if (project.length === 0) {
    // The refusal state renders no submit button, so an empty
    // project here means the form was swapped under the click; no
    // request leaves.
    return;
  }
  api
    .createSession(project, lifecycle, args)
    .then((html) => {
      const created = createdSessionId(html);
      closeModal();
      if (created !== null) {
        navigateTo(created, html);
      }
    })
    .catch(reportFailure);
}

/**
 * Extracts the created session's id from the answered page — the
 * page-root div's data attribute, the same attribute every page
 * carries (the activation response's shape).
 */
function createdSessionId(html: string): string | null {
  const holder = document.createElement("template");
  holder.innerHTML = html;
  const root = holder.content.getElementById(wire.PAGE_ROOT_ID);
  return root?.getAttribute(wire.PAGE_SESSION_DATA_ATTR) ?? null;
}

/** The page-swap hook `main.ts` installs at boot. */
let navigate: ((sessionId: string, html: string) => void) | null = null;

/**
 * Installs the navigation callback (boot-time wiring — `main.ts`
 * owns the swap mechanics; this region only reports the intent).
 *
 * @param swap - The callback run with the new session id and page HTML.
 */
export function bindNewSessionPageSwap(swap: (sessionId: string, html: string) => void): void {
  navigate = swap;
}

/** Reports the navigation to the orchestrator when the hook is set. */
function navigateTo(sessionId: string, html: string): void {
  if (navigate !== null) {
    navigate(sessionId, html);
  }
}

/** Replaces the host span's content with the (modal) fragment. */
function swapModal(html: string): void {
  const host = document.getElementById(wire.SESSION_NEW_MODAL_HOST_ID);
  if (host === null) {
    return;
  }
  host.innerHTML = html;
}

/** Closes the menu panel the way the sessions item does. */
function closeMenuPanel(): void {
  const panel = document.getElementById(wire.MENU_PANEL_ID);
  if (panel !== null) {
    panel.hidden = true;
  }
  const button = document.getElementById(wire.MENU_BUTTON_ID);
  button?.setAttribute("aria-expanded", "false");
  const scrim = document.getElementById(wire.MENU_SCRIM_ID);
  if (scrim !== null) {
    (scrim as { hidden: boolean }).hidden = true;
  }
}

/** Removes the modal from its host span (idempotent). */
function closeModal(): void {
  const host = document.getElementById(wire.SESSION_NEW_MODAL_HOST_ID);
  if (host !== null) {
    host.innerHTML = "";
  }
}

/** Resolves the modal overlay, or null when closed. */
function byModal(): HTMLElement | null {
  return document.getElementById(wire.SESSION_NEW_MODAL_ID);
}

/** Resolves the project select, or null when absent (refusal state). */
function byProject(): HTMLSelectElement | null {
  return document.getElementById(wire.SESSION_NEW_PROJECT_SELECT_ID) as
    | HTMLSelectElement
    | null;
}

/** Resolves the lifecycle select, or null when absent (refusal state). */
function byLifecycle(): HTMLSelectElement | null {
  return document.getElementById(wire.SESSION_NEW_LIFECYCLE_SELECT_ID) as
    | HTMLSelectElement
    | null;
}

/** Resolves the args input, or null when the lifecycle declares none. */
function byArgs(): HTMLInputElement | null {
  return document.getElementById(wire.SESSION_NEW_ARGS_INPUT_ID) as
    | HTMLInputElement
    | null;
}
