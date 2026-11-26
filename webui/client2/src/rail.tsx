// The session rail: the sidebar's two lists, and the drawer they become
// on a phone.
//
// The rail is always in the document. Which list it shows is store state
// (so a push can update it and a snapshot can reset it). Whether it is
// slid in belongs to the page, because two things open it: the title
// bar's `#` button and the hamburger menu's Sessions item. Both reach it
// through one signal the page owns. Below 768px the stylesheet takes the
// rail off-canvas and the drawer's scrim is live; at wider widths the
// stylesheet pins the rail in place and hides the scrim, so the open
// flag is simply inert there.

import { For, Show, createSignal, onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientStore } from './store';
import { isUnread, listHasUnread } from './unread';

export interface RailProps {
  store: ClientStore;
  /** Sends one command; a row click activates, a kebab item sets membership. */
  send: (msg: import('./protocol').ClientMsg) => void;
  /** Whether the drawer is slid in, and the setter that slides it. */
  open: () => boolean;
  setOpen: (open: boolean) => void;
}

export function SessionRail(props: RailProps): JSX.Element {
  const open = (): boolean => props.open();
  const setOpen = props.setOpen;
  const [picker, setPicker] = createSignal(false);
  const [menuFor, setMenuFor] = createSignal<string | null>(null);

  const activate = (id: string): void => {
    props.send({ type: 'activate_session', session_id: id });
    // Picking a row is the drawer's last act: a phone user tapped it to
    // go there, and the drawer in front of the transcript is now in the way.
    setOpen(false);
    setMenuFor(null);
  };
  const showPicker = (): void => {
    props.store.set((s) => { s.pickerFilter = ''; s.pickerRows = []; });
    props.send({ type: 'picker_rows', filter: '' });
    setPicker(true);
  };
  const setLibrary = (id: string, inLibrary: boolean): void => {
    props.send({ type: 'set_library', session_id: id, in_library: inLibrary });
    setMenuFor(null);
  };

  // Escape closes the top overlay: the kebab popup, then the drawer. The
  // menu and every modal register their own listener and stop the event
  // when they handle it, so this only fires when nothing above took it.
  const onKeydown = (event: KeyboardEvent): void => {
    if (event.key !== 'Escape') return;
    if (menuFor() !== null) {
      setMenuFor(null);
      event.stopPropagation();
      return;
    }
    if (open()) {
      setOpen(false);
      event.stopPropagation();
    }
  };
  document.addEventListener('keydown', onKeydown);
  onCleanup(() => document.removeEventListener('keydown', onKeydown));

  // Any click outside an open kebab popup closes it. The popup's own
  // clicks stop propagation, so a click that reaches the document is
  // outside.
  const onClick = (): void => {
    setMenuFor(null);
  };
  document.addEventListener('click', onClick);
  onCleanup(() => document.removeEventListener('click', onClick));

  const rows = (): import('./protocol').SessionRow[] =>
    props.store.state.railView === 'library' ? props.store.state.library : props.store.state.sessions;

  // A tab is marked when its own list holds an unread row, so the mark
  // always points at a row the user can find by selecting the tab.
  const sessionsTabUnread = (): boolean => listHasUnread(props.store.state.sessions, props.store.state.lastSeen);
  const libraryTabUnread = (): boolean => listHasUnread(props.store.state.library, props.store.state.lastSeen);

  return (
    <>
      <div
        class={`drawer-scrim${open() ? ' open' : ''}`}
        data-testid="drawer-scrim"
        onClick={() => setOpen(false)}
      />
      <nav id="session-rail" class={`rail${open() ? ' open' : ''}`} data-testid="rail">
        <div class="rail-toggle" role="tablist" aria-label="sidebar list">
          <button
            type="button"
            role="tab"
            class={`rail-tab${sessionsTabUnread() ? ' unread' : ''}`}
            data-testid="sessions-view"
            aria-selected={props.store.state.railView === 'sessions' ? 'true' : 'false'}
            onClick={() => props.store.set((s) => { s.railView = 'sessions'; })}
          >
            Sessions
          </button>
          <button
            type="button"
            role="tab"
            class={`rail-tab${libraryTabUnread() ? ' unread' : ''}`}
            data-testid="library-view"
            aria-selected={props.store.state.railView === 'library' ? 'true' : 'false'}
            onClick={() => props.store.set((s) => { s.railView = 'library'; })}
          >
            Library
          </button>
        </div>
        <button class="rail-action" data-testid="open-picker" onClick={showPicker}>
          Archive
        </button>
        <ul class="sessions" data-testid="rail-rows">
          <For each={rows()}>
            {(row) => (
              <li>
                <div class="session-row" data-testid="session-row-wrap" data-session-id={row.session_id}>
                  <button
                    class={`session${row.is_active ? ' active' : ''}${isUnread(row, props.store.state.lastSeen) ? ' unread' : ''}`}
                    data-testid="session-row"
                    data-session-id={row.session_id}
                    onClick={() => activate(row.session_id)}
                  >
                    <span class="session-title">{row.title || 'Untitled Session'}</span>
                    <Show when={row.is_busy}>
                      <span class="session-busy" aria-label="running" />
                    </Show>
                  </button>
                  <div class="row-menu">
                    <button
                      type="button"
                      class="row-menu-button"
                      data-testid="row-menu"
                      aria-label="row actions"
                      aria-haspopup="true"
                      aria-expanded={menuFor() === row.session_id ? 'true' : 'false'}
                      onClick={(e) => {
                        e.stopPropagation();
                        setMenuFor(menuFor() === row.session_id ? null : row.session_id);
                      }}
                    >
                      ⋮
                    </button>
                    <Show when={menuFor() === row.session_id}>
                      <div
                        class="row-menu-popup"
                        data-testid="row-menu-popup"
                        onClick={(e) => e.stopPropagation()}
                      >
                        <button
                          type="button"
                          class="row-menu-item"
                          data-testid="row-menu-library"
                          onClick={() => setLibrary(row.session_id, !row.in_library)}
                        >
                          {row.in_library ? 'remove from library' : 'add to library'}
                        </button>
                      </div>
                    </Show>
                  </div>
                </div>
              </li>
            )}
          </For>
        </ul>
      </nav>
      <Show when={picker()}>
        <SessionPicker store={props.store} send={props.send} onClose={() => setPicker(false)} />
      </Show>
    </>
  );
}

function SessionPicker(props: { store: ClientStore; send: RailProps['send']; onClose: () => void }): JSX.Element {
  // The filter is debounced so typing does not request per key; a late
  // answer to text the user typed past is dropped by the store.
  let timer: number | null = null;
  const onFilter = (value: string): void => {
    props.store.set((s) => { s.pickerFilter = value; });
    if (timer !== null) window.clearTimeout(timer);
    timer = window.setTimeout(() => props.send({ type: 'picker_rows', filter: value }), 150);
  };
  const pick = (row: import('./protocol').PickerRow): void => {
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
