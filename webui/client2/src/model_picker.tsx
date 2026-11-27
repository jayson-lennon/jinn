// The model picker: the title bar's model chip opens it, a row click
// switches the session's model, and its Refresh button asks the registry
// to rediscover.
//
// The rows live in the store and are the server's own — the shared
// provider row builder's output, filtered on the server in the filter the
// page last asked for. This component only renders them.
//
// `useFilter` lives here too: it is the one debounced filter request the
// page has, and every filterable modal in `App.tsx` reaches for it.

import { createEffect, For, Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientMsg, ModelRow } from './protocol';
import type { ClientStore } from './store';
import { Runs } from './runs';

/** Sends one command. */
export type Send = (msg: ClientMsg) => void;

/** Debounce for filter keystrokes, so typing does not request per key. */
export const FILTER_DEBOUNCE_MS = 150;

/** A debounced filter request; the store holds the filter so a stale
 *  answer (to text the user has typed past) can be recognised and dropped. */
export function useFilter(
  store: ClientStore,
  key: 'pickerFilter' | 'modelFilter' | 'skillFilter' | 'toolFilter' | 'mcpFilter' | 'personaFilter' | 'appletFilter',
  send: Send,
  request: (filter: string) => ClientMsg,
): (value: string) => void {
  let timer: number | null = null;
  return (value: string): void => {
    store.set((s) => { s[key] = value; });
    if (timer !== null) window.clearTimeout(timer);
    timer = window.setTimeout(() => send(request(value)), FILTER_DEBOUNCE_MS);
  };
}


export function ModelPicker(props: { store: ClientStore; send: Send; onClose: () => void }): JSX.Element {
  const onFilter = useFilter(props.store, 'modelFilter', props.send, (filter) => ({ type: 'model_rows', filter }));
  const pick = (row: ModelRow): void => {
    props.send({ type: 'switch_model', provider_id: row.provider_id });
    props.onClose();
  };
  // A refresh asks the registry to rediscover. The button stays busy until
  // the store's `refreshing` flag clears — which a pushed `model_rows`
  // frame does when discovery lands. The press is also gated by a plain
  // flag: store writes are batched, so a second click in the same tick
  // would still read `refreshing` as false and send a second refresh.
  let pressing = false;
  const refresh = (): void => {
    if (pressing || props.store.state.refreshing) return;
    pressing = true;
    props.store.set((s) => { s.refreshing = true; });
    props.send({ type: 'refresh_models' });
  };
  // The pushed rows that clear the store's busy state re-arm the press.
  createEffect(
    () => props.store.state.refreshing,
    (refreshing) => {
      if (!refreshing) pressing = false;
    },
  );
  const refreshing = (): boolean => props.store.state.refreshing;
  return (
    <div class="modal-backdrop" onClick={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
      <div class="modal" role="dialog" aria-label="Switch model">
        <div class="modal-head">
          <input
            class="modal-filter"
            data-testid="model-filter"
            autofocus
            placeholder="Filter models…"
            onInput={(e) => onFilter(e.currentTarget.value)}
            onKeyDown={(e) => { if (e.key === 'Escape') props.onClose(); }}
          />
          <button
            type="button"
            class="refresh-models"
            data-testid="refresh-models"
            disabled={refreshing()}
            aria-busy={refreshing() ? 'true' : undefined}
            onClick={refresh}
          >
            Refresh models
          </button>
        </div>
        <ul class="modal-list">
          <For each={props.store.state.modelRows}>
            {(row) => (
              <li>
                <button data-testid="model-row" disabled={!row.is_available} onClick={() => pick(row)}>
                  <span class="session-title"><Runs runs={row.model_runs} /></span>
                  <span class="session-meta">
                    <Runs runs={row.provider_runs} />
                    <Show when={row.is_active}> · current</Show>
                    <Show when={!row.is_available}> · unavailable</Show>
                  </span>
                </button>
              </li>
            )}
          </For>
        </ul>
      </div>
    </div>
  );
}

