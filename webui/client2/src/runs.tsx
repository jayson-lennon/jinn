// The matched-runs highlighter: a field's text with the filter's matched runs marked.

// Every filterable list renders its rows through this one component, so a
// match looks the same in the rail's session picker, the skills, tools,
// MCP, persona, and applets modals, and the model picker.

import { For } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { TextRun } from './protocol/protocol';

/** A field's text, with the filter's matched runs highlighted. */
export function Runs(props: { runs: readonly TextRun[] }): JSX.Element {
  return (
    <For each={props.runs}>
      {(run) => (run.matched ? <mark>{run.text}</mark> : <span>{run.text}</span>)}
    </For>
  );
}

