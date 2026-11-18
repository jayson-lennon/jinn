//! The client entry point — the page orchestrator.
//!
//! This module names no region. It boots the region registry, runs
//! the generic delegation loop (click / change / input / keydown,
//! dispatched to the first region whose selectors claim the target),
//! owns the page swap on session switch, and opens the stream.
//! Everything a region does lives in `regions/<id>.ts`; adding a
//! region never edits this file.
//!
//! There is no state module and none may be added. The server is
//! authoritative; the DOM and the URL are the client's only memory.

import * as dom from "./dom.js";
import * as sse from "./sse.js";
import * as wire from "./wire.js";
import { bindPageSwap } from "./regions/sessions.js";
import { regions, type RegionModule } from "./regions/mod.js";

/**
 * Extracts the page-root div from a whole-page HTML document (the
 * activate endpoint serves a full `<!DOCTYPE html>` document, not a
 * fragment). Assigning a whole document to `outerHTML` is invalid and
 * coerces unpredictably; the swap consumes exactly the root div.
 */
function extractPageRoot(html: string): string {
  const holder = document.createElement("template");
  holder.innerHTML = html;
  const root = holder.content.getElementById(wire.PAGE_ROOT_ID);
  if (root === null) {
    throw new Error(
      `the fetched page has no #${wire.PAGE_ROOT_ID} — contract violation`,
    );
  }
  return root.outerHTML;
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

/**
 * Replaces #page-root with the activated session's page-root div
 * (extracted from the full-document response) and rebinds everything:
 * the old stream closes inside {@link sse.follow}, the delegation
 * survives the swap by construction (document-level listeners over
 * region selectors), and the new stream opens on the new session.
 *
 * @param sessionId - The session whose page was fetched.
 * @param html - The WHOLE page HTML document from the activate endpoint.
 */
function swapPage(sessionId: string, html: string): void {
  const root = document.getElementById(wire.PAGE_ROOT_ID);
  if (root === null) {
    throw new Error(`client target #${wire.PAGE_ROOT_ID} is missing from the page`);
  }
  root.outerHTML = extractPageRoot(html);
  // The page shell carries the session id and cursor; follow() closes
  // the previous EventSource and opens the new session's.
  sse.follow(sessionId);
  // Regions re-resolve against the swapped-in page (the composer's
  // Enter binding is delegation and needs nothing, but a region with
  // a real post-swap bind lands it here).
  for (const region of regions()) {
    region.onPageSwap?.(sessionId);
  }
  history.pushState(null, "", `${wire.SESSION_PAGE_PATH}${sessionId}`);
}

/**
 * Finds the region whose selectors claim the target — first match
 * wins. Safe ONLY while the regions' selectors are pairwise disjoint
 * (the conformance test enforces it; an overlap would silently
 * change which region handles an event).
 */
function regionOf(target: Element): RegionModule | null {
  for (const region of regions()) {
    for (const selector of region.selectors) {
      if (target.closest(selector) !== null) {
        return region;
      }
    }
  }
  return null;
}

/** Dispatches one delegated event through the region registry. */
function delegateClickOrChange(
  target: EventTarget | null,
  kind: "click" | "change",
): void {
  const element = dom.targetElement(target);
  if (element === null) {
    return;
  }
  const region = regionOf(element);
  if (region === null) {
    return;
  }
  if (kind === "click") {
    region.onClick?.(element);
    return;
  }
  region.onChange?.(element);
}

/**
 * Dispatches one input event (every text-entry interaction: typing,
 * paste, cut) to the region claiming the target.
 */
function delegateInput(target: EventTarget | null): void {
  const element = dom.targetWithAttribute(target);
  if (element === null) {
    return;
  }
  const region = regionOf(element);
  if (region === null) {
    return;
  }
  region.onInput?.(element);
}

/**
 * Wires the page's delegated behaviors and opens the stream. All
 * listeners live on `document` — elements the page swap destroys are
 * re-resolved per event by the regions' selectors, so delegation
 * survives the swap by construction.
 */
export function initialize(): void {
  bindPageSwap(swapPage);

  document.addEventListener("click", (event) => {
    delegateClickOrChange(event.target, "click");
  });
  document.addEventListener("change", (event) => {
    delegateClickOrChange(event.target, "change");
  });
  document.addEventListener("input", (event) => {
    delegateInput(event.target);
  });
  document.addEventListener("keydown", (event) => {
    // The composer's Enter binding rides the same dispatch: only the
    // region claiming the target sees it.
    const element = dom.targetWithAttribute(event.target);
    if (element === null) {
      return;
    }
    const region = regionOf(element);
    if (region !== null) {
      region.onKeyDown?.(element, event as KeyboardEvent);
    }
  });

  // The stream for whatever session this page renders. The landing
  // page has none; a later swap opens one.
  const sessionId = viewedSessionId();
  if (sessionId !== null) {
    sse.follow(sessionId);
  }
}

// ES modules execute after the document is parsed, so the DOM is
// ready here — no DOMContentLoaded listener (one might miss the
// event; this cannot).
initialize();
