// The toast region: every refusal and failure the server reports, stacked
// bottom-right until it is dismissed.
//
// A toast is not the session's. It is not cleared by a snapshot, because a
// refusal raised by an action is a fact about what the user just asked for,
// not about which session the page is viewing — so it outlives the session
// switch an archive's successor push performs.

import { For } from 'solid-js';
import { createSignal, onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientStore } from './store/store';
import { type Toast, dismissToast } from './toast_state';

/** How long a toast stays before it dismisses itself. */
const TOAST_MS = 5000;

/** The toast region: one row per notice, newest at the bottom. */
export function Toasts(props: { store: ClientStore }): JSX.Element {
  return (
    <div class="toasts" data-testid="toasts">
      <For each={props.store.state.toasts}>
        {(toast) => <ToastRow store={props.store} toast={toast} />}
      </For>
    </div>
  );
}

/** One notice: it dismisses itself, and a tap dismisses it at once. */
function ToastRow(props: { store: ClientStore; toast: Toast }): JSX.Element {
  // The timer belongs to this row. A tap that dismisses early — or a store
  // reset that drops the row — clears it in `onCleanup`, so no stale timer
  // fires a dismiss for an id that is already gone.
  const timer = window.setTimeout(() => dismissToast(props.store, props.toast.id), TOAST_MS);
  onCleanup(() => window.clearTimeout(timer));

  return (
    <button
      type="button"
      class="toast"
      role="status"
      data-testid="toast"
      onClick={() => dismissToast(props.store, props.toast.id)}
    >
      {props.toast.message}
    </button>
  );
}
