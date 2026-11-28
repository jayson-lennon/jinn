// @vitest-environment jsdom
// The market's pure helpers: the sort rule, the saved preferences, and
// the popout URL. Each is a pure function, so each is proven here.

import { describe, it, expect, beforeEach, vi } from 'vitest';
import {
  DEFAULT_MARKET_VIEW,
  dirArrow,
  isMarketSort,
  loadMarketPrefs,
  modelPageUrl,
  naturalDir,
  nextSort,
  openingView,
  saveMarketPrefs,
} from './market_state';
import type { MarketSort, MarketView } from '../protocol/protocol';

const SORTS: readonly MarketSort[] = ['proj', 'in', 'out', 'cache', 'eci', 'vals', 'model'];

/** A view on PROJ ascending — the default — for the sort tests. */
function onProj(): MarketView {
  return { ...DEFAULT_MARKET_VIEW };
}

describe('natural direction', () => {
  it.each([
    ['proj', 'asc'],
    ['in', 'asc'],
    ['out', 'asc'],
    ['cache', 'asc'],
    ['model', 'asc'],
  ] as const)('sorts %s ascending on the first tap', (sort, dir) => {
    expect(naturalDir(sort)).toBe(dir);
  });

  it.each([
    ['eci', 'desc'],
    ['vals', 'desc'],
  ] as const)('sorts %s descending on the first tap', (sort, dir) => {
    expect(naturalDir(sort)).toBe(dir);
  });
});

describe('next sort', () => {
  it('takes a new column in its natural direction', () => {
    expect(nextSort(onProj(), 'eci')).toEqual({ ...onProj(), sort: 'eci', dir: 'desc' });
  });

  it('reverses the active column on a second tap', () => {
    const once = nextSort(onProj(), 'out');
    expect(nextSort(once, 'out')).toEqual({ ...onProj(), sort: 'out', dir: 'desc' });
  });

  it('reverses back on a third tap', () => {
    const once = nextSort(onProj(), 'eci');
    const twice = nextSort(once, 'eci');
    expect(nextSort(twice, 'eci').dir).toBe('desc');
  });

  it('keeps the filter and the mix', () => {
    const view: MarketView = { filter: 'claude', sort: 'proj', dir: 'asc', mix: 'usage' };
    expect(nextSort(view, 'in')).toEqual({ filter: 'claude', sort: 'in', dir: 'asc', mix: 'usage' });
  });
});

describe('the header arrow', () => {
  it('is up for ascending', () => {
    expect(dirArrow('asc')).toBe('↑');
  });

  it('is down for descending', () => {
    expect(dirArrow('desc')).toBe('↓');
  });
});

describe('the saved preferences', () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it('open with the defaults when nothing is saved', () => {
    expect(loadMarketPrefs()).toEqual({ sort: 'proj', dir: 'asc', mix: 'fixed' });
  });

  it('round-trip a saved view', () => {
    saveMarketPrefs({ filter: 'ignored', sort: 'vals', dir: 'desc', mix: 'usage' });
    expect(loadMarketPrefs()).toEqual({ sort: 'vals', dir: 'desc', mix: 'usage' });
  });

  it('never save the filter', () => {
    saveMarketPrefs({ filter: 'claude', sort: 'in', dir: 'asc', mix: 'fixed' });
    const raw = window.localStorage.getItem('jinn.web2.marketPrefs') ?? '';
    expect(raw).not.toContain('claude');
    expect(JSON.parse(raw)).not.toHaveProperty('filter');
  });

  it.each([
    ['a corrupt document', '{not json'],
    ['an unknown sort', '{"sort":"bogus","dir":"asc","mix":"fixed"}'],
    ['an unknown direction', '{"sort":"proj","dir":"sideways","mix":"fixed"}'],
    ['an unknown mix', '{"sort":"proj","dir":"asc","mix":"cosmic"}'],
    ['a non-object', '["proj"]'],
  ])('fall back to the defaults on %s', (_name, raw) => {
    window.localStorage.setItem('jinn.web2.marketPrefs', raw);
    expect(loadMarketPrefs()).toEqual({ sort: 'proj', dir: 'asc', mix: 'fixed' });
  });

  it('keep the valid fields when one is corrupt', () => {
    window.localStorage.setItem(
      'jinn.web2.marketPrefs',
      '{"sort":"bogus","dir":"desc","mix":"usage"}',
    );
    expect(loadMarketPrefs()).toEqual({ sort: 'proj', dir: 'desc', mix: 'usage' });
  });

  it('open with the defaults when storage is refused', () => {
    const get = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('refused');
    });
    expect(loadMarketPrefs()).toEqual({ sort: 'proj', dir: 'asc', mix: 'fixed' });
    get.mockRestore();
  });

  it('ignore a refused save', () => {
    const set = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('quota');
    });
    expect(() => saveMarketPrefs(onProj())).not.toThrow();
    set.mockRestore();
  });

  it('open on the saved sort and mix, with an empty filter', () => {
    saveMarketPrefs({ filter: 'claude', sort: 'eci', dir: 'desc', mix: 'usage' });
    expect(openingView()).toEqual({ filter: '', sort: 'eci', dir: 'desc', mix: 'usage' });
  });
});

describe('the sort-column guard', () => {
  it('accepts every column', () => {
    for (const sort of SORTS) expect(isMarketSort(sort)).toBe(true);
  });

  it('rejects anything else', () => {
    expect(isMarketSort('bogus')).toBe(false);
    expect(isMarketSort(3)).toBe(false);
    expect(isMarketSort(undefined)).toBe(false);
  });
});

describe('the model page URL', () => {
  it('strips the listing prefix', () => {
    expect(modelPageUrl('openrouter/anthropic/claude-sonnet-4.5')).toBe(
      'https://openrouter.ai/anthropic/claude-sonnet-4.5',
    );
  });

  it('keeps the variant suffix', () => {
    expect(modelPageUrl('openrouter/deepseek/deepseek-chat:free')).toBe(
      'https://openrouter.ai/deepseek/deepseek-chat:free',
    );
  });

  it('falls back to the raw id without the prefix', () => {
    expect(modelPageUrl('something/else')).toBe('https://openrouter.ai/something/else');
  });
});
