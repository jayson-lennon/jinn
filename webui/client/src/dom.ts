//! Shared DOM primitives — the small helpers every region's swap
//! resolves through.
//!
//! This module owns no behavior and no state: it is the client's
//! equivalent of a `render` utility belt, deliberately dumb so the
//! region modules (see `regions/`) stay the only place a page
//! behavior lives. A missing target must be a loud error, never the
//! silent `targetError` no-op that shipped once — these helpers are
//! the referential-integrity gate's runtime half.
//!
//! Ownership doc (the `SliceView` discipline, client-side): a region
//! module renders what the server pushed and owns nothing — no
//! application state, no caching of session ids, no cursor
//! bookkeeping. The DOM and the URL are the client's only memory.

/**
 * Resolves an element by id or throws naming what the client wanted.
 *
 * @param id - The contract id of the target element.
 * @returns The element.
 * @throws Error when the page lacks the target.
 */
export function byId(id: string): HTMLElement {
  const element = document.getElementById(id);
  if (element === null) {
    throw new Error(`client target #${id} is missing from the page`);
  }
  return element;
}

/**
 * Renders text as text — element.textContent, never HTML
 * interpolation. Any module building markup from server-adjacent
 * strings escapes through here.
 *
 * @param text - The text, verbatim.
 * @returns The HTML-escaped text.
 */
export function escapeText(text: string): string {
  const span = document.createElement("span");
  span.textContent = text;
  return span.innerHTML;
}

/**
 * Narrows an event target to an Element without `instanceof` — the
 * DOM globals the type system assumes are not installed in every
 * host the client's tests run under (happy-dom scopes its classes
 * per window). `closest` is the Element API actually needed; its
 * presence is the capability that matters.
 *
 * @param target - The raw event target.
 * @returns The target as an Element, or null when it is not one.
 */
export function targetElement(target: EventTarget | null): Element | null {
  const candidate = target as { closest?: Element["closest"] } | null;
  return typeof candidate?.closest === "function"
    ? (target as Element)
    : null;
}

/**
 * Narrows an event target to an Element carrying attributes without
 * `instanceof` — `getAttribute` (the action-attribute read) is the
 * capability that matters.
 *
 * @param target - The raw event target.
 * @returns The target as an Element, or null when it is not one.
 */
export function targetWithAttribute(
  target: EventTarget | null,
): Element | null {
  const candidate = target as { getAttribute?: Element["getAttribute"] } | null;
  return typeof candidate?.getAttribute === "function"
    ? (target as Element)
    : null;
}

/** Reports a failed action loudly — the anti-silent-failure rule. */
export function reportFailure(error: unknown): void {
  console.error("[webui] action failed:", error);
}
