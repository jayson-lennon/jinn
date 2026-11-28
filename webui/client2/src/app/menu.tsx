// The hamburger menu: a button in the title bar, a full-height panel that
// slides in from the left, and the scrim it sits over.
//
// The open flag is the component's own — whether a menu is open is view
// state nobody else reads, and it is not worth a store field. The items
// are a property, so the page decides what the panel carries; an item
// whose feature is not on this page is simply not passed, which is why
// the panel never shows a control that leads nowhere.
//
// The scrim and the panel stay mounted and are moved by the open flag:
// mounting on open would skip the slide, because the panel has to start
// off-canvas for the transition to have a from.
//
// The button only opens. While the menu is open the scrim covers it, so
// it cannot be clicked to close — and it does not need to be: the scrim,
// Escape, and picking an item each close the menu.

import { For, createSignal, onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';

/** One menu item: what it says, and what it does when picked. */
export interface MenuItem {
  /** A stable id, so a test can find one item without its label. */
  readonly id: string;
  /** The item's label. */
  readonly label: string;
  /** Runs when the item is picked. The menu closes first. */
  readonly run: () => void;
}

/** One option of a segmented group: what it says and whether it is current. */
export interface MenuGroupOption {
  /** A stable id, sent back when picked. */
  readonly id: string;
  /** The option's label. */
  readonly label: string;
  /** Whether this option is the current one. */
  readonly active: boolean;
}

/** A segmented control at the end of the panel — the color-mode group. */
export interface MenuGroup {
  /** A stable id for the group's test id. */
  readonly id: string;
  /** The group's accessible name. */
  readonly label: string;
  /** The test id stem for the group. */
  readonly testId: string;
  /** The options, in order. */
  readonly options: readonly MenuGroupOption[];
  /** Runs with the picked option's id. The menu stays open. */
  readonly pick: (id: string) => void;
}

export interface MenuProps {
  /** The items the panel lists, in order. An empty list renders no item. */
  readonly items: readonly MenuItem[];
  /** The groups after the items, in order. */
  readonly groups?: readonly MenuGroup[];
}

export function Menu(props: MenuProps): JSX.Element {
  const [open, setOpen] = createSignal(false);
  const close = (): void => {
    setOpen(false);
  };

  const pick = (item: MenuItem): void => {
    close();
    item.run();
  };

  // Escape closes the menu while it is open. The listener sits on the
  // document for the component's whole life and is guarded by the open
  // flag: focus can be anywhere when the menu is open (the scrim and the
  // panel hold no focus of their own), and a closed menu must never
  // swallow the key another overlay wants.
  const onKeydown = (event: KeyboardEvent): void => {
    if (event.key === 'Escape' && open()) {
      close();
    }
  };
  document.addEventListener('keydown', onKeydown);
  onCleanup(() => document.removeEventListener('keydown', onKeydown));

  return (
    <div class="menu">
      <button
        type="button"
        class="menu-button"
        data-testid="menu-button"
        aria-haspopup="true"
        aria-expanded={open() ? 'true' : 'false'}
        aria-controls="menu-panel"
        aria-label="Menu"
        onClick={() => {
          setOpen(true);
        }}
      >
        &#x2630;
      </button>
      <div
        class={`menu-scrim${open() ? ' open' : ''}`}
        data-testid="menu-scrim"
        onClick={close}
      />
      <div
        class={`menu-panel${open() ? ' open' : ''}`}
        id="menu-panel"
        role="menu"
        aria-label="Menu"
        data-testid="menu-panel"
      >
        <For each={props.items}>
          {(item) => (
            <button
              type="button"
              class="menu-item"
              role="menuitem"
              data-testid={`menu-item-${item.id}`}
              onClick={() => pick(item)}
            >
              {item.label}
            </button>
          )}
        </For>
        <For each={props.groups}>
          {(group) => (
            <div
              class="menu-group"
              role="group"
              aria-label={group.label}
              data-testid={group.testId}
            >
              {/* The pill that holds the options: it owns the border and
                  radius, the segments inside stay square. */}
              <div class="menu-group-options">
                <For each={group.options}>
                  {(option) => (
                    <button
                      type="button"
                      class={`menu-group-option${option.active ? ' active' : ''}`}
                      data-testid={`${group.testId}-${option.id}`}
                      aria-pressed={option.active ? 'true' : 'false'}
                      onClick={() => group.pick(option.id)}
                    >
                      {option.label}
                    </button>
                  )}
                </For>
              </div>
            </div>
          )}
        </For>
      </div>
    </div>
  );
}
