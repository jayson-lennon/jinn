// The session rows: the rail's two lists, and the title the title bar and
// the row share.
//
// Both lists are the server's own. A `sessions` frame replaces them whole
// and carries the hydration flag; a `title` frame is answered synchronously
// and no bus event carries it back, so one message updates the title in
// both places it is displayed — the bar and the sidebar row — and they
// cannot disagree.

import type { ClientStore } from '../store/store';
import { belongsToViewedSession } from '../store/store';
import { saveLastSeen, viewedRowSettledAt } from './last_seen';
import { notifySettles, pageChannel } from '../notifications/notify';
import type { ServerMsg } from '../protocol/protocol';

/** `sessions`: both lists, and the hydration flag every session-list frame sets. */
export function applySessions(store: ClientStore, msg: Extract<ServerMsg, { type: 'sessions' }>): void {
  // While a session is viewed, each settle it reports is seen: copy
  // the server's own time, so the comparison is exact rather than
  // against the browser's clock. The viewed session's row is in
  // whichever list holds it. A frame with no such settle changes
  // nothing, and the hot path then skips the storage write.
  const viewed = store.state.sessionId;
  const settled = viewed === null ? null : viewedRowSettledAt(viewed, msg.rows, msg.library);
  // Only a settle the map does not already hold is a change; the same
  // time arrives on every history append of the viewed session.
  const changed = settled !== null && store.state.lastSeen[viewed ?? ''] !== settled;
  store.set((s) => {
    s.sessions = msg.rows;
    s.library = msg.library;
    s.hydrating = msg.hydrating;
    if (viewed !== null && settled !== null) {
      s.lastSeen[viewed] = settled;
    }
  });
  if (changed) {
    saveLastSeen(store.state.lastSeen, msg.rows, msg.library);
  }
  // The live-socket path to a banner: a settle that is new since the last
  // list, posted to the worker while the page is hidden. It keeps its own
  // memory — the gold mark's last-seen map is written for the viewed
  // session on every frame and cannot say whether a settle is new.
  notifySettles([...msg.rows, ...msg.library], pageChannel);
}

/** `title`: the bar and the sidebar row, from one message. */
export function applyTitle(store: ClientStore, msg: Extract<ServerMsg, { type: 'title' }>): void {
  if (!belongsToViewedSession(store, msg.session_id)) return;
  store.set((s) => {
    s.title = msg.title;
    // The sidebar row shows the same title; one message updates both
    // places it is displayed, so they cannot disagree.
    s.sessions = s.sessions.map((row) =>
      row.session_id === msg.session_id ? { ...row, title: msg.title } : row,
    );
  });
}
