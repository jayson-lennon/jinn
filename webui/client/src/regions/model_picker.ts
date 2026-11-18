//! The model-picker region — the titlebar select (client half of
//! `crates/slices/jinn-webui/src/regions/model_picker.rs`).
//!
//! Owns: the change delegation on the contract's select (the
//! `data-webui-action="model"` element) and the picker's innerHTML
//! swap from the POST response. No SSE handler exists — by design,
//! mirroring the server region's declared parts: a model switch is
//! per-POST and no convergence event re-renders this region.
//!
//! Ownership contract: this module renders what the server pushed and
//! owns nothing (see `regions/mod.ts`).

import * as api from "../api.js";
import * as wire from "../wire.js";
import { reportFailure, targetWithAttribute } from "../dom.js";
import type { RegionModule } from "./mod.js";

/** The model-picker region's client surface. */
export const modelPickerRegion: RegionModule = {
  id: "model-picker",
  selectors: [`#${wire.MODEL_PICKER_ID}`],

  onChange(target) {
    const candidate = targetWithAttribute(target);
    if (candidate === null) {
      return false;
    }
    if (candidate.getAttribute(wire.ACTION_DATA_ATTR) !== wire.ACTION_MODEL) {
      return false;
    }
    // The model action only ever rides the contract's select; the
    // duck-type read of `value` keeps this host-agnostic (the
    // instanceof global is not installed under happy-dom).
    const select = candidate as { value?: string };
    const modelId = typeof select.value === "string" ? select.value : "";
    const sessionId = viewedSessionId();
    if (sessionId === null || modelId.length === 0) {
      return true;
    }
    api.switchModel(sessionId, modelId)
      .then((html) => {
        byPickerId().innerHTML = html;
      })
      .catch(reportFailure);
    return true;
  },

  sseHandlers: {},
};

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
