// The skills modal: the active session's discovered skills, with a filter.
//
// The rows live in the store and are the server's own — this component
// only renders them and asks for more. Every row shows whether its skill
// is enabled, so a click's result is read back from the server rather
// than guessed here.

import { For, Show, onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { SkillRow } from './protocol/protocol';
import { Runs } from './runs';

export interface SkillsModalProps {
  /** The rows, as the store holds them. */
  readonly rows: readonly SkillRow[];
  /** The filter, so the input shows what was typed. */
  readonly filter: string;
  /** Asks for rows matching the new filter text. */
  readonly onFilter: (value: string) => void;
  /** Toggles one skill's enablement, by name. */
  readonly onToggle: (skill: string) => void;
  /** Loads one skill's body into the session's context, by name. */
  readonly onLoad: (skill: string) => void;
  /** Closes the modal. */
  readonly onClose: () => void;
}

export function SkillsModal(props: SkillsModalProps): JSX.Element {
  // Escape closes the modal. The listener sits on the document for as
  // long as the modal is mounted: focus can be anywhere once the user
  // has clicked a row, and an Escape that reaches nothing would strand
  // the modal. It is removed on unmount, so a closed modal never
  // swallows the key another overlay wants.
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
      <div class="modal skills-modal" role="dialog" aria-label="Skills" data-testid="skills-modal">
        <input
          class="modal-filter"
          data-testid="skills-filter"
          autofocus
          placeholder="Filter skills…"
          value={props.filter}
          onInput={(e) => props.onFilter(e.currentTarget.value)}
        />
        <ul class="modal-list skills-rows" data-testid="skills-rows">
          <For each={props.rows}>
            {(row) => (
              <SkillRowView row={row} onToggle={props.onToggle} onLoad={props.onLoad} />
            )}
          </For>
        </ul>
      </div>
    </div>
  );
}

function SkillRowView(props: {
  row: SkillRow;
  onToggle: (skill: string) => void;
  onLoad: (skill: string) => void;
}): JSX.Element {
  const row = (): SkillRow => props.row;
  return (
    <li>
      <div
        class={`skill-row${row().enabled ? ' skill-row-enabled' : ''}${row().loaded ? ' skill-row-loaded' : ''}`}
        data-testid="skill-row"
        data-skill={row().name}
        data-enabled={row().enabled ? 'true' : 'false'}
        data-loaded={row().loaded ? 'true' : 'false'}
      >
        {/* The toggle target is its own button, disjoint from the load
            button beside it, so a click can never mean both. A loaded
            skill cannot be disabled — its body stays pinned in history —
            so the toggle is disabled for it. */}
        <button
          type="button"
          class="skill-toggle"
          data-testid="skill-toggle"
          aria-pressed={row().enabled ? 'true' : 'false'}
          aria-label={`${row().enabled ? 'Disable' : 'Enable'} ${row().name}`}
          disabled={row().loaded}
          onClick={() => props.onToggle(row().name)}
        >
          <span class="skill-row-marker" aria-hidden="true">
            {row().enabled ? '\u25CF' : '\u25CB'}
          </span>
          <span class="skill-row-name">
            <Runs runs={row().name_runs} />
          </span>
          <span class="skill-row-sep" aria-hidden="true">
            -
          </span>
          <span class="skill-row-description">
            <Runs runs={row().description_runs} />
          </span>
          <Show when={row().is_project}>
            <span class="skill-row-source">project</span>
          </Show>
        </button>
        <button
          type="button"
          class="skill-load"
          data-testid="skill-load"
          aria-label={`Load ${row().name} into context`}
          title="Load into context"
          onClick={() => props.onLoad(row().name)}
          disabled={row().loaded}
        >
          +
        </button>
      </div>
    </li>
  );
}

