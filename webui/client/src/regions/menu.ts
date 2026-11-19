//! The menu region — the titlebar hamburger and its "Sessions" panel
//! (client half of `crates/slices/jinn-webui/src/regions/menu.rs`).
//!
//! Owns:
//! - the hamburger click → toggle the panel's visibility (the panel
//!   ships `hidden` from the server, so a fresh page and every page
//!   swap boot with the menu closed),
//! - the "Sessions" item → close the menu and toggle the drawer-open
//!   class on `#page-root` — the drawer's carrier is the page root,
//!   never `body`: the page swap replaces `#page-root`'s outerHTML
//!   with a fresh server render that cannot carry the class, so
//!   switching sessions structurally resets the drawer (a class on
//!   `body` would survive the swap and load the next session under a
//!   phantom drawer),
//! - Escape at ANY keyboard focus → close the menu when open. This
//!   rides the document-level keydown hook (`onDocumentKeyDown`), not
//!   the selector dispatch: the selector path resolves the target
//!   through `targetWithAttribute` and a region's selectors, which
//!   fails for focus on `body` — the common case. When the menu is
//!   closed the hook declines, so other regions' Escape consumers
//!   (the rename editor's revert, the picker modal's close) are never
//!   fought.
//!
//! No width branching lives here (no `matchMedia`, no `innerWidth`):
//! the item toggles the drawer-open class at every width, and the
//! drawer's own media query decides whether that class does anything.
//! At desktop widths the class matches nothing and the observable
//! effect of the item is "the menu closed" — CSS owns width, alone.
//!
//! Ownership contract: this module renders what the server pushed and
//! holds no application state — open/closed is view-local DOM (the
//! panel's `hidden` attribute, the class on the page root), reset
//! structurally by any page swap. See `regions/mod.ts`.

import * as wire from "../wire.js";
import { targetElement } from "../dom.js";
import type { RegionModule } from "./mod.js";

/** The menu region's client surface. */
export const menuRegion: RegionModule = {
  id: "menu",
  selectors: [`#${wire.MENU_ID}`],

  onClick(target) {
    const origin = targetElement(target);
    if (origin === null) {
      return false;
    }
    if (origin.closest(`#${wire.MENU_BUTTON_ID}`) !== null) {
      toggleMenu();
      return true;
    }
    const item = origin.closest(`#${wire.MENU_ITEM_SESSIONS_ID}`);
    if (item !== null) {
      closeMenu();
      toggleDrawer();
      return true;
    }
    return false;
  },

  onDocumentKeyDown(event) {
    if (event.key !== "Escape") {
      return false;
    }
    // Only when the menu is open — a closed menu must never consume
    // an Escape another region's consumer is waiting on (the rename
    // editor's revert, the picker modal's close).
    const panel = byPanel();
    if (panel === null || panel.hidden) {
      return false;
    }
    closeMenu();
    return true;
  },

  sseHandlers: {},
};

/** Toggles the panel's visibility — the menu's open/closed state. */
function toggleMenu(): void {
  const panel = byPanel();
  if (panel === null) {
    return;
  }
  panel.hidden = !panel.hidden;
  byButton()?.setAttribute("aria-expanded", panel.hidden ? "false" : "true");
}

/** Closes the panel (idempotent — closing a closed menu is a no-op). */
function closeMenu(): void {
  const panel = byPanel();
  if (panel !== null) {
    panel.hidden = true;
  }
  byButton()?.setAttribute("aria-expanded", "false");
}

/**
 * Toggles the drawer-open class on `#page-root` — the drawer's only
 * state hook. The carrier is the page root so the activate swap (a
 * fresh server render of the root div) resets it structurally.
 */
function toggleDrawer(): void {
  const root = document.getElementById(wire.PAGE_ROOT_ID);
  if (root === null) {
    return;
  }
  root.classList.toggle(wire.DRAWER_OPEN_CLASS);
}

/** Resolves the panel, or null when the page carries no menu. */
function byPanel(): HTMLElement | null {
  return document.getElementById(wire.MENU_PANEL_ID);
}

/** Resolves the hamburger button, or null when absent. */
function byButton(): HTMLElement | null {
  return document.getElementById(wire.MENU_BUTTON_ID);
}
