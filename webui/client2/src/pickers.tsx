// The tools, MCP servers, and persona modals.
//
// Like the skills modal, each of these is a backdrop, a filter input, and
// a list of rows the server built. This file holds the shell the three
// share and one component per picker. The rows live in the store and are
// the server's own: a component only renders them and asks for more, so
// a click's result is read back from the server rather than guessed here.

import { For, onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { McpBadge, McpRow, PersonaRow, TextRun, ToolRow } from './protocol';

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

/** A field's text, with the filter's matched runs highlighted. */
function Runs(props: { runs: readonly TextRun[] }): JSX.Element {
  return (
    <For each={props.runs}>
      {(run) => (run.matched ? <mark>{run.text}</mark> : <span>{run.text}</span>)}
    </For>
  );
}

export interface ToolsModalProps {
  readonly rows: readonly ToolRow[];
  readonly filter: string;
  readonly onFilter: (value: string) => void;
  /** Withholds or re-permits one tool, by name. The write lands at once. */
  readonly onToggle: (tool: string) => void;
  readonly onClose: () => void;
}

/** The tools modal: one row per tool the session's registry offers. */
export function ToolsModal(props: ToolsModalProps): JSX.Element {
  return (
    <FilterableModal
      label="Tools"
      testId="tools"
      placeholder="Filter tools…"
      filter={props.filter}
      onFilter={props.onFilter}
      onClose={props.onClose}
    >
      <ul class="modal-list picker-rows tools-rows" data-testid="tools-rows">
        <For each={props.rows}>
          {(row) => (
            <li>
              <button
                type="button"
                class="picker-row tool-row"
                data-testid="tool-row"
                data-tool={row.name}
                data-enabled={row.enabled ? 'true' : 'false'}
                aria-pressed={row.enabled ? 'true' : 'false'}
                aria-label={`${row.enabled ? 'Disable' : 'Enable'} ${row.name}`}
                onClick={() => props.onToggle(row.name)}
              >
                <span class="picker-marker" aria-hidden="true">
                  {row.enabled ? '\u2713' : '\u2717'}
                </span>
                <span class="picker-name">
                  <Runs runs={row.name_runs} />
                </span>
                <span class="picker-sep" aria-hidden="true">
                  -
                </span>
                <span class="picker-description">
                  <Runs runs={row.description_runs} />
                </span>
              </button>
            </li>
          )}
        </For>
      </ul>
    </FilterableModal>
  );
}

export interface McpModalProps {
  readonly rows: readonly McpRow[];
  readonly filter: string;
  readonly onFilter: (value: string) => void;
  /** Enables or disables one server for the session, by name. */
  readonly onToggle: (server: string) => void;
  readonly onClose: () => void;
}

/** The MCP servers modal: one row per `[mcp.<name>]` entry in `jinn.toml`. */
export function McpModal(props: McpModalProps): JSX.Element {
  return (
    <FilterableModal
      label="MCP servers"
      testId="mcp"
      placeholder="Filter MCP servers…"
      filter={props.filter}
      onFilter={props.onFilter}
      onClose={props.onClose}
    >
      <ul class="modal-list picker-rows mcp-rows" data-testid="mcp-rows">
        <For each={props.rows}>
          {(row) => (
            <li>
              <button
                type="button"
                class="picker-row mcp-row"
                data-testid="mcp-row"
                data-server={row.name}
                data-enabled={row.enabled ? 'true' : 'false'}
                data-badge={row.badge}
                aria-pressed={row.enabled ? 'true' : 'false'}
                aria-label={`${row.enabled ? 'Disable' : 'Enable'} MCP server ${row.name}`}
                onClick={() => props.onToggle(row.name)}
              >
                <span class="picker-marker" aria-hidden="true">
                  {row.enabled ? '\u25CF' : '\u25CB'}
                </span>
                <span class="picker-name">
                  <Runs runs={row.name_runs} />
                </span>
                <span class="picker-sep" aria-hidden="true">
                  -
                </span>
                <span class="picker-description">
                  <Runs runs={row.description_runs} />
                </span>
                {/* The badge always renders: a disabled server reads `off`,
                    which the server derives for it whatever its connection
                    last reported. */}
                <span class="mcp-badge" data-badge={row.badge}>
                  {row.badge}
                </span>
              </button>
            </li>
          )}
        </For>
      </ul>
    </FilterableModal>
  );
}

export interface PersonaModalProps {
  readonly rows: readonly PersonaRow[];
  readonly filter: string;
  readonly onFilter: (value: string) => void;
  /** Switches the session's persona, by name. */
  readonly onSwitch: (persona: string) => void;
  readonly onClose: () => void;
}

/** The persona modal: the catalog's personas, the session's own marked. */
export function PersonaModal(props: PersonaModalProps): JSX.Element {
  return (
    <FilterableModal
      label="Persona"
      testId="persona"
      placeholder="Filter personas…"
      filter={props.filter}
      onFilter={props.onFilter}
      onClose={props.onClose}
    >
      <ul class="modal-list picker-rows persona-rows" data-testid="persona-rows">
        <For each={props.rows}>
          {(row) => (
            <li>
              <button
                type="button"
                class="picker-row persona-row"
                data-testid="persona-row"
                data-persona={row.name}
                data-active={row.is_active ? 'true': 'false'}
                aria-current={row.is_active ? 'true' : undefined}
                onClick={() => props.onSwitch(row.name)}
              >
                <span class="picker-marker" aria-hidden="true">
                  {row.is_active ? '\u25CF' : '\u25CB'}
                </span>
                <span class="picker-name">
                  <Runs runs={row.name_runs} />
                </span>
                <span class="picker-sep" aria-hidden="true">-</span>
                <span class="picker-description">
                  <Runs runs={row.description_runs} />
                </span>
              </button>
            </li>
          )}
        </For>
      </ul>
    </FilterableModal>
  );
}
