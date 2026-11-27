// @vitest-environment jsdom
//
// The unread rule: a row is unread when its session settled after the page
// last saw it �� compared as instants, never as text. And the last-seen map
// that rule reads, which persists in the browser's localStorage and so
// needs a real `window`: this file runs under jsdom for that alone.

import { describe, it, expect, beforeEach } from 'vitest';
import { isUnread, listHasUnread, unreadCount } from './unread';
import { loadLastSeen, markSeenOnSnapshot, saveLastSeen, viewedRowSettledAt } from './last_seen';
import type { SessionRow } from '../protocol/protocol';

/** A session row that settled at `settled`, with the name an assertion reads. */
function row(id: string, settled: string | null): SessionRow {
  return {
    session_id: id,
    title: id,
    is_active: false,
    is_busy: false,
    is_tearing_down: false,
    in_library: false,
    last_settled_at: settled,
    parent_session: null,
    origin: 'user',
    depth: 0,
  } as SessionRow;
}

describe('isUnread', () => {
  it('a row that settled after the page last saw it is unread', () => {
    // Given a row that settled at noon, last seen an hour before.
    const r = row('s1', '2026-01-01T12:00:00Z');

    // When asking whether it is unread.
    const yes = isUnread(r, { s1: '2026-01-01T11:00:00Z' });

    // Then it is.
    expect(yes).toBe(true);
  });

  it('a row that settled before the page last saw it is not unread', () => {
    // Given a row that settled at 11, last seen at noon.
    const r = row('s1', '2026-01-01T11:00:00Z');

    // When asking whether it is unread.
    const yes = isUnread(r, { s1: '2026-01-01T12:00:00Z' });

    // Then it is not.
    expect(yes).toBe(false);
  });

  it('a row that never settled is not unread', () => {
    // Given a row with no settle time.
    const r = row('s1', null);

    // When asking whether it is unread, having never seen it.
    const yes = isUnread(r, {});

    // Then it is not: nothing settled to be unseen.
    expect(yes).toBe(false);
  });

  it('a settled row the page has never seen is unread', () => {
    // Given a row that settled, and no record of seeing it.
    const r = row('s1', '2026-01-01T12:00:00Z');

    // When asking whether it is unread.
    const yes = isUnread(r, {});

    // Then it is.
    expect(yes).toBe(true);
  });

  it('two instants with different fractional-digit counts compare as instants', () => {
    // Given a settle time with six fractional digits and a seen time with
    // three — text order would call the shorter one lesser, wrongly.
    const r = row('s1', '2026-01-01T12:00:00.100000Z');

    // When the page saw it at .099 (three digits).
    const yes = isUnread(r, { s1: '2026-01-01T12:00:00.099Z' });

    // Then the row is unread: the instants are compared, not the strings.
    expect(yes).toBe(true);
  });

  it('a row settling at exactly the moment it was seen is not unread', () => {
    // Given a settle time equal to the seen time.
    const r = row('s1', '2026-01-01T12:00:00Z');

    // When asking whether it is unread.
    const yes = isUnread(r, { s1: '2026-01-01T12:00:00Z' });

    // Then it is not: unread is strictly after.
    expect(yes).toBe(false);
  });
});

describe('unreadCount', () => {
  it('counts the unread rows across both lists', () => {
    // Given one unread row in each list and one read one.
    const sessions = [row('a', '2026-01-01T12:00:00Z'), row('b', null)];
    const library = [row('c', '2026-01-01T12:00:00Z')];

    // When counting.
    const n = unreadCount(sessions, library, {});

    // Then both unread rows are counted, whichever list they sit in.
    expect(n).toBe(2);
  });
});

describe('listHasUnread', () => {
  it('a list holding one unread row has unread', () => {
    // Given a list with a settled, never-seen row.
    // When asking.
    const yes = listHasUnread([row('a', '2026-01-01T12:00:00Z'), row('b', null)], {});

    // Then it does.
    expect(yes).toBe(true);
  });

  it('a list of rows all seen has none unread', () => {
    // Given a list whose one row settled before it was seen.
    // When asking.
    const yes = listHasUnread([row('a', '2026-01-01T11:00:00Z')], { a: '2026-01-01T12:00:00Z' });

    // Then it does not.
    expect(yes).toBe(false);
  });
});

/** The storage key `last_seen.ts` persists under. */
const KEY = 'jinn.web2.lastSeen';

beforeEach(() => {
  // Each test builds its own map; none may read one another left behind.
  window.localStorage.clear();
});

describe('viewedRowSettledAt', () => {
  it('the settle time of the viewed session, from whichever list holds it', () => {
    // Given the session sitting in the library list.
    // When reading its settle time.
    const settled = viewedRowSettledAt('a', [], [row('a', '2026-01-01T12:00:00Z')]);

    // Then it is found there.
    expect(settled).toBe('2026-01-01T12:00:00Z');
  });

  it('a session in neither list has no settle time', () => {
    // Given the session is not listed.
    // When reading its settle time.
    const settled = viewedRowSettledAt('nope', [], []);

    // Then there is none.
    expect(settled).toBeNull();
  });
});

describe('markSeenOnSnapshot', () => {
  it("records the row's own settle time, never the browser's clock", () => {
    // Given a snapshot for a session that settled at noon, and an empty map.
    const map: Record<string, string> = {};
    const sessions = [row('a', '2026-01-01T12:00:00Z')];

    // When the snapshot lands.
    markSeenOnSnapshot('a', sessions, [], map);

    // Then the seen time is the server's settle, not `now`.
    expect(map.a).toBe('2026-01-01T12:00:00Z');
  });

  it('a session that never settled is seen as of now', () => {
    // Given a snapshot for a session with no settle time.
    const map: Record<string, string> = {};
    const before = Date.now();

    // When the snapshot lands.
    markSeenOnSnapshot('a', [row('a', null)], [], map);

    // Then the seen time is the browser's clock, within the moment.
    const seen = Date.parse(map.a!);
    expect(seen).toBeGreaterThanOrEqual(before);
    expect(seen).toBeLessThanOrEqual(Date.now());
  });

  it('a session absent from both lists is still marked, as of now', () => {
    // Given a snapshot naming a session no row carries.
    const map: Record<string, string> = {};

    // When the snapshot lands.
    markSeenOnSnapshot('a', [], [], map);

    // Then it is marked seen, so it does not linger unread.
    expect(typeof map.a).toBe('string');
  });
});

describe('loadLastSeen', () => {
  it('an empty storage yields an empty map', () => {
    // Given nothing saved.
    // When loading.
    const seen = loadLastSeen();

    // Then the map is empty.
    expect(seen).toEqual({});
  });

  it('a saved map round-trips', () => {
    // Given a map saved under the key.
    window.localStorage.setItem(KEY, JSON.stringify({ a: '2026-01-01T12:00:00Z' }));

    // When loading.
    const seen = loadLastSeen();

    // Then every entry is back.
    expect(seen).toEqual({ a: '2026-01-01T12:00:00Z' });
  });

  it('an entry whose value is not a valid instant is dropped', () => {
    // Given a saved map with one good entry and one corrupt.
    window.localStorage.setItem(
      KEY,
      JSON.stringify({ a: '2026-01-01T12:00:00Z', bad: 'not-a-date' }),
    );

    // When loading.
    const seen = loadLastSeen();

    // Then only the good one survives.
    expect(seen).toEqual({ a: '2026-01-01T12:00:00Z' });
  });

  it('an entry whose value is not a string is dropped', () => {
    // Given a saved map with a number where an instant belongs.
    window.localStorage.setItem(KEY, JSON.stringify({ a: '2026-01-01T12:00:00Z', n: 42 }));

    // When loading.
    const seen = loadLastSeen();

    // Then the number is not an entry.
    expect(seen).toEqual({ a: '2026-01-01T12:00:00Z' });
  });

  it('a saved value that is not JSON yields an empty map', () => {
    // Given garbage under the key.
    window.localStorage.setItem(KEY, '{not json');

    // When loading.
    const seen = loadLastSeen();

    // Then the page starts with no memory, and no throw.
    expect(seen).toEqual({});
  });
});

describe('saveLastSeen', () => {
  it('saves the map less any session no longer listed', () => {
    // Given a map holding a listed and an unlisted session.
    const map = { a: '2026-01-01T12:00:00Z', gone: '2026-01-01T12:00:00Z' };

    // When saving against a list that holds only `a`.
    saveLastSeen(map, [row('a', null)], []);

    // Then only the listed one is persisted.
    expect(JSON.parse(window.localStorage.getItem(KEY)!)).toEqual({ a: '2026-01-01T12:00:00Z' });
  });

  it('an empty map saves as an empty object', () => {
    // Given no seen times.
    // When saving.
    saveLastSeen({}, [], []);

    // Then an empty object is written, not `undefined`.
    expect(window.localStorage.getItem(KEY)).toBe('{}');
  });
});
