// The market's frames: a rows frame lands only in the view the page last
// asked for, a detail frame only while the page is pinned to its model,
// and a stale frame sets the note and raises a toast.
//
// Every helper writes the store, so every test awaits one microtask before
// it reads the result.

import { describe, it, expect } from 'vitest';
import { createStore, type ClientStore } from '../store/store';
import {
  applyMarketDetail,
  applyMarketRows,
  applyMarketStale,
  resetMarketDetail,
  sameMarketView,
} from './market_rows';
import { DEFAULT_DETAIL_VIEW } from './market_state';
import type { MarketEndpoint, MarketRow, MarketView } from '../protocol/protocol';

/** Awaits the microtask a store write needs before it can be read back. */
async function settled(): Promise<void> {
  await Promise.resolve();
}

const VIEW: MarketView = { filter: '', sort: 'proj', dir: 'asc', mix: 'fixed' };

/** A rows frame for `view`, carrying `rows`. */
function rowsFrame(view: MarketView, rows: MarketRow[], extra: Partial<{ session_id: string; loading: boolean; mix_degraded: boolean }> = {}) {
  return {
    type: 'market_rows' as const,
    session_id: 's1',
    view,
    rows,
    total: rows.length,
    loading: false,
    updated: '3h ago',
    mix_label: 'fixed 10M·98%·50k',
    mix_degraded: false,
    active_id: null,
    ...extra,
  };
}

/** A listing row with only the id an assertion reads. */
function row(id: string): MarketRow {
  return {
    id,
    author: 'a',
    slug: id,
    variant: null,
    name_runs: [{ text: id, matched: false }],
    variant_runs: [],
    proj: '$0.5100',
    in_m: '$3.0000/M',
    out_m: '$15.0000/M',
    cache_m: '$0.3000/M',
    eci: '58.3',
    vals: '-',
    is_active: false,
  };
}

/** The auto-route endpoint. */
function endpoint(tag: string | null): MarketEndpoint {
  return {
    tag,
    name: tag === null ? 'Default' : `Azure (${tag})`,
    proj: '$0.5100',
    in_m: '$3.0000/M',
    out_m: '$15.0000/M',
    cache_m: '$0.3000/M',
    latency: '-',
    throughput: '-',
    uptime: '-',
    is_default: tag === null,
  };
}

/** A page holding the default market view. */
async function onDefaultView(store: ClientStore): Promise<void> {
  store.set((s) => {
    s.sessionId = 's1';
    s.marketView = { ...VIEW };
  });
  await settled();
}

describe('sameMarketView', () => {
  it('agrees on identical views', () => {
    expect(sameMarketView(VIEW, { ...VIEW })).toBe(true);
  });

  it.each([
    ['filter', { filter: 'claude' }],
    ['sort', { sort: 'eci' }],
    ['dir', { dir: 'desc' }],
    ['mix', { mix: 'usage' }],
  ] as const)('disagrees on the %s', (_field, change) => {
    expect(sameMarketView(VIEW, { ...VIEW, ...change })).toBe(false);
  });
});

describe('applyMarketRows', () => {
  it('lands the rows when the frame echoes the view the page holds', async () => {
    // Given a page on the default view.
    const store = createStore();
    await onDefaultView(store);

    // When a frame answers that view.
    applyMarketRows(store, rowsFrame({ ...VIEW }, [row('a/one'), row('a/two')]));
    await settled();

    // Then the rows, the count, the stamp and the label land.
    expect(store.state.marketRows.map((r) => r.id)).toEqual(['a/one', 'a/two']);
    expect(store.state.marketTotal).toBe(2);
    expect(store.state.marketUpdated).toBe('3h ago');
    expect(store.state.marketMixLabel).toBe('fixed 10M·98%·50k');
  });

  it('drops a frame that answers a view the page has moved past', async () => {
    // Given a page that has since sorted by ECI.
    const store = createStore();
    await onDefaultView(store);
    store.set((s) => {
      s.marketView = { ...VIEW, sort: 'eci', dir: 'desc' };
      s.marketSorting = 'eci';
    });
    await settled();

    // When the late frame for the old view lands.
    applyMarketRows(store, rowsFrame({ ...VIEW }, [row('a/one')]));
    await settled();

    // Then nothing lands, and the ECI header is still waiting.
    expect(store.state.marketRows).toEqual([]);
    expect(store.state.marketSorting).toBe('eci');
  });

  it('clears the tapped header’s marker when its frame lands', async () => {
    // Given a page whose PROJ header is waiting.
    const store = createStore();
    await onDefaultView(store);
    store.set((s) => {
      s.marketSorting = 'proj';
    });
    await settled();

    // When the view's frame lands.
    applyMarketRows(store, rowsFrame({ ...VIEW }, [row('a/one')]));
    await settled();

    // Then the marker is gone.
    expect(store.state.marketSorting).toBeNull();
  });

  it('drops a frame for a session the page has left', async () => {
    // Given a page viewing s2.
    const store = createStore();
    store.set((s) => {
      s.sessionId = 's2';
      s.marketView = { ...VIEW };
    });
    await settled();

    // When a frame for s1 lands.
    applyMarketRows(store, rowsFrame({ ...VIEW }, [row('a/one')], { session_id: 's1' }));
    await settled();

    // Then nothing lands.
    expect(store.state.marketRows).toEqual([]);
  });

  it('accepts a frame for any session while the page holds none', async () => {
    // Given a page with no session yet.
    const store = createStore();
    store.set((s) => {
      s.sessionId = null;
      s.marketView = { ...VIEW };
    });
    await settled();

    // When a frame for s1 lands.
    applyMarketRows(store, rowsFrame({ ...VIEW }, [row('a/one')]));
    await settled();

    // Then the rows land.
    expect(store.state.marketRows.length).toBe(1);
  });

  it('clears a stale note when a frame lands', async () => {
    // Given a page carrying a stale note.
    const store = createStore();
    await onDefaultView(store);
    store.set((s) => {
      s.marketStale = 'models fetch failed';
    });
    await settled();

    // When a frame lands.
    applyMarketRows(store, rowsFrame({ ...VIEW }, [row('a/one')]));
    await settled();

    // Then the note is gone.
    expect(store.state.marketStale).toBeNull();
  });

  it('flags a usage mix that fell back to the fixed constant', async () => {
    // Given the page asking for the tree's usage on a tree with none.
    const store = createStore();
    await onDefaultView(store);
    const usage = { ...VIEW, mix: 'usage' as const };
    store.set((s) => {
      s.marketView = usage;
    });
    await settled();

    // When the frame lands degraded.
    applyMarketRows(store, rowsFrame(usage, [row('a/one')], { mix_degraded: true }));
    await settled();

    // Then the page knows the mix fell back.
    expect(store.state.marketMixDegraded).toBe(true);
  });

  it('marks the active model the frame names', async () => {
    // Given a page on the default view.
    const store = createStore();
    await onDefaultView(store);

    // When a frame names an active id.
    applyMarketRows(
      store,
      rowsFrame({ ...VIEW }, [{ ...row('a/one'), is_active: true }], { active_id: 'a/one' } as never),
    );
    await settled();

    // Then the page remembers it.
    expect(store.state.marketActiveId).toBe('a/one');
  });
});

describe('applyMarketDetail', () => {
  it('lands the endpoints while the page is pinned to the frame’s model', async () => {
    // Given a page pinned to a/one.
    const store = createStore();
    store.set((s) => {
      s.marketPinnedId = 'a/one';
      s.marketDetailSorting = 'proj';
    });
    await settled();

    // When its detail lands.
    applyMarketDetail(store, {
      type: 'market_detail',
      session_id: 's1',
      model_id: 'a/one',
      view: DEFAULT_DETAIL_VIEW,
      context_length: 200_000,
      is_free: false,
      has_overrides: true,
      endpoints: [endpoint(null), endpoint('azure')],
      loading: false,
      pinned_tag: null,
    });
    await settled();

    // Then the endpoints, the loading flag and the facts land.
    expect(store.state.marketEndpoints.map((e) => e.name)).toEqual(['Default', 'Azure (azure)']);
    expect(store.state.marketDetailSorting).toBeNull();
    expect(store.state.marketModelFacts).toEqual({ context_length: 200_000, is_free: false, has_overrides: true, pinned_tag: null });
  });

  it('drops a frame for a detail sort the page has moved past', async () => {
    // Given the page pinned to a model, now sorted by latency.
    const store = createStore();
    await onDefaultView(store);
    store.set((s) => {
      s.marketPinnedId = 'a/one';
      s.marketDetailView = { sort: 'lat', dir: 'asc' };
      s.marketDetailSorting = 'lat';
    });
    await settled();

    // When a frame for the earlier projection sort lands.
    applyMarketDetail(store, {
      type: 'market_detail',
      session_id: 's1',
      model_id: 'a/one',
      view: DEFAULT_DETAIL_VIEW,
      context_length: null,
      is_free: false,
      has_overrides: false,
      endpoints: [endpoint(null)],
      loading: false,
      pinned_tag: null,
    });
    await settled();

    // Then nothing lands, and the LAT header is still waiting.
    expect(store.state.marketEndpoints).toEqual([]);
    expect(store.state.marketDetailSorting).toBe('lat');
  });

  it('clears the tapped header’s marker when its frame lands', async () => {
    // Given a page pinned to a model, its LAT header waiting.
    const store = createStore();
    await onDefaultView(store);
    store.set((s) => {
      s.marketPinnedId = 'a/one';
      s.marketDetailView = { sort: 'lat', dir: 'asc' };
      s.marketDetailSorting = 'lat';
    });
    await settled();

    // When the latency sort's frame lands.
    applyMarketDetail(store, {
      type: 'market_detail',
      session_id: 's1',
      model_id: 'a/one',
      view: { sort: 'lat', dir: 'asc' },
      context_length: null,
      is_free: false,
      has_overrides: false,
      endpoints: [endpoint(null)],
      loading: false,
      pinned_tag: null,
    });
    await settled();

    // Then the marker is gone.
    expect(store.state.marketDetailSorting).toBeNull();
  });

  it('drops a frame for a model the page has backed out of', async () => {
    // Given a page no longer pinned.
    const store = createStore();
    store.set((s) => {
      s.marketPinnedId = null;
    });
    await settled();

    // When a detail frame for a/one lands.
    applyMarketDetail(store, {
      type: 'market_detail',
      session_id: 's1',
      model_id: 'a/one',
      view: DEFAULT_DETAIL_VIEW,
      context_length: null,
      is_free: false,
      has_overrides: false,
      endpoints: [endpoint(null)],
      loading: false,
      pinned_tag: null,
    });
    await settled();

    // Then nothing lands.
    expect(store.state.marketEndpoints).toEqual([]);
  });
});

describe('applyMarketStale', () => {
  it('sets the note and raises a toast, leaving the rows standing', async () => {
    // Given a page with rows.
    const store = createStore();
    await onDefaultView(store);
    applyMarketRows(store, rowsFrame({ ...VIEW }, [row('a/one')]));
    await settled();

    // When a refresh step fails.
    applyMarketStale(store, { type: 'market_stale', message: 'models fetch failed' });
    await settled();

    // Then the note and the toast carry the failure, and the rows stand.
    expect(store.state.marketStale).toBe('models fetch failed');
    expect(store.state.toasts.map((t) => t.message)).toEqual(['models fetch failed']);
    expect(store.state.marketRows.length).toBe(1);
  });
});

describe('resetMarketDetail', () => {
  it('returns the page to the list', async () => {
    // Given a page pinned with endpoints.
    const store = createStore();
    store.set((s) => {
      s.marketPinnedId = 'a/one';
      s.marketEndpoints = [endpoint(null)];
      s.marketDetailSorting = 'proj';
      s.marketModelFacts = { context_length: 1, is_free: false, has_overrides: false, pinned_tag: null };
    });
    await settled();

    // When the session switches.
    store.set((s) => resetMarketDetail(s));
    await settled();

    // Then the detail state is cleared.
    expect(store.state.marketPinnedId).toBeNull();
    expect(store.state.marketEndpoints).toEqual([]);
    expect(store.state.marketDetailSorting).toBeNull();
    expect(store.state.marketModelFacts).toBeNull();
  });
});
