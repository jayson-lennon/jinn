// The page: one session's transcript, the composer, and the session rail.
//
// Everything here is a pure function of the store. Nothing holds state of
// its own beyond what a control is mid-editing (the draft, an open flag);
// every fact shown is read from the store *inside* the component, so it
// updates when the store does, and every gesture leaves through the one
// socket.

import { For, Show, Switch, Match, createRenderEffect, createSignal } from 'solid-js';
import { render } from '@solidjs/web';
import type { JSX } from '@solidjs/web';
import type { ClientMsg, ModelRow, PickerRow } from './protocol';
import { type ClientStore, addOptimistic, apply, isBusy, newestEntryId, createStore } from './store';
import { type ConnectionState, openSocket } from './socket';
import { TranscriptEntry } from './transcript';
import { Menu, type MenuItem } from './menu';
import { SkillsModal } from './skills';

type Send = (msg: ClientMsg) => void;

/** Debounce for filter keystrokes, so typing does not request per key. */
const FILTER_DEBOUNCE_MS = 150;

export function App(): JSX.Element {
  const store = createStore();
  const [connection, setConnection] = createSignal<ConnectionState>('connecting');

  const socket = openSocket({
    onFrame: (msg) => apply(store, msg),
    onState: setConnection,
    cursor: () => ({
      last_entry_id: newestEntryId(store.state.entries),
      revision: store.state.synced ? store.state.revision : null,
    }),
  });
  const send: Send = (msg) => socket.send(msg);

  // The hamburger menu's items, in panel order. An item that has no
  // destination on this page is not listed, so the panel never offers a
  // control that leads nowhere.
  //
  // Skills edits the active session's skill filter, so it is offered
  // only while a session is active.
  const [skillsOpen, setSkillsOpen] = createSignal(false);
  const onSkillFilter = useFilter(store, 'skillFilter', send, (filter) => ({ type: 'skill_rows', filter }));
  const openSkills = (): void => {
    store.set((s) => {
      s.skillFilter = '';
      s.skillRows = [];
    });
    send({ type: 'skill_rows', filter: '' });
    setSkillsOpen(true);
  };
  const menuItems = (): MenuItem[] =>
    store.state.sessionId === null ? [] : [{ id: 'skills', label: 'Skills', run: openSkills }];

  return (
    <div class={`app${connection() === 'open' ? '' : ' disconnected'}`}>
      <SessionRail store={store} send={send} />
      <main class="main">
        <TitleBar store={store} send={send} menuItems={menuItems()} />
        <Transcript store={store} />
        <Composer store={store} send={send} />
      </main>
      <Show when={skillsOpen()}>
        <SkillsModal
          rows={store.state.skillRows}
          filter={store.state.skillFilter}
          onFilter={onSkillFilter}
          onToggle={(skill) => {
            send({ type: 'toggle_skill', skill });
          }}
          onLoad={(skill) => {
            send({ type: 'load_skill', skill });
          }}
          onClose={() => {
            setSkillsOpen(false);
          }}
        />
      </Show>
      <Show when={connection() !== 'open'}>
        <div class="overlay" role="status" aria-live="polite" data-testid="overlay">
          {connection() === 'connecting' ? 'connecting…' : 'disconnected — reconnecting…'}
        </div>
      </Show>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Transcript
// ---------------------------------------------------------------------------

function Transcript(props: { store: ClientStore }): JSX.Element {
  let node: HTMLElement | undefined;
  // Follow the stream: when the newest entry grows and the reader is at
  // the bottom, keep them there. A reader who scrolled up is left alone.
  const isNearBottom = (): boolean =>
    node !== undefined && node.scrollHeight - node.scrollTop - node.clientHeight < 80;
  createRenderEffect(
    // Compute (tracked): the number of entries and the newest one's text.
    () => {
      const entries = props.store.state.entries;
      const last = entries[entries.length - 1];
      return { count: entries.length, tail: last === undefined ? '' : textOf(last) };
    },
    // Effect (untracked): keep a reader who is at the bottom at the bottom.
    (now, prev) => {
      const grew = prev === undefined || now.count !== prev.count || now.tail !== prev.tail;
      if (grew && isNearBottom()) {
        queueMicrotask(() => {
          if (node) node.scrollTop = node.scrollHeight;
        });
      }
    },
  );
  return (
    <section class="transcript" data-testid="transcript" ref={node}>
      <For each={props.store.state.entries}>{(envelope) => <TranscriptEntry envelope={envelope} />}</For>
    </section>
  );
}

/** A crude text signature of an entry, enough to notice a stream growing. */
function textOf(envelope: import('./protocol').EntryEnvelope): string {
  const inner = envelope.entry.kind as Record<string, unknown>;
  const value = inner[Object.keys(inner)[0] ?? ''];
  if (typeof value === 'string') return value;
  if (typeof value === 'object' && value !== null) {
    const text = (value as { text?: unknown; content?: unknown }).text ?? (value as { content?: unknown }).content;
    if (typeof text === 'string') return text;
  }
  return '';
}

// ---------------------------------------------------------------------------
// Title bar: rename in place, model chip
// ---------------------------------------------------------------------------

function TitleBar(props: { store: ClientStore; send: Send; menuItems: readonly MenuItem[] }): JSX.Element {
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal('');
  const [picker, setPicker] = createSignal(false);

  const commit = (): void => {
    const title = draft().trim();
    setEditing(false);
    if (title !== '' && title !== props.store.state.title) props.send({ type: 'rename', title });
  };
  const openModels = (): void => {
    props.store.set((s) => { s.modelFilter = ''; s.modelRows = []; });
    props.send({ type: 'model_rows', filter: '' });
    setPicker(true);
  };

  return (
    <header class="title-bar">
      <Menu items={props.menuItems} />
      <Switch>
        <Match when={editing()}>
          <input
            class="title-edit"
            data-testid="title-edit"
            value={draft()}
            autofocus
            onInput={(e) => setDraft(e.currentTarget.value)}
            onBlur={commit}
            onKeyDown={(e) => {
              if (e.key === 'Enter') commit();
              if (e.key === 'Escape') setEditing(false);
            }}
          />
        </Match>
        <Match when={true}>
          <button
            class="title"
            data-testid="title"
            onClick={() => { setDraft(props.store.state.title); setEditing(true); }}
          >
            {props.store.state.title || 'Untitled Session'}
          </button>
        </Match>
      </Switch>
      <button class="model-chip" data-testid="model-chip" onClick={openModels}>
        {props.store.state.modelLabel || 'no model'}
      </button>
      <Show when={picker()}>
        <ModelPicker store={props.store} send={props.send} onClose={() => setPicker(false)} />
      </Show>
    </header>
  );
}

// ---------------------------------------------------------------------------
// Composer
// ---------------------------------------------------------------------------

function Composer(props: { store: ClientStore; send: Send }): JSX.Element {
  const [draft, setDraft] = createSignal('');
  const busy = (): boolean => isBusy(props.store.state.phase);
  const hasText = (): boolean => draft().trim() !== '';

  const submit = (): void => {
    const text = draft();
    if (text.trim() === '') return;
    // The message appears now; the server's own entry replaces it.
    addOptimistic(props.store, text);
    props.store.set((s) => { s.error = null; });
    setDraft('');
    props.send({ type: busy() ? 'queue' : 'send', text });
  };

  return (
    <footer class="composer">
      <textarea
        class="draft"
        data-testid="draft"
        placeholder={busy() ? 'Queue a message for after this turn…' : 'Message jinn…'}
        value={draft()}
        onInput={(e) => setDraft(e.currentTarget.value)}
        onKeyDown={(e) => {
          // Enter sends; Shift+Enter is a newline.
          if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            submit();
          }
        }}
      />
      <div class="composer-actions">
        <Switch>
          <Match when={busy()}>
            <button data-testid="queue" onClick={submit} disabled={!hasText()}>
              Queue
            </button>
            <button class="danger" data-testid="cancel" onClick={() => props.send({ type: 'cancel' })}>
              Cancel
            </button>
          </Match>
          <Match when={true}>
            <button class="primary" data-testid="send" onClick={submit} disabled={!hasText()}>
              Send
            </button>
          </Match>
        </Switch>
      </div>
      <Show when={props.store.state.error}>
        <div class="composer-error" role="alert" data-testid="composer-error">
          {props.store.state.error}
        </div>
      </Show>
    </footer>
  );
}

// ---------------------------------------------------------------------------
// Session rail: loaded sessions + the stored-session picker
// ---------------------------------------------------------------------------

function SessionRail(props: { store: ClientStore; send: Send }): JSX.Element {
  const [open, setOpen] = createSignal(false);
  const showPicker = (): void => {
    props.store.set((s) => { s.pickerFilter = ''; s.pickerRows = []; });
    props.send({ type: 'picker_rows', filter: '' });
    setOpen(true);
  };
  return (
    <nav class="rail">
      <button class="rail-action" data-testid="open-picker" onClick={showPicker}>
        Switch session…
      </button>
      <ul class="sessions">
        <For each={props.store.state.sessions}>
          {(row) => (
            <li>
              <button
                class={`session${row.is_active ? ' active' : ''}`}
                data-testid="session-row"
                data-session-id={row.session_id}
                onClick={() => props.send({ type: 'activate_session', session_id: row.session_id })}
              >
                <span class="session-title">{row.title || 'Untitled Session'}</span>
                <Show when={row.is_busy}>
                  <span class="session-busy" aria-label="running" />
                </Show>
              </button>
            </li>
          )}
        </For>
      </ul>
      <Show when={open()}>
        <SessionPicker store={props.store} send={props.send} onClose={() => setOpen(false)} />
      </Show>
    </nav>
  );
}

/** A debounced filter request; the store holds the filter so a stale
 *  answer (to text the user has typed past) can be recognised and dropped. */
function useFilter(store: ClientStore, key: 'pickerFilter' | 'modelFilter' | 'skillFilter', send: Send, request: (filter: string) => ClientMsg) {
  let timer: number | null = null;
  return (value: string): void => {
    store.set((s) => { s[key] = value; });
    if (timer !== null) window.clearTimeout(timer);
    timer = window.setTimeout(() => send(request(value)), FILTER_DEBOUNCE_MS);
  };
}

function SessionPicker(props: { store: ClientStore; send: Send; onClose: () => void }): JSX.Element {
  const onFilter = useFilter(props.store, 'pickerFilter', props.send, (filter) => ({ type: 'picker_rows', filter }));
  const pick = (row: PickerRow): void => {
    props.send({ type: 'activate_session', session_id: row.session_id });
    props.onClose();
  };
  return (
    <div class="modal-backdrop" onClick={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
      <div class="modal" role="dialog" aria-label="Switch session">
        <input
          class="modal-filter"
          data-testid="picker-filter"
          autofocus
          placeholder="Filter sessions by title…"
          onInput={(e) => onFilter(e.currentTarget.value)}
          onKeyDown={(e) => { if (e.key === 'Escape') props.onClose(); }}
        />
        <ul class="modal-list">
          <For each={props.store.state.pickerRows}>
            {(row) => (
              <li>
                <button data-testid="picker-row" onClick={() => pick(row)}>
                  <span class="session-title">{row.title || 'Untitled Session'}</span>
                  <span class="session-meta">
                    {row.state === 'Archived' ? 'archived' : ''}
                    <Show when={row.project}> · {row.project}</Show>
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

function ModelPicker(props: { store: ClientStore; send: Send; onClose: () => void }): JSX.Element {
  const onFilter = useFilter(props.store, 'modelFilter', props.send, (filter) => ({ type: 'model_rows', filter }));
  const pick = (row: ModelRow): void => {
    props.send({ type: 'switch_model', provider_id: row.provider_id });
    props.onClose();
  };
  return (
    <div class="modal-backdrop" onClick={(e) => { if (e.target === e.currentTarget) props.onClose(); }}>
      <div class="modal" role="dialog" aria-label="Switch model">
        <input
          class="modal-filter"
          data-testid="model-filter"
          autofocus
          placeholder="Filter models…"
          onInput={(e) => onFilter(e.currentTarget.value)}
          onKeyDown={(e) => { if (e.key === 'Escape') props.onClose(); }}
        />
        <ul class="modal-list">
          <For each={props.store.state.modelRows}>
            {(row) => (
              <li>
                <button data-testid="model-row" disabled={!row.is_available} onClick={() => pick(row)}>
                  <span class="session-title">{row.model}</span>
                  <span class="session-meta">
                    {row.provider_name}
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

/** Mounts the page; returns a function that unmounts it. */
export function mount(parent: HTMLElement): () => void {
  return render(() => <App />, parent);
}
