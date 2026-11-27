// An actor entry: which subagent spoke, and nothing more.

import { Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import { body } from './shared';

export function ActorView(props: { value: unknown }): JSX.Element {
  const data = () => props.value as { source?: string; text?: string } | undefined;
  return (
    <div class="entry-body">
      <span class="entry-role">actor · {data()?.source}</span>
      <pre class="entry-text">{data()?.text}</pre>
    </div>
  );
}

