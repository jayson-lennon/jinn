// A tool call: its name and its arguments, pretty-printed when they parse.

import { Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import { body, opt, prettyArgs, str } from './shared';

export function ToolCallView(props: { value: unknown }): JSX.Element {
  const data = () =>
    props.value as { name?: string; arguments?: string; child_session?: string } | undefined;
  return (
    <details class="entry-body tool">
      <summary>
        <span class="entry-role">tool call</span> <code>{data()?.name}</code>
      </summary>
      <pre class="entry-text">{prettyArgs(data()?.arguments ?? '')}</pre>
      <Show when={opt(data()?.child_session)}>
        {(child) => <span class="entry-note">child session {child()}</span>}
      </Show>
    </details>
  );
}

