// The OpenRouter model market: compare prices and benchmark scores across
// the catalog, then switch to a model — and pin one of its endpoints — from
// its detail page.
//
// The purpose is comparison, so the rows are a table and every metric sits
// at the same x-position in every row. A phone in portrait has half the
// terminal row's width, so a row is two lines there — the name with PROJ,
// then the metric strip — and one line on a wide screen. One markup, two
// densities: the grid in `styles/market.css` reflows it, and the header is
// on the same grid so the thing you tap is the column you sort.
//
// The rows are the server's, in the view the page last asked for (the
// terminal's own filter, sort and projection); the browser computes no
// price and holds no catalog. What survives an open — the sort, the
// direction, the mix — is saved in localStorage; the filter is not.

import { createSignal, For, Show, onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientStore } from '../store/store';
import type { DetailSort, DetailView, MarketEndpoint, MarketRow, MarketSort } from '../protocol/protocol';
import type { Send } from './model_picker';
import { Runs } from './runs';
import { FILTER_DEBOUNCE_MS } from './model_picker';
import { DEFAULT_DETAIL_VIEW, dirArrow, modelPageUrl, nextDetailSort, nextSort, saveMarketPrefs } from './market_state';

/** The strip's five columns, under the name on a phone and beside it on a
 * wide screen. IN and OUT always ride the strip; PROJ sits by the name. */
const STRIP: readonly { sort: MarketSort; label: string; cell: 'in' | 'out' | 'cache' | 'eci' | 'vals' }[] = [
  { sort: 'in', label: 'IN/M', cell: 'in' },
  { sort: 'out', label: 'OUT/M', cell: 'out' },
  { sort: 'cache', label: 'CACH/M', cell: 'cache' },
  { sort: 'eci', label: 'ECI', cell: 'eci' },
  { sort: 'vals', label: 'VALS', cell: 'vals' },
];

/** The detail page's columns, in the TUI's order. */
const DETAIL_COLUMNS: readonly { sort: DetailSort; label: string; cell: string }[] = [
  { sort: 'endpoint', label: 'ENDPOINT', cell: 'e-name' },
  { sort: 'proj', label: 'PROJ', cell: 'e-proj' },
  { sort: 'in', label: 'IN/M', cell: 'e-in' },
  { sort: 'out', label: 'OUT/M', cell: 'e-out' },
  { sort: 'cache', label: 'CACH/M', cell: 'e-cache' },
  { sort: 'lat', label: 'LAT', cell: 'e-lat' },
  { sort: 'tp', label: 'TP', cell: 'e-tp' },
  { sort: 'up', label: 'UP', cell: 'e-up' },
];

/** The market modal: the list page, or one model's detail page. */
export function MarketModal(props: { store: ClientStore; send: Send; onClose: () => void }): JSX.Element {
  const pinned = (): string | null => props.store.state.marketPinnedId;
  let filterTimer: number | null = null;

  // Escape steps back on the detail page and closes on the list. The
  // listener sits on the document for as long as the modal is mounted,
  // and is removed on unmount so a closed modal never swallows the key.
  const onKeydown = (event: KeyboardEvent): void => {
    if (event.key !== 'Escape') return;
    event.stopPropagation();
    if (pinned() !== null) back(); else props.onClose();
  };
  document.addEventListener('keydown', onKeydown);
  onCleanup(() => document.removeEventListener('keydown', onKeydown));

  // The request's view is the store's view with the new filter, and the
  // store's view takes that filter at the same moment — the frame echoes
  // the view, so a view the page never recorded would drop every answer.
  const onFilter = (filter: string): void => {
    const view = { ...props.store.state.marketView, filter };
    props.store.set((s) => {
      s.marketFilter = filter;
      s.marketView = view;
    });
    if (filterTimer !== null) window.clearTimeout(filterTimer);
    filterTimer = window.setTimeout(() => props.send({ type: 'market_rows', view }), FILTER_DEBOUNCE_MS);
  };

  const sort = (by: MarketSort): void => {
    const view = nextSort(props.store.state.marketView, by);
    props.store.set((s) => {
      s.marketView = view;
      // The tapped header waits: its frame clears the marker when it lands.
      s.marketSorting = by;
    });
    saveMarketPrefs(view);
    props.send({ type: 'market_rows', view });
  };

  const toggleMix = (): void => {
    const view = { ...props.store.state.marketView, mix: props.store.state.marketView.mix === 'usage' ? 'fixed' as const : 'usage' as const };
    props.store.set((s) => { s.marketView = view; });
    saveMarketPrefs(view);
    props.send({ type: 'market_rows', view });
  };

  let pressing = false;
  const refresh = (): void => {
    if (pressing || props.store.state.marketLoading) return;
    pressing = true;
    props.store.set((s) => { s.marketLoading = true; });
    props.send({ type: 'market_refresh' });
    // The flag is cleared by the frame that lands; a refresh that fails
    // publishes a stale frame instead, which also leaves loading true
    // until the next rows frame — so release the press guard on a timer
    // as well, the way the provider picker does.
    window.setTimeout(() => { pressing = false; }, 2000);
  };

  const openDetail = (row: MarketRow): void => {
    props.store.set((s) => {
      s.marketPinnedId = row.id;
      s.marketDetailView = DEFAULT_DETAIL_VIEW;
      s.marketEndpoints = [];
      // A first open waits on PROJ, the default sort's header.
      s.marketDetailSorting = DEFAULT_DETAIL_VIEW.sort;
      s.marketModelFacts = null;
    });
    props.send({ type: 'market_detail', model_id: row.id, view: DEFAULT_DETAIL_VIEW });
  };

  const back = (): void => {
    props.store.set((s) => {
      s.marketPinnedId = null;
      s.marketEndpoints = [];
      s.marketDetailSorting = null;
      s.marketModelFacts = null;
    });
  };

  const view = () => props.store.state.marketView;
  const sorting = (): MarketSort | null => props.store.state.marketSorting;
  const rows = () => props.store.state.marketRows;
  const detailRow = (): MarketRow | undefined =>
    rows().find((row) => row.id === pinned());

  return (
    <div class="market-modal" role="dialog" aria-label="OpenRouter Model Market" data-testid="market-modal">
      <Show
        when={pinned() === null}
        fallback={
          <MarketDetail
            store={props.store}
            send={props.send}
            row={detailRow()}
            onBack={back}
            onDone={props.onClose}
          />
        }
      >
        <div class="market-topbar">
          <input
            class="modal-filter"
            data-testid="market-filter"
            placeholder="Filter models…"
            value={props.store.state.marketFilter}
            onInput={(e) => onFilter(e.currentTarget.value)}
          />
          <button
            type="button"
            class="market-icon-btn"
            data-testid="market-mix"
            aria-pressed={view().mix === 'usage' ? 'true' : 'false'}
            title={view().mix === 'usage' ? 'Projecting under the session tree’s usage' : 'Projecting under the fixed mix'}
            onClick={toggleMix}
          >
            mix
          </button>
          <button
            type="button"
            class="market-icon-btn"
            data-testid="market-refresh"
            aria-busy={props.store.state.marketLoading ? 'true' : 'false'}
            disabled={props.store.state.marketLoading}
            title="Refresh the catalog"
            onClick={refresh}
          >
            ⟳
          </button>
          {/* A phone has no Escape key and the market is full-bleed, so
              the list needs its own close. */}
          <button
            type="button"
            class="market-icon-btn"
            data-testid="market-close"
            aria-label="Close the market"
            title="Close"
            onClick={props.onClose}
          >
            ×
          </button>
        </div>

        <div class="market-status" data-testid="market-status">
          <Show when={props.store.state.marketUpdated !== null} fallback={<>not loaded yet</>}>
            <>
              updated {props.store.state.marketUpdated} · mix: {props.store.state.marketMixLabel}
              <Show when={props.store.state.marketMixDegraded}>
                <span class="m-degraded" data-testid="market-mix-degraded"> (no session usage yet)</span>
              </Show>{' '}
              · {props.store.state.marketTotal} models
            </>
          </Show>
          <Show when={props.store.state.marketStale !== null}>
            <> · <span class="stale">stale: {props.store.state.marketStale}</span></>
          </Show>
        </div>

        <div class="market-list" data-testid="market-list">
          <div class="market-head" role="row">
            <span class="m-name" role="columnheader">MODEL</span>
            <SortButton sort="proj" label="PROJ" view={view()} onSort={sort} busy={sorting() === 'proj'} cell="proj" prefix="m-" />
            <For each={STRIP}>
                {(column) => (
                  <SortButton sort={column.sort} label={column.label} view={view()} onSort={sort} busy={sorting() === column.sort} cell={column.cell} prefix="m-" />
                )}
            </For>
          </div>
          <For each={rows()}>
            {(row) => (
              <button
                type="button"
                class="m-row"
                data-testid="market-row"
                data-model-id={row.id}
                onClick={() => openDetail(row)}
              >
                <span class="m-top">
                  <span class="m-name">
                    <Runs runs={row.name_runs} />
                    <Show when={row.variant_runs.length > 0}>
                      <span class="m-variant">
                        <Runs runs={row.variant_runs} />
                      </span>
                    </Show>
                    <Show when={row.is_active}>
                      <span class="m-current" data-testid="market-current"> · current</span>
                    </Show>
                  </span>
                  <span class="m-proj">{row.proj}</span>
                </span>
                <span class="m-strip">
                  <span class="m-in">{row.in_m}</span>
                  <span class="m-out">{row.out_m}</span>
                  <span class="m-cache">{row.cache_m}</span>
                  <span class="m-eci">{row.eci}</span>
                  <span class="m-vals">{row.vals}</span>
                </span>
              </button>
            )}
          </For>
          <Show when={rows().length === 0}>
            <div class="market-empty">
              {props.store.state.marketLoading ? 'loading models…' : 'no models match'}
            </div>
          </Show>
        </div>
      </Show>
    </div>
  );
}

/** A column header on the rows' grid: the sort control. Shared by the
 * list and the detail page — `cell` names the grid track, `testid` the
 * header's test id. */
function SortButton<S extends string>(props: {
  sort: S;
  label: string;
  view: { sort: S; dir: 'asc' | 'desc' };
  onSort: (sort: S) => void;
  /** Whether this header's sort is the one in flight: the label dims and
   * the spinner overlays it, without the header's box moving. */
  busy?: boolean;
  cell?: string;
  /** The class prefix on `cell`; the list's is `m-`, the detail's none. */
  prefix?: string;
  testid?: string;
}): JSX.Element {
  const active = (): boolean => props.view.sort === props.sort;
  const busy = (): boolean => props.busy === true;
  return (
    <button
      type="button"
      class={`${props.cell === undefined ? 'm-proj' : props.prefix === undefined ? props.cell : `${props.prefix}${props.cell}`}${active() ? ' on' : ''}${busy() ? ' sorting' : ''}`}
      data-testid={props.testid ?? `market-sort-${props.sort}`}
      aria-sort={active() ? (props.view.dir === 'asc' ? 'ascending' : 'descending') : 'none'}
      aria-busy={busy() ? 'true' : undefined}
      onClick={() => props.onSort(props.sort)}
    >
      <span class="sort-label">{props.label}{active() ? ` ${dirArrow(props.view.dir)}` : ''}</span>
      <Show when={busy()}>
        <span class="spinner spinner-small sort-busy" aria-hidden="true"><i /></span>
      </Show>
    </button>
  );
}

/** One model's detail: its endpoints, the popout, and "Use this model". */
function MarketDetail(props: {
  store: ClientStore;
  send: Send;
  row: MarketRow | undefined;
  onBack: () => void;
  onDone: () => void;
}): JSX.Element {
  // The highlighted endpoint is the one "Use this model" acts on: the
  // auto-route sentinel until another is tapped.
  const [chosen, setChosen] = createSignal<MarketEndpoint | null>(null);
  const endpoints = (): MarketEndpoint[] => props.store.state.marketEndpoints;
  const selected = (): MarketEndpoint | undefined =>
    chosen() ?? endpoints().find((endpoint) => endpoint.is_default);

  const use = (): void => {
    const modelId = props.store.state.marketPinnedId;
    if (modelId === null) return;
    const endpoint = selected();
    props.send({ type: 'market_use', model_id: modelId, tag: endpoint?.tag ?? null });
    props.onDone();
  };

  const row = (): MarketRow | undefined => props.row;
  // The detail sort: the active column reverses, another takes its
  // natural direction, and the server re-sorts — the Default row stays
  // first in every sort.
  const detailView = (): DetailView => props.store.state.marketDetailView;
  const detailSorting = (): DetailSort | null => props.store.state.marketDetailSorting;
  const sortDetail = (sort: DetailSort): void => {
    const modelId = props.store.state.marketPinnedId;
    if (modelId === null) return;
    const view = nextDetailSort(detailView(), sort);
    props.store.set((s) => {
      s.marketDetailView = view;
      // The tapped header waits; its frame clears the marker.
      s.marketDetailSorting = sort;
    });
    props.send({ type: 'market_detail', model_id: modelId, view });
  };
  // The facts line, as the strings it renders: context length in k, then
  // free, then overrides — each only when it is true.
  const facts = (): string[] => {
    const facts = props.store.state.marketModelFacts;
    if (facts === null) return [];
    const out: string[] = [];
    if (facts.context_length !== null) out.push(`${Math.round(facts.context_length / 1000)}k context`);
    if (facts.is_free) out.push('free');
    if (facts.has_overrides) out.push('pricing overrides');
    return out;
  };
  // The Use button is a no-op when the model is already the session's
  // and the pin would not change: the highlighted endpoint equals the
  // model's pinned tag (a null highlight means auto-route). Highlighting
  // any other endpoint still changes the pin, so it stays enabled there.
  const useIsNoop = (): boolean => {
    const current = row();
    if (current === undefined || !current.is_active) return false;
    const pinned = props.store.state.marketModelFacts?.pinned_tag ?? null;
    return (selected()?.tag ?? null) === pinned;
  };
  const popoutHref = (): string => {
    const current = row();
    return current === undefined ? '#' : modelPageUrl(current.id);
  };

  return (
    <div class="market-detail" data-testid="market-detail">
      <div class="market-topbar">
        <button type="button" class="market-icon-btn" data-testid="market-back" aria-label="Back to the market" onClick={props.onBack}>
          ←
        </button>
        <div class="market-detail-title">
          <span class="market-detail-name" data-testid="market-detail-name">
            {row()?.author}/{row()?.slug}
            <Show when={row()?.variant !== null && row()?.variant !== undefined}>
              <span class="m-variant">:{row()?.variant}</span>
            </Show>
          </span>
        </div>
        <a
          class="market-icon-btn popout"
          data-testid="market-popout"
          href={popoutHref()}
          target="_blank"
          rel="noopener noreferrer"
          aria-label="Open on openrouter.ai"
          title="Open on openrouter.ai"
        >
          <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
            <path d="M5 2h7v7M12 2 3 11" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
          </svg>
        </a>
      </div>

      <Show when={facts().length > 0}>
        <div class="market-facts" data-testid="market-facts">
          <For each={facts()}>{(fact) => <span class="fact">{fact}</span>}</For>
        </div>
      </Show>

      <div class="market-list" data-testid="market-endpoints">
        <Show when={detailSorting() === null && endpoints().length === 0}>
          <div class="market-empty">no endpoints</div>
        </Show>
        <Show when={endpoints().length > 0}>
          <div class="market-head e-head" role="row">
            <For each={DETAIL_COLUMNS}>
              {(column) => (
                <SortButton
                  sort={column.sort}
                  label={column.label}
                  view={detailView()}
                  onSort={sortDetail}
                  busy={detailSorting() === column.sort}
                  cell={column.cell}
                  testid={`market-detail-sort-${column.sort}`}
                />
              )}
            </For>
          </div>
          <For each={endpoints()}>
            {(endpoint) => (
              <button
                type="button"
                class={`e-row${selected()?.tag === endpoint.tag && selected()?.is_default === endpoint.is_default ? ' chosen' : ''}`}
                data-testid="market-endpoint"
                data-tag={endpoint.tag ?? ''}
                onClick={() => setChosen(endpoint)}
              >
                <span class="e-name">{endpoint.name}</span>
                <span class="e-proj">{endpoint.proj}</span>
                <span class="e-in">{endpoint.in_m}</span>
                <span class="e-out">{endpoint.out_m}</span>
                <span class="e-cache">{endpoint.cache_m}</span>
                <span class="e-lat">{endpoint.latency}</span>
                <span class="e-tp">{endpoint.throughput}</span>
                <span class="e-up">{endpoint.uptime}</span>
              </button>
            )}
          </For>
        </Show>
      </div>

      <div class="market-use">
        <button
          type="button"
          class="use-btn"
          data-testid="market-use"
          disabled={useIsNoop()}
          onClick={use}
        >
          Use this model
        </button>
      </div>
    </div>
  );
}
