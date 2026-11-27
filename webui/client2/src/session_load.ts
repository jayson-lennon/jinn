// The session-load overlay: the one wait the page raises itself.
//
// The store's `loadingSessionId` is the overlay's cause. The page sets it
// when it asks for a session and the server clears it — by the requested
// session's snapshot, a refusal, or the page's own stall timeout. Nothing
// in between dismisses it: no click handler, no key handler.
//
// A fork is the one wait with no id to wait on. The child's id does not
// exist until the store mints it, so the wait rides a sentinel and ends on
// the first snapshot naming a session other than the fork's source. The
// sentinel and the source are module-level and live only here.

import type { ClientStore } from './store/store';

/** The loading id while the page waits on a fork it asked for. The child's
 *  id does not exist until the store mints it, so no session id can be
 *  waited on; a string that can never be a session id marks the wait. */
const FORK_PENDING = '\u0000fork';

/** The session the pending fork was taken from, or null when none is. */
let forkSource: string | null = null;

/** Marks `id` as the session the page is waiting on — the loading overlay's cause. */
export function beginSessionLoad(store: ClientStore, id: string): void {
  store.set((s) => {
    s.loadingSessionId = id;
  });
}

/** Raises the loading overlay for a fork of `source`. It ends when a snapshot
 *  names a session other than `source` — the fork's own — or by a refusal or
 *  the stall timeout, like any other wait. */
export function beginFork(store: ClientStore, source: string | null): void {
  forkSource = source;
  store.set((s) => {
    s.loadingSessionId = FORK_PENDING;
  });
}

/** Ends the wait, whatever ended it. */
export function clearSessionLoad(store: ClientStore): void {
  forkSource = null;
  store.set((s) => {
    s.loadingSessionId = null;
  });
}

/**
 * Resolves the wait inside the snapshot's setter, with the loading id as it
 * was read *before* the setter ran.
 *
 * No wait leaves the overlay alone. A fork's ends when the snapshot names
 * a session other than its source — one naming the source leaves the
 * overlay up, the fork has not landed yet. Any other wait ends only on the
 * snapshot naming the session the page asked for; one naming another
 * session — the old session's, answered when a disk load outlived the
 * handler's own wait — leaves the overlay up until the real one lands.
 */
export function resolveLoadingAfterSnapshot(
  loadingId: string | null,
  sessionId: string | null,
  s: { loadingSessionId: string | null },
): void {
  if (loadingId === null) {
    // No wait to end.
  } else if (loadingId === FORK_PENDING) {
    if (sessionId !== forkSource) s.loadingSessionId = null;
    if (s.loadingSessionId === null) forkSource = null;
  } else if (sessionId === loadingId) {
    s.loadingSessionId = null;
  }
}

/** Ends any wait inside the error arm's setter: the request the refusal
 *  answers will never bring a snapshot, and the overlay must not outlive
 *  the answer. A pending fork is forgotten with it. */
export function resolveLoadingAfterError(s: { loadingSessionId: string | null }): void {
  forkSource = null;
  s.loadingSessionId = null;
}
