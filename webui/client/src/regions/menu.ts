//! The menu region — the titlebar hamburger, its "Sessions" slide-in
//! panel, and the drawer-open controller (client half of
//! `crates/slices/jinn-webui/src/regions/menu.rs`).
//!
//! Owns:
//! - the hamburger click → open the panel (a full-height slide-in
//!   from the left edge, at every viewport width) by clearing its
//!   `hidden` attribute and showing the menu scrim; close reverses
//!   both (the panel ships `hidden` from the server, so a fresh page
//!   and every page swap boot with the menu closed),
//! - the "Sessions" item → close the menu and toggle the drawer-open
//!   class on `#page-root` — the drawer's carrier is the page root,
//!   never `body`: the page swap replaces `#page-root`'s outerHTML
//!   with a fresh server render that cannot carry the class, so
//!   switching sessions structurally resets the drawer (a class on
//!   `body` would survive the swap and load the next session under a
//!   phantom drawer). The static scrim element lives INSIDE the page
//!   root for the same reason, and this module mirrors the class onto
//!   the scrim's `hidden` attribute so the two state hooks move as
//!   one,
//! - the drawer scrim click → close the drawer (this module owns the
//!   drawer's whole controller, the scrim included — the scrim's
//!   dismissal target is the drawer-open toggle it already owns),
//! - the menu scrim click → close the menu (same ownership logic, one
//!   scrim per overlay, both scrims inside `#page-root`),
//! - Escape at ANY keyboard focus → close whichever overlay is open —
//!   panel-then-drawer in one press if both are (in practice they
//!   never intentionally co-open; the menu scrim stacks above the
//!   drawer stack). This rides the document-level keydown hook
//!   (`onDocumentKeyDown`), not the selector dispatch: the selector
//!   path resolves the target through `targetWithAttribute` and a
//!   region's selectors, which fails for focus on `body` — the common
//!   case. When neither is open the hook declines, so other regions'
//!   Escape consumers (the rename editor's revert, the picker modal's
//!   close) are never fought.
//!
//! No width branching lives here (no `matchMedia`, no `innerWidth`):
//! the item toggles the drawer-open class at every width, and the
//! drawer's own media query decides whether that class does anything.
//! At desktop widths the class matches nothing and the observable
//! effect of the item is "the menu closed" — CSS owns width, alone.
//!
//! Ownership contract: this module renders what the server pushed and
//! holds no application state — open/closed is view-local DOM (the
//! panel's `hidden` attribute, the class on the page root, the
//! scrim's `hidden`), reset structurally by any page swap. See
//! `regions/mod.ts`.

import * as wire from "../wire.js";
import { targetElement } from "../dom.js";
import { openNewSessionModal } from "./session_new.js";
import type { RegionModule } from "./mod.js";

/** The menu region's client surface. */
export const menuRegion: RegionModule = {
  id: "menu",
  selectors: [`#${wire.MENU_ID}`, `#${wire.DRAWER_SCRIM_ID}`, `#${wire.MENU_SCRIM_ID}`],

  onClick(target) {
    const origin = targetElement(target);
    if (origin === null) {
      return false;
    }
    if (origin.closest(`#${wire.MENU_BUTTON_ID}`) !== null) {
      openMenu();
      return true;
    }
    const item = origin.closest(`#${wire.MENU_ITEM_SESSIONS_ID}`);
    if (item !== null) {
      closeMenu();
      toggleDrawer();
      return true;
    }
    // The new-session item: close the menu, then hand the open to
    // the new-session region — the modal is that region's fragment
    // and host span, and this region's selectors own `#menu` only.
    if (origin.closest(`#${wire.MENU_ITEM_NEW_SESSION_ID}`) !== null) {
      closeMenu();
      openNewSessionModal();
      return true;
    }
    // Each scrim sits outside every other region's DOM and inside the
    // page root; the click delegation only reaches a scrim when it is
    // the event's own target (its overlay covers everything else
    // while open). Dismissing is a scrim's one job.
    if (origin.closest(`#${wire.DRAWER_SCRIM_ID}`) !== null) {
      closeDrawer();
      return true;
    }
    if (origin.closest(`#${wire.MENU_SCRIM_ID}`) !== null) {
      closeMenu();
      return true;
    }
    return false;
  },

  onDocumentKeyDown(event) {
    if (event.key !== "Escape") {
      return false;
    }
    // Close whatever is open — both, in one press, when both are —
    // and decline when neither is, so a closed menu+drawer never
    // consumes an Escape another region's consumer is waiting on (the
    // rename editor's revert, the picker modal's close).
    const menuWasOpen = isMenuOpen();
    const drawerWasOpen = isDrawerOpen();
    if (!menuWasOpen && !drawerWasOpen) {
      return false;
    }
    if (menuWasOpen) {
      closeMenu();
    }
    if (drawerWasOpen) {
      closeDrawer();
    }
    return true;
  },

  sseHandlers: {},
};

/** Whether the menu panel is currently open. */
function isMenuOpen(): boolean {
  const panel = byPanel();
  return panel !== null && !panel.hidden;
}

/**
 * Whether the drawer is currently open — the drawer-open class on the
 * page root, read fresh at handler time (the class IS the state; no
 * module-level boolean shadows it).
 */
function isDrawerOpen(): boolean {
  const root = document.getElementById(wire.PAGE_ROOT_ID);
  return root !== null && root.classList.contains(wire.DRAWER_OPEN_CLASS);
}

/** Opens the panel and shows the menu scrim (idempotent). */
function openMenu(): void {
  const panel = byPanel();
  if (panel === null) {
    return;
  }
  panel.hidden = false;
  byButton()?.setAttribute("aria-expanded", "true");
  menuScrim().hidden = false;
}

/** Closes the panel and hides the menu scrim (idempotent). */
function closeMenu(): void {
  const panel = byPanel();
  if (panel !== null) {
    panel.hidden = true;
  }
  byButton()?.setAttribute("aria-expanded", "false");
  menuScrim().hidden = true;
}

/** Resolves the menu scrim, or an inert stand-in when absent. */
function menuScrim(): { hidden: boolean } {
  const scrim = document.getElementById(wire.MENU_SCRIM_ID);
  return scrim ?? { hidden: true };
}

/**
 * Toggles the drawer-open class on `#page-root` — the drawer's state
 * hook — and mirrors the new state onto the static scrim's `hidden`
 * attribute so the overlay shows exactly while the drawer does. The
 * carrier is the page root (the scrim lives inside it) so the
 * activate swap — a fresh server render of the root div — resets both
 * structurally.
 */
function toggleDrawer(): void {
  const root = document.getElementById(wire.PAGE_ROOT_ID);
  if (root === null) {
    return;
  }
  root.classList.toggle(wire.DRAWER_OPEN_CLASS);
  syncScrim(root.classList.contains(wire.DRAWER_OPEN_CLASS));
}

/** Closes the drawer: the class off the page root, the scrim hidden. */
function closeDrawer(): void {
  const root = document.getElementById(wire.PAGE_ROOT_ID);
  if (root !== null) {
    root.classList.remove(wire.DRAWER_OPEN_CLASS);
  }
  syncScrim(false);
}

/**
 * Shows the scrim exactly while the drawer is open. `hidden` is the
 * server-rendered default; the phone media query gives the element
 * its full-page overlay look, and this attribute is the switch.
 */
function syncScrim(open: boolean): void {
  const scrim = document.getElementById(wire.DRAWER_SCRIM_ID);
  if (scrim !== null) {
    (scrim as { hidden: boolean }).hidden = !open;
  }
}

/** Resolves the panel, or null when the page carries no menu. */
function byPanel(): HTMLElement | null {
  return document.getElementById(wire.MENU_PANEL_ID);
}

/** Resolves the hamburger button, or null when absent. */
function byButton(): HTMLElement | null {
  return document.getElementById(wire.MENU_BUTTON_ID);
}
