// Turning a settle that arrived over the socket into a banner.
//
// This is the live-socket path: the page is open but hidden, so jinn never
// needed to push — the frame came in on its own. The push path exists for
// the page that is not running at all; both end in the same service worker,
// which decides whether to show anything.
//
// The decision needs a memory of its own. The gold mark's last-seen map is
// written for the *viewed* session on every frame, so it cannot say whether
// a settle is new; this map records the latest settle time each session has
// announced, and only a time that got newer is a settle. A page that is
// visible when the frame lands shows nothing — the gold row already says
// it — and is not replayed later.

import type { SessionRow } from '../protocol/protocol';

/** How the module reaches the worker. In production, the page's. */
export interface WorkerChannel {
  /** Posts one message to the service worker. */
  post(message: unknown): Promise<void>;
  /** Whether the document is hidden — the user is not looking at it. */
  hidden(): boolean;
}

/** The page's own channel: the active worker, and the document's visibility. */
export const pageChannel: WorkerChannel = {
  async post(message: unknown): Promise<void> {
    const registration = await navigator.serviceWorker.ready;
    await Promise.all(
      registration.active ? [registration.active.postMessage(message)] : [],
    );
  },
  hidden: (): boolean => document.visibilityState === 'hidden',
};

/** The latest settle time each session has announced, as the frames carried
 *  it — `null` for a session that is known but has not settled. A row seen
 *  for the first time is recorded and never notified on: the snapshot on
 *  load carries every past settle, and none of them is news. */
const seen = new Map<string, string | null>();

/** Reads a session list the frames just landed, and posts a banner for each
 *  settle that is new since the last list — but only while the page is
 *  hidden. Rows that dropped off the list are forgotten. */
export function notifySettles(rows: readonly SessionRow[], channel: WorkerChannel): number {
  let posted = 0;
  const present = new Set<string>();
  for (const row of rows) {
    present.add(row.session_id);
    // A row with no settle time still makes the session known, so the
    // settle that follows is news rather than a first sighting.
    const settled = row.last_settled_at;
    if (settled === null) {
      seen.set(row.session_id, null);
      continue;
    }
    const previous = seen.get(row.session_id);
    if (previous === settled) continue;
    seen.set(row.session_id, settled);
    // A first sighting is history, not news: the snapshot on load carries
    // every past settle, and none of them is a thing that just happened.
    if (previous === undefined) continue;
    if (!channel.hidden()) continue;
    void channel.post({
      type: 'settle',
      session_id: row.session_id,
      title: 'jinn',
      body: `Turn finished: ${row.title}`,
    });
    posted += 1;
  }
  for (const id of seen.keys()) {
    if (!present.has(id)) seen.delete(id);
  }
  return posted;
}

/** Forgets everything. A reconnect re-sends the snapshot, and every settle
 *  in it is history again. */
export function reset(): void {
  seen.clear();
}
