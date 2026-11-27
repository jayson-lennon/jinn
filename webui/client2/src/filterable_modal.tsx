// The shell the filterable modals share: a backdrop, a filter input, and a slot.

// The backdrop closes on click, the input refocuses on every filter change,
// and Escape closes. The rows a modal lists are its own; this file holds only
// what every one of them draws around the list.

import { onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';

export interface FilterableModalProps {
  /** The modal's accessible name. */
  readonly label: string;
  /** The test id stem, shared by the modal, its filter, and its rows. */
  readonly testId: string;
  /** The filter input's placeholder. */
  readonly placeholder: string;
  /** The filter, so the input shows what was typed. */
  readonly filter: string;
  /** Asks for rows matching the new filter text. */
  readonly onFilter: (value: string) => void;
  /** Closes the modal. */
  readonly onClose: () => void;
  readonly children: JSX.Element;
}

/**
 * The shell the tools, MCP servers, and persona modals share: a backdrop,
 * a card, a filter input on top, and the caller's list below.
 *
 * Escape closes the modal. The listener sits on the document for as long
 * as the modal is mounted: focus can be anywhere once the user has
 * clicked a row, and an Escape that reaches nothing would strand the
 * modal. It is removed on unmount, so a closed modal never swallows the
 * key another overlay wants.
 */
export function FilterableModal(props: FilterableModalProps): JSX.Element {
  const onKeydown = (event: KeyboardEvent): void => {
    if (event.key === 'Escape') {
      event.stopPropagation();
      props.onClose();
    }
  };
  document.addEventListener('keydown', onKeydown);
  onCleanup(() => document.removeEventListener('keydown', onKeydown));

  return (
    <div
      class="modal-backdrop"
      onClick={(e) => {
        if (e.target === e.currentTarget) props.onClose();
      }}
    >
      <div class="modal picker-modal" role="dialog" aria-label={props.label} data-testid={`${props.testId}-modal`}>
        <input
          class="modal-filter"
          data-testid={`${props.testId}-filter`}
          autofocus
          placeholder={props.placeholder}
          value={props.filter}
          onInput={(e) => props.onFilter(e.currentTarget.value)}
        />
        {props.children}
      </div>
    </div>
  );
}
