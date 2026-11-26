// The skills modal: the active session's discovered skills, with a filter.
//
// The rows live in the store and are the server's own — this component
// only renders them and asks for more. Every row shows whether its skill
// is enabled, so a click's result is read back from the server rather
// than guessed here.

import { For, Show, onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { SkillRow } from './protocol';

export interface SkillsModalProps {
  /** The rows, as the store holds them. */
  readonly rows: readonly SkillRow[];
  /** The filter, so the input shows what was typed. */
  readonly filter: string;
  /** Asks for rows matching the new filter text. */
  readonly onFilter: (value: string) => void;
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
          <For each={props.rows}>{(row) => <SkillRowView row={row} />}</For>
        </ul>
      </div>
    </div>
  );
}

function SkillRowView(props: { row: SkillRow }): JSX.Element {
  const row = (): SkillRow => props.row;
  return (
    <li>
      <div
        class={`skill-row${row().enabled ? ' skill-row-enabled' : ''}`}
        data-testid="skill-row"
        data-skill={row().name}
        data-enabled={row().enabled ? 'true' : 'false'}
        data-loaded={row().loaded ? 'true' : 'false'}
      >
        <span class="skill-row-marker" aria-hidden="true">
          {row().enabled ? '\u25CF' : '\u25CB'}
        </span>
        <span class="skill-row-name">
          <Runs runs={row().name_runs} />
        </span>
        <span class="skill-row-description">
          <Runs runs={row().description_runs} />
        </span>
        <Show when={row().is_project}>
          <span class="skill-row-source">project</span>
        </Show>
      </div>
    </li>
  );
}

/** A field's text, with the filter's matched runs highlighted. */
function Runs(props: { runs: readonly import('./protocol').TextRun[] }): JSX.Element {
  return (
    <For each={props.runs}>
      {(run) => (run.matched ? <mark>{run.text}</mark> : <span>{run.text}</span>)}
    </For>
  );
}
