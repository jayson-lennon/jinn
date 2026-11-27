// The toast stack: every refusal and failure the server reports, as
// transient notices the page shows until they are dismissed.
//
// A toast is not the session's. A snapshot leaves them, because a refusal
// raised by an action is a fact about what the user just asked for, not
// about which session the page is viewing. The toast component owns each
// notice's lifetime; this file only stacks and removes them.

import type { ClientStore } from './store/store';
import { resolveLoadingAfterError } from './session_load';
import type { ServerMsg } from './protocol/protocol';

/** One transient notice the page shows until it is dismissed. */
export interface Toast {
  readonly id: number;
  readonly message: string;
}

let toastCounter = 0;

/** Stacks one notice. The toast component owns its lifetime. */
export function pushToast(store: ClientStore, message: string): void {
  const id = toastCounter++;
  store.set((s) => {
    s.toasts = [...s.toasts, { id, message }];
  });
}

/** Removes one notice, by id. */
export function dismissToast(store: ClientStore, id: number): void {
  store.set((s) => {
    s.toasts = s.toasts.filter((t) => t.id !== id);
  });
}

/** `error`: every refusal and every failure the server reports is one
 *  notice, and it ends any wait: the request it answers will never bring a
 *  snapshot, and the overlay must not outlive the answer. */
export function applyError(store: ClientStore, msg: Extract<ServerMsg, { type: 'error' }>): void {
  pushToast(store, msg.message);
  store.set((s) => {
    resolveLoadingAfterError(s);
  });
}
