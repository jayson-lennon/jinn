// The page: one session's transcript, the composer, and the session rail.
//
// Everything here is a pure function of the store. Nothing holds state of
// its own beyond what a control is mid-editing (the draft, an open flag);
// every fact shown is read from the store *inside* the component, so it
// updates when the store does, and every gesture leaves through the one
// socket.

import { For, Show, Switch, Match, createEffect, createRenderEffect, createSignal } from 'solid-js';
import { render } from '@solidjs/web';
import type { JSX } from '@solidjs/web';
import type { ClientMsg, ModelRow } from './protocol';
import { type ClientStore, apply, newestEntryId, createStore } from './store';
import { type ConnectionState, openSocket } from './socket';
import { TranscriptEntry } from './transcript';
import { Menu, type MenuItem, type MenuGroup } from './menu';
import { SkillsModal } from './skills';
import { McpModal, PersonaModal, ToolsModal } from './pickers';
import { Composer } from './composer';
import { SessionRail } from './rail';
import { NewSessionModal } from './new-session';
import { applyTheme, mirrorTheme } from './theme';

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
  // The tools, MCP servers, and persona modals open the same way: drop
  // any rows a previous opening left behind, ask for the unfiltered rows,
  // and mount. A row click on tools or MCP toggles at once and the modal
  // stays open; a persona click switches and closes.
  const [toolsOpen, setToolsOpen] = createSignal(false);
  const onToolFilter = useFilter(store, 'toolFilter', send, (filter) => ({ type: 'tool_rows', filter }));
  const openTools = (): void => {
    store.set((s) => {
      s.toolFilter = '';
      s.toolRows = [];
    });
    send({ type: 'tool_rows', filter: '' });
    setToolsOpen(true);
  };
  const [mcpOpen, setMcpOpen] = createSignal(false);
  const onMcpFilter = useFilter(store, 'mcpFilter', send, (filter) => ({ type: 'mcp_rows', filter }));
  const openMcp = (): void => {
    store.set((s) => {
      s.mcpFilter = '';
      s.mcpRows = [];
    });
    send({ type: 'mcp_rows', filter: '' });
    setMcpOpen(true);
  };
  const [personaOpen, setPersonaOpen] = createSignal(false);
  const onPersonaFilter = useFilter(store, 'personaFilter', send, (filter) => ({ type: 'persona_rows', filter }));
  const openPersona = (): void => {
    store.set((s) => {
      s.personaFilter = '';
      s.personaRows = [];
    });
    send({ type: 'persona_rows', filter: '' });
    setPersonaOpen(true);
  };
  // New session needs no active session: it makes one. The choices are
  // asked for fresh on every open so the form never lists a project the
  // config has dropped.
  const [newSessionOpen, setNewSessionOpen] = createSignal(false);
  const openNewSession = (): void => {
    store.set((s) => { s.newSession = null; });
    send({ type: 'new_session_options' });
    setNewSessionOpen(true);
  };
  // Whether the phone's session drawer is slid in. The page owns it:
  // both the title bar's `#` and the menu's Sessions item open it, and a
  // picked row closes it.
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const openDrawer = (): void => {
    setDrawerOpen(true);
  };

  // The items are in panel order: New session first, then the pickers
  // (only while a session is active), then Sessions; the color-mode
  // group comes after every item, last.
  const menuItems = (): MenuItem[] => [
    { id: 'new-session', label: 'New session', run: openNewSession },
    ...(store.state.sessionId === null
      ? []
      : [
          { id: 'skills', label: 'Skills', run: openSkills },
          { id: 'persona', label: 'Persona', run: openPersona },
          { id: 'tools', label: 'Tools', run: openTools },
          { id: 'mcp', label: 'MCP servers', run: openMcp },
        ]),
    { id: 'sessions', label: 'Sessions', run: openDrawer },
  ];
  const menuGroups = (): MenuGroup[] => [
    {
      id: 'theme',
      label: 'Color mode',
      testId: 'theme-group',
      options: [
        { id: 'light', label: 'Light', active: store.state.theme === 'light' },
        { id: 'dark', label: 'Dark', active: store.state.theme === 'dark' },
        { id: 'system', label: 'System', active: store.state.theme === 'system' },
      ],
      pick: (id) => send({ type: 'set_theme', theme: id as 'light' | 'dark' | 'system' }),
    },
  ];

  // The theme follows the store: the snapshot seeds it and a `theme`
  // frame moves it. Both the attribute the stylesheet reads and the
  // pre-paint hint are written here, so the next load starts in mode.
  createEffect(
    // Compute (tracked): the store's theme.
    () => store.state.theme,
    // Effect (untracked): write it where the stylesheet and the next load
    // read it.
    (theme) => {
      applyTheme(theme);
      mirrorTheme(theme);
    },
  );

  return (
    <div class={`app${connection() === 'open' ? '' : ' disconnected'}`}>
      <SessionRail
        store={store}
        send={send}
        open={drawerOpen}
        setOpen={(next: boolean): void => {
          setDrawerOpen(next);
        }}
      />
      <main class="main">
        <TitleBar
          store={store}
          send={send}
          menuItems={menuItems()}
          menuGroups={menuGroups()}
          openDrawer={openDrawer}
          drawerOpen={drawerOpen}
        />
        <Transcript store={store} />
        <Composer store={store} send={send} />
      </main>
      <Show when={newSessionOpen()}>
        <NewSessionModal store={store} send={send} onClose={() => setNewSessionOpen(false)} />
      </Show>
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
      <Show when={toolsOpen()}>
        <ToolsModal
          rows={store.state.toolRows}
          filter={store.state.toolFilter}
          onFilter={onToolFilter}
          onToggle={(tool) => {
            send({ type: 'toggle_tool', tool });
          }}
          onClose={() => {
            setToolsOpen(false);
          }}
        />
      </Show>
      <Show when={mcpOpen()}>
        <McpModal
          rows={store.state.mcpRows}
          filter={store.state.mcpFilter}
          onFilter={onMcpFilter}
          onToggle={(server) => {
            send({ type: 'toggle_mcp', server });
          }}
          onClose={() => {
            setMcpOpen(false);
          }}
        />
      </Show>
      <Show when={personaOpen()}>
        <PersonaModal
          rows={store.state.personaRows}
          filter={store.state.personaFilter}
          onFilter={onPersonaFilter}
          onSwitch={(persona) => {
            // A persona switch is a confirm, not a toggle: the modal has
            // done its one job, so it closes on the click.
            send({ type: 'switch_persona', persona });
            setPersonaOpen(false);
          }}
          onClose={() => {
            setPersonaOpen(false);
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
      <For each={props.store.state.entries}>
        {(envelope) => <TranscriptEntry envelope={envelope} sessionId={props.store.state.sessionId} />}
      </For>
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

function TitleBar(props: {
  store: ClientStore;
  send: Send;
  menuItems: readonly MenuItem[];
  menuGroups: readonly MenuGroup[];
  /** Slides in the phone's session drawer. */
  openDrawer: () => void;
  /** Whether that drawer is slid in — what the `#` button reports. */
  drawerOpen: () => boolean;
}): JSX.Element {
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
      {/* Left of the hamburger, and only at phone widths: there the rail is an
          off-canvas drawer and this is what opens it. */}
      <button
        type="button"
        class="sessions-button"
        data-testid="sessions-button"
        aria-label="Sessions"
        aria-expanded={props.drawerOpen() ? 'true' : 'false'}
        aria-controls="session-rail"
        onClick={props.openDrawer}
      >
        #
      </button>
      <Menu items={props.menuItems} groups={props.menuGroups} />
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

/** A debounced filter request; the store holds the filter so a stale
 *  answer (to text the user has typed past) can be recognised and dropped. */
function useFilter(store: ClientStore, key: 'pickerFilter' | 'modelFilter' | 'skillFilter' | 'toolFilter' | 'mcpFilter' | 'personaFilter', send: Send, request: (filter: string) => ClientMsg) {
  let timer: number | null = null;
  return (value: string): void => {
    store.set((s) => { s[key] = value; });
    if (timer !== null) window.clearTimeout(timer);
    timer = window.setTimeout(() => send(request(value)), FILTER_DEBOUNCE_MS);
  };
}

function ModelPicker(props: { store: ClientStore; send: Send; onClose: () => void }): JSX.Element {
  const onFilter = useFilter(props.store, 'modelFilter', props.send, (filter) => ({ type: 'model_rows', filter }));
  const pick = (row: ModelRow): void => {
    props.send({ type: 'switch_model', provider_id: row.provider_id });
    props.onClose();
  };
  // A refresh asks the registry to rediscover. The button stays busy until
  // the store's `refreshing` flag clears — which a pushed `model_rows`
  // frame does when discovery lands.
  const refresh = (): void => {
    props.store.set((s) => { s.refreshing = true; });
    props.send({ type: 'refresh_models' });
  };
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
