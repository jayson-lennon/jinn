// The model market's pure helpers: the sort rule, the saved preferences,
// and the popout URL. Nothing here touches the DOM or the store, so each
// is proven by a unit test beside this file.

import type { DetailSort, DetailView, MarketDir, MarketSort, MarketView } from '../protocol/protocol';

/** The sort columns the header offers, in the rows' order — the name
 * label first, then PROJ beside it, then the strip. */
export const MARKET_SORTS: readonly MarketSort[] = ['proj', 'in', 'out', 'cache', 'eci', 'vals', 'model'];

/** The view a fresh market opens in: no filter, PROJ ascending, the
 * shipped mix — the terminal's default. */
export const DEFAULT_MARKET_VIEW: MarketView = { filter: '', sort: 'proj', dir: 'asc', mix: 'fixed' };

/** Whether `value` is one of the sort columns. */
export function isMarketSort(value: unknown): value is MarketSort {
  return typeof value === 'string' && (MARKET_SORTS as readonly string[]).includes(value);
}

/** Whether `value` is a sort direction. */
export function isMarketDir(value: unknown): value is MarketDir {
  return value === 'asc' || value === 'desc';
}

/** Whether `value` is a mix source. */
export function isMarketMix(value: unknown): value is MarketView['mix'] {
  return value === 'fixed' || value === 'usage';
}

/** The direction a column takes when its header is first tapped: prices
 * and costs ascend (cheapest first), scores descend (best first), names
 * ascend. A second tap on the same column flips it — that rule lives in
 * `nextSort`. */
export function naturalDir(sort: MarketSort): MarketDir {
  return sort === 'eci' || sort === 'vals' ? 'desc' : 'asc';
}

/** The view after a header tap. Tapping the active column reverses its
 * direction; tapping another column sorts by it in its natural
 * direction. The filter and the mix are untouched. */
export function nextSort(view: MarketView, sort: MarketSort): MarketView {
  if (sort === view.sort) {
    return { ...view, dir: view.dir === 'asc' ? 'desc' : 'asc' };
  }
  return { ...view, sort, dir: naturalDir(sort) };
}

/** The detail view a page opens with: projected cost, cheapest first —
 * the TUI's own default on entering detail mode. */
export const DEFAULT_DETAIL_VIEW: DetailView = { sort: 'proj', dir: 'asc' };

/** The direction a detail column takes when first tapped: prices, costs,
 * latency (lowest ms is best) and the name ascend; throughput and
 * uptime descend (best first). */
export function naturalDetailDir(sort: DetailSort): MarketDir {
  return sort === 'tp' || sort === 'up' ? 'desc' : 'asc';
}

/** The detail view after a header tap: the active column reverses, any
 * other column takes its natural direction. */
export function nextDetailSort(view: DetailView, sort: DetailSort): DetailView {
  if (sort === view.sort) {
    return { ...view, dir: view.dir === 'asc' ? 'desc' : 'asc' };
  }
  return { sort, dir: naturalDetailDir(sort) };
}

/** The arrow the active header shows. */
export function dirArrow(dir: MarketDir): string {
  return dir === 'asc' ? '↑' : '↓';
}

/** What the market remembers across opens. The filter is deliberately
 * not here: a stale filter reads as missing models. */
export interface MarketPrefs {
  sort: MarketSort;
  dir: MarketDir;
  mix: MarketView['mix'];
}

const PREFS_KEY = 'jinn.web2.marketPrefs';

/** The preferences from localStorage, or the defaults. Storage can be
 * refused and a saved value can be stale or corrupt; either way the
 * market opens on the defaults and nothing throws. */
export function loadMarketPrefs(): MarketPrefs {
  try {
    const raw = window.localStorage.getItem(PREFS_KEY);
    if (raw === null) return { sort: 'proj', dir: 'asc', mix: 'fixed' };
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null) return { sort: 'proj', dir: 'asc', mix: 'fixed' };
    const record = parsed as Record<string, unknown>;
    return {
      sort: isMarketSort(record.sort) ? record.sort : 'proj',
      dir: isMarketDir(record.dir) ? record.dir : 'asc',
      mix: isMarketMix(record.mix) ? record.mix : 'fixed',
    };
  } catch {
    return { sort: 'proj', dir: 'asc', mix: 'fixed' };
  }
}

/** Saves the preferences. A refused write is ignored: the market still
 * works, it just does not remember. */
export function saveMarketPrefs(view: MarketView): void {
  try {
    window.localStorage.setItem(
      PREFS_KEY,
      JSON.stringify({ sort: view.sort, dir: view.dir, mix: view.mix }),
    );
  } catch {
    // Nothing to do: the preferences are a convenience.
  }
}

/** The view the market opens in: the saved preferences over the default,
 * with an empty filter. */
export function openingView(): MarketView {
  return { ...DEFAULT_MARKET_VIEW, ...loadMarketPrefs(), filter: '' };
}

/** The openrouter.ai page for a listing id: the id minus its
 * `openrouter/` prefix, appended to the site root — the browser's twin
 * of the contract crate's `model_page_url`, locked to it by a test with
 * the same cases. */
export function modelPageUrl(id: string): string {
  const rest = id.startsWith('openrouter/') ? id.slice('openrouter/'.length) : id;
  return `https://openrouter.ai/${rest}`;
}
