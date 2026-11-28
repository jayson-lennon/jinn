// The page: one session's transcript, the composer, and the session rail.
//
// Everything here is a pure function of the store. Nothing holds state of
// its own beyond what a control is mid-editing (the draft, an open flag);
// every fact shown is read from the store *inside* the component, so it
// updates when the store does, and every gesture leaves through the one
// socket.
//
// The transcript's scroll box is `transcript/transcript_scroll.tsx`, the
// title bar is `title_bar.tsx`, and the model picker is
// `pickers/model_picker.tsx`.

import { render } from '@solidjs/web';
import { For, Show, Switch, Match, createEffect, createSignal, onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientMsg } from '../protocol/protocol';
import { type ClientStore, apply, createStore } from '../store/store';
import { pushToast } from '../overlays/toast_state';
import { newestEntryId } from '../transcript/transcript_entries';
import { beginFork, clearSessionLoad } from '../overlays/session_load';
import { type ConnectionState, openSocket } from '../protocol/socket';
import { Transcript } from '../transcript/transcript_scroll';
import { useFilter, ModelPicker, type Send } from '../pickers/model_picker';
import { TitleBar } from './title_bar';
import { MarketModal } from '../pickers/market';
import { openingView } from '../pickers/market_state';
import { Menu, type MenuItem, type MenuGroup } from './menu';
import { SkillsModal } from '../pickers/skills';
import { AppletsModal } from '../pickers/applets';
import { McpModal, PersonaModal, ToolsModal } from '../pickers/pickers';
import { Composer } from '../composer/composer';
import { SessionRail } from '../rail/rail';
import { SessionStatus } from './status';
import { StatusBar } from './statusbar';
import { Dash } from '../dash/session_dash';
import { cancelJump, openDash } from '../dash/dash';
import { NewSessionModal } from '../pickers/new-session';
import { Toasts } from '../overlays/toast';
import { applyTheme, mirrorTheme } from './theme';
import { applyThemeBar } from './theme_bar';

/**
 * How long a requested session load may go unanswered before the page gives
 * up on it: the overlay clears and the stall is named. An in-memory switch
 * answers in milliseconds; a disk read takes longer; and a request that never
 * answers means the socket is dead or the server is wedged — an overlay that
 * can never clear is the one outcome the timeout exists to rule out.
 */
const SESSION_LOAD_STALL_MS = 30_000;
/** How long a jump must be under way before the navigating overlay shows, so
 *  a jump that lands at once — the common case on a quick link — never
 *  flashes it. */
const NAVIGATING_DELAY_MS = 150;

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

  // The dash's one close path. `closeDash` only drops the open flag; this
  // effect is what tells the server, so every dismiss — the scrim, Escape, a
  // pin, the MCP picker, a session switch's snapshot — reports the same way.
  createEffect(
    // Compute (tracked): whether the dash is open.
    () => store.state.dashOpen,
    // Effect (untracked): a fall from open to closed is reported, whatever
    // dropped the flag.
    (open, wasOpen) => {
      if (wasOpen === true && open === false) send({ type: 'dash_close' });
    }
  );
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

  // The market is reached from the model chip's chooser. It opens on the
  // saved sort and mix with an empty filter, and asks for its rows.
  const [marketOpen, setMarketOpen] = createSignal(false);
  const openMarket = (): void => {
    const view = openingView();
    store.set((s) => {
      s.marketFilter = '';
      s.marketRows = [];
      s.marketPinnedId = null;
      s.marketEndpoints = [];
      s.marketDetailSorting = null;
      s.marketSorting = null;
      s.marketModelFacts = null;
      s.marketStale = null;
      s.marketView = view;
    });
    // The open's frame is the first answer the PROJ header waits on.
    send({ type: 'market_open', view });
    setMarketOpen(true);
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
  // pre-paint hint are written here, so the next load starts in mode. The
  // browser bar's colour is read back from the stylesheet here too; and in
  // system mode the palette follows the OS without a store write, so the
  // bar gets a listener for that change — living only while this theme is
  // current, since `onCleanup` runs before the effect's next run.
  createEffect(
    // Compute (tracked): the store's theme.
    () => store.state.theme,
    // Effect (untracked): write it where the stylesheet and the next load
    // read it.
    (theme) => {
      applyTheme(theme);
      mirrorTheme(theme);
      applyThemeBar();
      if (theme === 'system') {
        const query = window.matchMedia('(prefers-color-scheme: light)');
        const onSchemeChange = (): void => applyThemeBar();
        query.addEventListener('change', onSchemeChange);
        onCleanup(() => query.removeEventListener('change', onSchemeChange));
      }
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

  // The navigating overlay, and its beat. The store's `navigating` is the
  // cause, and this effect watches it: a true arms one timer to show the
  // overlay, and any later run clears the timer first — so a jump that ends
  // inside the beat never shows it, and one that ends after hides it at once.
  const [navigating, setNavigating] = createSignal(false);
  let navigateTimer: number | null = null;
  createEffect(
    // Compute (tracked): is a jump under way?
    () => store.state.navigating,
    // Effect (untracked): show the overlay after a beat, or hide it now.
    (underWay) => {
      if (navigateTimer !== null) {
        window.clearTimeout(navigateTimer);
        navigateTimer = null;
      }
      if (!underWay) {
        setNavigating(false);
        return;
      }
      navigateTimer = window.setTimeout(() => {
        navigateTimer = null;
        setNavigating(true);
      }, NAVIGATING_DELAY_MS);
    },
  );
  onCleanup(() => {
    if (navigateTimer !== null) window.clearTimeout(navigateTimer);
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
          onOpenMarket={openMarket}
          openDrawer={openDrawer}
          drawerOpen={drawerOpen}
        />
        {/* The bar describes a session; with none active there is nothing
            to show, and the hamburger hides its session items for the same
            reason. */}
        <Show when={store.state.sessionId !== null}>
          <StatusBar
            persona={store.state.persona}
            contextLabel={store.state.contextLabel}
            onOpenPersona={openPersona}
            onOpenDash={() => openDash(store, send)}
          />
        </Show>
        <Transcript store={store} send={send} />
        <SessionStatus store={store} />
        <Composer store={store} send={send} />
      </main>
      <Show when={newSessionOpen()}>
        <NewSessionModal store={store} send={send} onClose={() => setNewSessionOpen(false)} />
      </Show>
      <Show when={marketOpen()}>
        <MarketModal store={store} send={send} onClose={() => setMarketOpen(false)} />
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
      <Show when={store.state.dashOpen}>
        <Dash store={store} send={send} onOpenMcp={openMcp} />
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
      {/* Raised while a pin's jump is under way, after a beat. It covers the
          page and swallows the pointer like the session-load overlay, with
          one way out: Cancel closes it and stops the server's stream. No
          live role — it holds a button, so it is a plain region. */}
      <Show when={navigating()}>
        <div class="navigating-overlay" data-testid="navigating-overlay">
          <div class="navigating-card">
            <span class="spinner" aria-hidden="true"><i /></span>
            <span class="navigating-label">Navigating…</span>
            <button
              type="button"
              class="navigating-cancel"
              data-testid="navigating-cancel"
              onClick={() => cancelJump(store, send)}
            >
              Cancel
            </button>
          </div>
        </div>
      </Show>
      {/* Raised by the page when it asks for a session. Nothing dismisses it
          — no click handler, no key handler: it ends when the load ends, by
          the requested session's snapshot, a refusal, or the stall timeout. */}
      <Show when={store.state.loadingSessionId !== null}>
        <div class="session-load-overlay" role="status" aria-live="polite" data-testid="session-load-overlay">
          <div class="session-load-card">
            <span class="spinner" aria-hidden="true"><i /></span>
            <span class="session-load-label">Loading session…</span>
          </div>
        </div>
      </Show>
      <Show when={connection() !== 'open'}>
        <div class="overlay" role="status" aria-live="polite" data-testid="overlay">
          <span class="spinner" aria-hidden="true"><i /></span>
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

export function mount(parent: HTMLElement): () => void {
  return render(() => <App />, parent);
}
