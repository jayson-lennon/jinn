//! jinn webui — the session-switch timeout.
//!
//! Datastar's expressions are synchronous, so nothing declarative can
//! arm a timer. This is the page's second bespoke script (the composer's
//! paste helper is the other); the Datastar bundle is the third.
//!
//! It owns one job and one piece of state — a timer handle:
//!
//! The page root carries `data-effect="jinnSwitchWatch($switching)"`,
//! so the bundle re-runs this hook whenever the `switching` signal
//! changes. A flip to `true` arms a timer; a flip back clears it. When
//! the timer fires the switch has stalled — the request hung, or was
//! aborted without a terminal fetch event — and the hook dispatches a
//! `jinnswitchstalled` document event. The page root's
//! `data-on:jinnswitchstalled__document` expression clears the overlay
//! and sets the error notice, so the signals keep their single writer
//! (Datastar) exactly as the paste helper's `jinnaddimages` does.
//!
//! The contract ids below are asserted against `contract.rs` by a Rust
//! drift test, so a renamed event fails the build rather than silently
//! leaving the overlay stuck.
(() => {
  "use strict";

  const STALLED_EVENT = "jinnswitchstalled";
  const TIMEOUT_MS = 10000;

  let timer = null;

  /** Arms the stall timer for a switch that just started, or clears it
   * for one that just ended. Called by the page root's data-effect. */
  window.jinnSwitchWatch = (switching) => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    if (!switching) return false;
    timer = setTimeout(() => {
      timer = null;
      document.dispatchEvent(new CustomEvent(STALLED_EVENT));
    }, TIMEOUT_MS);
    return true;
  };
})();
