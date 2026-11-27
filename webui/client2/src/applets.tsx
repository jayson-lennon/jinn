// The applets modal: the saved-app catalog as a list of new-tab links.
//
// Like the other pickers, a backdrop, a filter input, and rows the server
// built. The rows live in the store and are the server's own: this
// component only renders them. The one thing it adds over the other
// pickers is the status line — where the applets server is serving —
// because a row's being a link depends on it.

import { For, Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { AppletRow, TextRun } from './protocol/protocol';
import { FilterableModal } from './filterable_modal';
import { Runs } from './runs';

export interface AppletsModalProps {
  readonly rows: readonly AppletRow[];
  readonly filter: string;
  /** Whether the applets server is bound and serving. */
  readonly serving: boolean;
  /** The resolved bind while serving; empty while offline. */
  readonly bind: string;
  readonly onFilter: (value: string) => void;
  readonly onClose: () => void;
}

/**
 * The applets modal. Each row is one line: the title, with the filter's
 * matched runs marked. While the server is serving, the row is a link that
 * opens the applet in a new tab; offline (or for an id the gate refused),
 * it is a disabled line with no href.
 */
export function AppletsModal(props: AppletsModalProps): JSX.Element {
  return (
    <FilterableModal
      label="Applets"
      testId="applets"
      placeholder="Filter applets…"
      filter={props.filter}
      onFilter={props.onFilter}
      onClose={props.onClose}
    >
      <p
        class="applet-status"
        data-testid="applet-status"
        data-serving={props.serving ? 'true' : 'false'}
      >
        {props.serving ? `serving http://${props.bind}` : '<server offline>'}
      </p>
      <Show
        when={props.rows.length > 0}
        fallback={<div class="applet-empty" data-testid="applet-empty">No applets saved yet</div>}
      >
        <ul class="modal-list picker-rows applet-rows" data-testid="applet-rows">
          <For each={props.rows}>
            {(row) => (
              <li>
                <Show
                  when={row.href !== null}
                  fallback={
                    <span
                      class="picker-row applet-row applet-row-disabled"
                      data-testid="applet-row"
                      data-applet={row.id}
                      aria-disabled="true"
                      title="The applets server is offline"
                    >
                      <span class="picker-name">
                        <Runs runs={row.title_runs} />
                      </span>
                    </span>
                  }
                >
                  <a
                    class="picker-row applet-row"
                    data-testid="applet-row"
                    data-applet={row.id}
                    href={row.href ?? undefined}
                    target="_blank"
                    rel="noopener"
                  >
                    <span class="picker-name">
                      <Runs runs={row.title_runs} />
                    </span>
                  </a>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </FilterableModal>
  );
}

