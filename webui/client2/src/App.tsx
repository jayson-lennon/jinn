// The page: one session's transcript, the composer, and the session rail.
//
// Everything here is a pure function of the store. Nothing holds state of
// its own beyond what a control is mid-editing (the draft, an open flag);
// every fact shown is read from the store *inside* the component, so it
// updates when the store does, and every gesture leaves through the one
// socket.

import { For, Show, Switch, Match, createEffect, createRenderEffect, createSignal, onCleanup, onSettled } from 'solid-js';
import { render } from '@solidjs/web';
import type { JSX } from '@solidjs/web';
import type { ClientMsg, ModelRow } from './protocol';
import { type ClientStore, apply, beginFork, clearSessionLoad, newestEntryId, createStore, pushToast } from './store';
import { type ConnectionState, openSocket } from './socket';
import { TranscriptEntry, useEntryMenuDismiss } from './transcript';
import { Menu, type MenuItem, type MenuGroup } from './menu';
import { SkillsModal } from './skills';
import { AppletsModal } from './applets';
import { McpModal, PersonaModal, Runs, ToolsModal } from './pickers';
import { Composer } from './composer';
import { SessionRail } from './rail';
import { SessionStatus } from './status';
import { NewSessionModal } from './new-session';
import { Toasts } from './toast';
import { applyTheme, mirrorTheme } from './theme';
import { unreadCount } from './unread';

type Send = (msg: ClientMsg) => void;

/** Debounce for filter keystrokes, so typing does not request per key. */
const FILTER_DEBOUNCE_MS = 150;

/**
 * How long a requested session load may go unanswered before the page gives
 * up on it: the overlay clears and the stall is named. An in-memory switch
 * answers in milliseconds; a disk read takes longer; and a request that never
 * answers means the socket is dead or the server is wedged — an overlay that
 * can never clear is the one outcome the timeout exists to rule out.
 */
const SESSION_LOAD_STALL_MS = 10_000;

/** What the composer shows when a session load times out. */
const SESSION_LOAD_STALL_MESSAGE = 'Session switch timed out. The session did not load.';

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
  // The applets modal opens like the pickers: drop any rows a previous
  // opening left behind, ask for the unfiltered rows, and mount. The ask
  // also asks the applets slice to rescan and to wake its server, so the
  // first rows may be stale and the fresh catalog arrives as a push.
  const [appletsOpen, setAppletsOpen] = createSignal(false);
  const onAppletFilter = useFilter(store, 'appletFilter', send, (filter) => ({ type: 'applet_rows', filter }));
  const openApplets = (): void => {
    store.set((s) => {
      s.appletFilter = '';
      s.appletRows = [];
    });
    send({ type: 'applet_rows', filter: '' });
    setAppletsOpen(true);
  };
  // Whether the phone's session drawer is slid in. The page owns it:
  // both the title bar's `#` and the menu's Sessions item open it, and a
  // picked row closes it.
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const openDrawer = (): void => {
    setDrawerOpen(true);
  };

  // The items are in panel order: New session first, then Archive session
  // and the pickers (only while a session is active), then Applets, then
  // Sessions; the color-mode group comes after every item, last. Applets
  // sits outside the session gate because the saved-app catalog is
  // instance-wide: it has nothing to do with the session, so it is listed
  // on every page.
  const archiveSession = (): void => {
    const id = store.state.sessionId;
    if (id !== null) send({ type: 'archive_session', session_id: id });
  };
  const menuItems = (): MenuItem[] => [
    { id: 'new-session', label: 'New session', run: openNewSession },
    ...(store.state.sessionId === null
      ? []
      : [
          { id: 'archive', label: 'Archive session', run: archiveSession },
          { id: 'skills', label: 'Skills', run: openSkills },
          { id: 'persona', label: 'Persona', run: openPersona },
          { id: 'tools', label: 'Tools', run: openTools },
          { id: 'mcp', label: 'MCP servers', run: openMcp },
        ]),
    { id: 'applets', label: 'Applets', run: openApplets },
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

  // The session-load stall timeout. The store's loading id is the cause of
  // the overlay, and this effect watches it: a non-null id arms one timer
  // for the wait, and any later run — the id cleared by a snapshot or a
  // refusal, or replaced by a newer request — clears the timer first, so a
  // timer never fires for a load that already ended.
  let stallTimer: number | null = null;
  createEffect(
    // Compute (tracked): which session the page is waiting on.
    () => store.state.loadingSessionId,
    // Effect (untracked): arm or clear the timer.
    (loadingId) => {
      if (stallTimer !== null) {
        window.clearTimeout(stallTimer);
        stallTimer = null;
      }
      if (loadingId === null) return;
      stallTimer = window.setTimeout(() => {
        stallTimer = null;
        clearSessionLoad(store);
        pushToast(store, SESSION_LOAD_STALL_MESSAGE);
      }, SESSION_LOAD_STALL_MS);
    },
  );
  onCleanup(() => {
    if (stallTimer !== null) window.clearTimeout(stallTimer);
  });

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
        <Transcript store={store} send={send} />
        <SessionStatus store={store} />
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
      <Show when={appletsOpen()}>
        <AppletsModal
          rows={store.state.appletRows}
          filter={store.state.appletFilter}
          serving={store.state.appletServing.serving}
          bind={store.state.appletServing.bind}
          onFilter={onAppletFilter}
          onClose={() => {
            setAppletsOpen(false);
          }}
        />
      </Show>
      {/* Raised by the page when it asks for a session. Nothing dismisses it
          — no click handler, no key handler: it ends when the load ends, by
          the requested session's snapshot, a refusal, or the stall timeout. */}
      <Show when={store.state.loadingSessionId !== null}>
        <div class="session-load-overlay" role="status" aria-live="polite" data-testid="session-load-overlay">
          <div class="session-load-card">
            <span class="spinner" aria-hidden="true" />
            <span class="session-load-label">Loading session…</span>
          </div>
        </div>
      </Show>
      <Show when={connection() !== 'open'}>
        <div class="overlay" role="status" aria-live="polite" data-testid="overlay">
          <span class="spinner" aria-hidden="true" />
          <span>{connection() === 'connecting' ? 'connecting…' : 'disconnected — reconnecting…'}</span>
        </div>
      </Show>
      <Toasts store={store} />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Transcript
// ---------------------------------------------------------------------------

/** How close to an edge, in pixels, still counts as being at it. One value
 *  drives both the follow rule and the two jump buttons, so a hidden
 *  jump-to-end always means the stream keeps the reader at the end. */
const EDGE_BAND_PX = 80;

function Transcript(props: { store: ClientStore; send: (msg: ClientMsg) => void }): JSX.Element {
  let node: HTMLElement | undefined;
  // Which ends of the transcript the view is off. The page's own view state:
  // it lives here and never reaches the store or the socket.
  const [offStart, setOffStart] = createSignal(false);
  const [offEnd, setOffEnd] = createSignal(false);
  // At most one entry menu is open across the whole transcript, so the
  // open id is the transcript's — the way the rail owns its row menu.
  const [menuFor, setMenuFor] = createSignal<string | null>(null);
  const toggleMenu = (id: string): void => {
    setMenuFor(menuFor() === id ? null : id);
  };
  useEntryMenuDismiss(menuFor, () => setMenuFor(null));
  // Follow the stream: when the newest entry grows and the reader is at
  // the bottom, keep them there. A reader who scrolled up is left alone.
  const isNearBottom = (): boolean =>
    node !== undefined && node.scrollHeight - node.scrollTop - node.clientHeight < EDGE_BAND_PX;
  // Reads the scroll box and records which ends the view is off. Called on
  // every scroll, after the content changes, and on resize.
  const measure = (): void => {
    if (node === undefined) return;
    setOffStart(node.scrollTop > EDGE_BAND_PX);
    setOffEnd(node.scrollHeight - node.scrollTop - node.clientHeight > EDGE_BAND_PX);
  };
  // An instant scroll: the reader asked for an edge and lands on it now.
  const jumpTo = (top: number): void => {
    if (node === undefined) return;
    node.scrollTop = top;
    measure();
  };
  createRenderEffect(
    // Compute (tracked): the number of entries and the newest one's text.
    () => {
      const entries = props.store.state.entries;
      const last = entries[entries.length - 1];
      return { count: entries.length, tail: last === undefined ? '' : textOf(last) };
    },
    // Effect (untracked): keep a reader who is at the bottom at the bottom.
    // The decision is read before the microtask is queued: it asks where the
    // reader is now, before the new entry has grown the box under them.
    (now, prev) => {
      const follow = (prev === undefined || now.count !== prev.count || now.tail !== prev.tail) && isNearBottom();
      queueMicrotask(() => {
        if (!node) return;
        if (follow) node.scrollTop = node.scrollHeight;
        // After the layout the new entry produced: the end may now be out
        // of reach, and no scroll event follows a content change.
        measure();
      });
      // And once now, for a reader who was not followed: the content grew
      // while they read, and the next scroll may be a long time coming.
      measure();
    },
  );
  // Older entries, on scroll. The transcript is a bounded tail; the top of
  // it is the edge of what the page holds, not the start of the session.
  const oldestHeld = (): string | undefined => props.store.state.entries[0]?.entry.id;
  const requestOlder = (): void => {
    const s = props.store.state;
    if (s.loadingMore || s.entriesBefore === 0 || !s.synced) return;
    const before = oldestHeld();
    if (before === undefined) return;
    props.store.set((v) => {
      v.loadingMore = true;
    });
    props.send({ type: 'more_entries', before });
  };
  // Scroll anchoring across a prepend: the browser keeps `scrollTop` fixed,
  // which would jump the reader up by the page's height. Record the height
  // before, and restore the delta after the rows land.
  let heightBeforePage: number | null = null;
  createRenderEffect(
    () => props.store.state.loadingMore,
    (loading, prev) => {
      if (loading && node) {
        heightBeforePage = node.scrollHeight;
        return;
      }
      if (prev === true && !loading && node && heightBeforePage !== null) {
        const delta = node.scrollHeight - heightBeforePage;
        heightBeforePage = null;
        queueMicrotask(() => {
          if (node) node.scrollTop += delta;
        });
      }
    },
  );
  // One scroll handler carries both behaviours: the jump buttons' measure
  // and the older-page request, so neither can be wired without the other.
  const onTranscriptScroll = (): void => {
    measure();
    if (node && node.scrollTop < 160) requestOlder();
  };
  // The box resizes when the window does, when the phone's drawer opens,
  // and when the chips strip appears above the composer — each of those
  // moves an edge under a reader who has not scrolled, so no scroll event
  // reports it. The observer does. It is set up once the page has settled
  // (the ref is attached by then) and torn down when this component goes.
  onSettled(() => {
    measure();
    if (node === undefined) return;
    const observer = new ResizeObserver(() => measure());
    observer.observe(node);
    return () => observer.disconnect();
  });
  return (
    <div class="transcript-wrap">
      <section class="transcript" data-testid="transcript" ref={node} onScroll={onTranscriptScroll}>
        <Show when={props.store.state.loadingMore}>
          <div class="older-spinner" data-testid="older-spinner">
            <span class="spinner spinner-small" aria-hidden="true" /> loading older entries…
          </div>
        </Show>
        <Show when={!props.store.state.loadingMore && props.store.state.entriesBefore > 0}>
          <button type="button" class="older-more" data-testid="older-more" onClick={requestOlder}>
            load older entries
          </button>
        </Show>
        {/* Keyed by entry id, not by the envelope object: an upsert replaces
            the envelope in place, and a reference-keyed list would mount a
            second row for the same entry — one user entry, two images. */}
        <For each={props.store.state.entries}>
          {(envelope) => (
            <TranscriptEntry
              envelope={envelope}
              sessionId={props.store.state.sessionId}
              menu={{
                openFor: menuFor,
                toggle: toggleMenu,
                send: props.send,
                beginFork: (source) => beginFork(props.store, source),
              }}
            />
          )}
        </For>
      </section>
      {/* The two jump buttons float over the box's corners as its siblings,
          so they never scroll away with the entries. Each is shown only
          while the view is off that end. */}
      <Show when={offStart()}>
        <button
          type="button"
          class="jump jump-start"
          data-testid="jump-start"
          aria-label="Jump to start"
          onClick={() => jumpTo(0)}
        >
          <ArrowToLineIcon direction="up" />
        </button>
      </Show>
      <Show when={offEnd()}>
        <button
          type="button"
          class="jump jump-end"
          data-testid="jump-end"
          aria-label="Jump to end"
          onClick={() => jumpTo(node === undefined ? 0 : node.scrollHeight)}
        >
          <ArrowToLineIcon direction="down" />
        </button>
      </Show>
    </div>
  );
}

/** An arrow pointing at a bar — up for the start of the transcript, down
 *  for its end. Inline SVG: the glyph is identical on every OS and takes
 *  its colour from the button, where a Unicode arrow falls back to another
 *  font on some phones. */
function ArrowToLineIcon(props: { direction: 'up' | 'down' }): JSX.Element {
  const up = props.direction === 'up';
  return (
    <svg
      class="jump-icon"
      viewBox="0 0 24 24"
      width="18"
      height="18"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {/* The bar: above an up arrow, below a down arrow. */}
      <path d={up ? 'M5 3h14' : 'M5 21h14'} />
      {/* The arrow: shaft, then its head. */}
      <path d={up ? 'M12 21V8' : 'M12 3v13'} />
      <path d={up ? 'm6 14 6-6 6 6' : 'm6 10 6 6 6-6'} />
    </svg>
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
  // Across both lists: the number on the button is the number of marked
  // rows the drawer holds, wherever they are.
  const unread = (): number =>
    unreadCount(props.store.state.sessions, props.store.state.library, props.store.state.lastSeen);

  return (
    <header class="title-bar">
      {/* Left of the hamburger, and only at phone widths: there the rail is an
          off-canvas drawer and this is what opens it. Marked like /web's
          button: a gold fill and the count of sessions that settled
          unseen, so a phone user knows the drawer is worth opening. */}
      <button
        type="button"
        class={`sessions-button${unread() > 0 ? ' unread' : ''}`}
        data-testid="sessions-button"
        aria-label={unread() > 0 ? `Sessions, ${unread()} unread` : 'Sessions'}
        aria-expanded={props.drawerOpen() ? 'true' : 'false'}
        aria-controls="session-rail"
        onClick={props.openDrawer}
      >
        #
        <Show when={unread() > 0}>
          <span class="sessions-button-count" data-testid="sessions-button-count">{unread()}</span>
        </Show>
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
function useFilter(store: ClientStore, key: 'pickerFilter' | 'modelFilter' | 'skillFilter' | 'toolFilter' | 'mcpFilter' | 'personaFilter' | 'appletFilter', send: Send, request: (filter: string) => ClientMsg) {
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

/** Mounts the page; returns a function that unmounts it. */
export function mount(parent: HTMLElement): () => void {
  return render(() => <App />, parent);
}
