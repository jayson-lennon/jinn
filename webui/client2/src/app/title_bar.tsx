// The title bar: the hamburger, the session's title (renamed in place),
// the model chip, and — at phone widths only — the `#` that slides in the
// session drawer.
//
// The title is written synchronously on the server and no bus event carries
// it back, so the bar commits a rename by sending it and the sidebar row
// follows from the one message. The chip opens a two-row chooser — the
// OpenRouter model market, or the provider picker (`pickers/model_picker.tsx`).

import { createSignal, Show, Switch, Match } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientStore } from '../store/store';
import { Menu, type MenuGroup, type MenuItem } from './menu';
import { ModelPicker, type Send } from '../pickers/model_picker';
import { ModelChooser } from '../pickers/model_chooser';
import { unreadCount } from '../rail/unread';

// ---------------------------------------------------------------------------
// Title bar: rename in place, model chip
// ---------------------------------------------------------------------------

export function TitleBar(props: {
  store: ClientStore;
  send: Send;
  menuItems: readonly MenuItem[];
  menuGroups: readonly MenuGroup[];
  /** Opens the OpenRouter model market. */
  onOpenMarket: () => void;
  /** Slides in the phone's session drawer. */
  openDrawer: () => void;
  /** Whether that drawer is slid in — what the `#` button reports. */
  drawerOpen: () => boolean;
}): JSX.Element {
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal('');
  const [picker, setPicker] = createSignal(false);
  const [chooser, setChooser] = createSignal(false);

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
          off-canvas drawer and this is what opens it. Marked like an unread
          row: a gold fill and the count of sessions that settled
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
      <button class="model-chip" data-testid="model-chip" onClick={() => setChooser(true)}>
        {props.store.state.modelLabel || 'no model'}
      </button>
      <Show when={chooser()}>
        <ModelChooser
          onMarket={props.onOpenMarket}
          onProvider={openModels}
          onClose={() => setChooser(false)}
        />
      </Show>
      <Show when={picker()}>
        <ModelPicker store={props.store} send={props.send} onClose={() => setPicker(false)} />
      </Show>
    </header>
  );
}

