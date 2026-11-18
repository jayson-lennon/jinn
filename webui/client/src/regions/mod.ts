//! The client region registry — the TypeScript half of the server's
//! `region.rs` registry.
//!
//! One module per region, mirroring the Rust side one-for-one:
//! sessions, transcript, composer, model_picker. A region module
//! registers its delegation selectors (contract-derived), its event
//! handlers, and its SSE event handlers. `main.ts` consumes the
//! registry generically — the boot module names no region.
//!
//! ## Ownership contract (the `SliceView` discipline, client-side)
//!
//! Every region module is a pure renderer of what the server pushed:
//!
//! - It owns **no application state** — the server is authoritative,
//!   and the DOM plus the URL are the client's only memory. The only
//!   permitted module-level binding is the registry entry itself.
//! - It owns only **view-local DOM state** (e.g. an optimistic entry
//!   the next server push reconciles).
//! - It reads the viewed session id fresh from the page's data
//!   attribute at handler time — a page swap can change it under any
//!   closure, so nothing may cache it.
//!
//! A module that reaches for a shared state store is violating this
//! contract, not extending it.
//!
//! ## Disjointness requirement
//!
//! The delegation selectors across region modules MUST be pairwise
//! disjoint — the registry's dispatch is first-match, and an overlap
//! would silently change which region handles an event. The jsdom
//! conformance test asserts this; it is what licenses the simple
//! dispatch loop.

import { sessionsRegion } from "./sessions.js";
import { transcriptRegion } from "./transcript.js";
import { composerRegion } from "./composer.js";
import { modelPickerRegion } from "./model_picker.js";

/** One SSE event, as EventSource delivers it. */
export interface SseEventLike {
  /** The pushed HTML fragment (empty on cursor-advance frames). */
  data: string;
  /** The cursor the event parks the client at (`Last-Event-ID`). */
  lastEventId: string;
}

/** One registered region's client-side surface. */
export interface RegionModule {
  /**
   * The region id — matches the server region's `id()` so the two
   * halves of one region are findable by the same name.
   */
  id: string;
  /**
   * The delegation selectors this region claims. MUST be pairwise
   * disjoint across regions (the conformance test enforces it) and
   * each MUST be derived from a `wire.ts` contract constant.
   */
  selectors: readonly string[];
  /**
   * Click delegation. Returns true when the region handled the
   * event (the dispatch loop stops at the first handler).
   */
  onClick?: (target: Element) => boolean;
  /**
   * Change delegation (the model select). Same first-match rule.
   */
  onChange?: (target: Element) => boolean;
  /**
   * Keydown delegation, fired for targets inside the region's
   * selectors (the composer's Enter binding). Return true when
   * handled.
   */
  onKeyDown?: (target: Element, event: KeyboardEvent) => boolean;
  /**
   * The SSE events this region consumes, keyed by the wire event
   * name. Every key MUST be a `wire.SSE_EVENTS` value (the
   * conformance test enforces it).
   */
  sseHandlers: Readonly<Record<string, (event: SseEventLike) => void>>;
  /**
   * Called after the page swaps (a session switch): the region may
   * need to re-bind to elements the swap replaced (the composer's
   * Enter binding). Receives the NEW session's id.
   */
  onPageSwap?: (sessionId: string) => void;
}

/** The registered regions, in registration order (mirrors the server). */
const REGIONS: readonly RegionModule[] = [
  sessionsRegion,
  transcriptRegion,
  composerRegion,
  modelPickerRegion,
];

/** The registered regions, in registration order. */
export function regions(): readonly RegionModule[] {
  return REGIONS;
}

/** The SSE event names across all regions' handlers. */
export function sseEventNames(): string[] {
  return REGIONS.flatMap((region) => Object.keys(region.sseHandlers));
}
