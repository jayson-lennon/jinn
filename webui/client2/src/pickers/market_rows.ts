// The market's frames: a rows frame lands only in the view the page last
// asked for, a detail frame only while the page is pinned to its model,
// and a stale frame sets the note and raises a toast.
//
// Every helper writes the store, so a test awaits one microtask before it
// reads the result.

import type { ClientStore, ViewState } from '../store/store';
import type { ServerMsg } from '../protocol/protocol';
import type { MarketView } from '../protocol/protocol';
import { pushToast } from '../overlays/toast_state';
import { DEFAULT_DETAIL_VIEW } from './market_state';

type Msg<Kinds extends ServerMsg['type']> = Extract<ServerMsg, { type: Kinds }>;

/** Whether two views ask for the same rows. */
export function sameMarketView(a: MarketView, b: MarketView): boolean {
  return a.filter === b.filter && a.sort === b.sort && a.dir === b.dir && a.mix === b.mix;
}

/** `market_rows`: the list's rows, in the view the frame echoes. */
export function applyMarketRows(store: ClientStore, msg: Msg<'market_rows'>): void {
  // A page with no session yet accepts any session's frame: the catalog is
  // instance-wide, and the open races the first snapshot.
  if (store.state.sessionId !== null && msg.session_id !== store.state.sessionId) return;
  if (!sameMarketView(msg.view, store.state.marketView)) return;
  store.set((s) => {
    s.marketRows = msg.rows;
    s.marketTotal = msg.total;
    s.marketLoading = msg.loading;
    s.marketUpdated = msg.updated;
    s.marketMixLabel = msg.mix_label;
    s.marketMixDegraded = msg.mix_degraded;
    s.marketActiveId = msg.active_id;
    s.marketStale = null;
    // The view's frame landed: whichever header was waiting is answered.
    s.marketSorting = null;
  });
}

/** `market_detail`: one model's endpoints, while the page is pinned to it. */
export function applyMarketDetail(store: ClientStore, msg: Msg<'market_detail'>): void {
  if (msg.model_id !== store.state.marketPinnedId) return;
  // The echo: a frame for a sort the page has moved past is dropped.
  if (msg.view.sort !== store.state.marketDetailView.sort || msg.view.dir !== store.state.marketDetailView.dir) return;
  store.set((s) => {
    s.marketEndpoints = msg.endpoints;
    // The sort's frame landed: whichever header was waiting is answered.
    s.marketDetailSorting = null;
    s.marketModelFacts = {
      context_length: msg.context_length,
      is_free: msg.is_free,
      has_overrides: msg.has_overrides,
      pinned_tag: msg.pinned_tag,
    };
  });
}

/** `market_stale`: a fetch failed. The rows stand; the note is set and
 * the failure is raised as a toast. */
export function applyMarketStale(store: ClientStore, msg: Msg<'market_stale'>): void {
  store.set((s) => {
    s.marketStale = msg.message;
  });
  pushToast(store, msg.message);
}

/** A session switch: the detail page describes one model, not one
 * session, but its pin is a request the next rows frame re-answers, so
 * the page goes back to the list rather than hold a stale table. */
export function resetMarketDetail(s: ViewState): void {
  s.marketPinnedId = null;
  s.marketDetailView = DEFAULT_DETAIL_VIEW;
  s.marketEndpoints = [];
  s.marketDetailSorting = null;
  s.marketModelFacts = null;
}
