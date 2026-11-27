// The last-seen times: when the page last saw each session, kept so the
// unread rule can compare a row's settle time against it.
//
// The map is the store's; its persistence is the browser's localStorage,
// so each browser keeps its own unread marks and a mark survives a reload.
// The time recorded is always the server's own settle time for the
// session, never the browser's clock: the two run on different machines
// and drift, and a browser that is behind would record a "seen" older
// than the settle and never clear the mark.
//
// These are plain functions over plain rows and a plain map. Nothing here
// touches the store, so a caller can run one inside its own setter.

import type { LastSeen } from './unread';
import type { SessionRow } from '../protocol/protocol';

/**
 * One line on the console for an unread-mark decision. Off unless the
 * page is opened with `?debug=unread`: the mark is a quiet feature and
 * normal operation logs nothing, but a mark that will not clear is only
 * diagnosable from the two times behind it.
 */
const UNREAD_DEBUG: boolean = (() => {
  try {
    return new URLSearchParams(window.location.search).get('debug') === 'unread';
  } catch {
    return false;
  }
})();

function debugUnread(message: string, fields: Record<string, unknown>): void {
  if (!UNREAD_DEBUG) return;
  console.debug(`[unread] ${message}`, fields);
}

/** Where the last-seen map persists, so a mark survives a reload. */
const LAST_SEEN_KEY = 'jinn.web2.lastSeen';

/**
 * The saved last-seen map, or an empty one. Storage can be refused
 * (private mode, a quota) and a saved value can be stale or corrupt;
 * either way the page starts with no memory and marks nothing as read
 * that it cannot prove it saw.
 */
export function loadLastSeen(): Record<string, string> {
  try {
    const raw = window.localStorage.getItem(LAST_SEEN_KEY);
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(
        (entry): entry is [string, string] =>
          typeof entry[1] === 'string' && Number.isFinite(Date.parse(entry[1])),
      ),
    );
  } catch {
    return {};
  }
}

/** The row's settle time when it is the viewed session's, in either list. */
export function viewedRowSettledAt(
  sessionId: string,
  sessions: readonly SessionRow[],
  library: readonly SessionRow[],
): string | null {
  const row = [...sessions, ...library].find((r) => r.session_id === sessionId);
  return row === undefined || row.last_settled_at === null ? null : row.last_settled_at;
}

/**
 * Persists the map, less any session no longer listed: archived sessions
 * must not accumulate. A refused write leaves the in-memory map intact.
 */
export function saveLastSeen(lastSeen: LastSeen, sessions: readonly SessionRow[], library: readonly SessionRow[]): void {
  const listed = new Set([...sessions, ...library].map((row) => row.session_id));
  const kept = Object.fromEntries(
    Object.entries(lastSeen).filter(([id]) => listed.has(id)),
  );
  try {
    window.localStorage.setItem(LAST_SEEN_KEY, JSON.stringify(kept));
  } catch {
    // As `theme.ts`: the hint is an optimization, never a correctness need.
  }
}

/** The viewed session of a snapshot is seen from the moment it lands:
 *  whatever settled before this moment is seen. One arm covers a click on
 *  the page and a switch made elsewhere that the page follows — both land
 *  here. Called inside the snapshot's setter. */
export function markSeenOnSnapshot(
  sessionId: string,
  sessions: readonly SessionRow[],
  library: readonly SessionRow[],
  lastSeen: Record<string, string>,
): void {
  const settled = viewedRowSettledAt(sessionId, sessions, library);
  const seen = settled ?? new Date().toISOString();
  lastSeen[sessionId] = seen;
  if (UNREAD_DEBUG) {
    debugUnread('snapshot: marked seen', {
      session: sessionId,
      seenAt: seen,
      rowSettledAt: settled,
    });
  }
}
