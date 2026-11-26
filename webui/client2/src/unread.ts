// The unread rule: which session rows carry a mark.
//
// A row is unread when the session settled to Idle after the page last
// saw it. The settle time is the server's fact, carried on the row; the
// last-seen time is the page's own, kept in the store. Both are compared
// as instants, never as strings — two RFC 3339 strings with different
// fractional-digit counts do not order correctly as text.
//
// These are pure functions over plain rows and a plain map, so they carry
// no store dependency and the page's components can call them per render.

import type { SessionRow } from './protocol';

/** The store's last-seen times: session id -> when the page last saw it. */
export type LastSeen = Readonly<Record<string, string>>;

/** Whether a session settled after the page last saw it. */
export function isUnread(row: SessionRow, lastSeen: LastSeen): boolean {
  if (row.last_settled_at === null) return false;
  const settled = Date.parse(row.last_settled_at);
  const seen = lastSeen[row.session_id];
  // No record of seeing it: whatever settled is unseen.
  if (seen === undefined) return true;
  return settled > Date.parse(seen);
}

/** How many rows in one list are unread. */
function countUnread(rows: readonly SessionRow[], lastSeen: LastSeen): number {
  return rows.reduce((total, row) => (isUnread(row, lastSeen) ? total + 1 : total), 0);
}

/**
 * How many sessions are unread across both sidebar lists.
 *
 * The phone's `#` count spans both, so a number on the button always
 * matches a marked row somewhere in the drawer.
 */
export function unreadCount(
  sessions: readonly SessionRow[],
  library: readonly SessionRow[],
  lastSeen: LastSeen,
): number {
  return countUnread(sessions, lastSeen) + countUnread(library, lastSeen);
}

/** Whether any row in one list is unread — the list's tab is then marked. */
export function listHasUnread(rows: readonly SessionRow[], lastSeen: LastSeen): boolean {
  return rows.some((row) => isUnread(row, lastSeen));
}
