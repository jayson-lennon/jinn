// A rule interrupt: the text, and the rule that stopped the turn.

import { Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import { renderMarkdown } from '../markdown/markdown';
import { body, parseInterruptBody, str } from './shared';

export function RuleInterruptView(props: { value: unknown; entryId: string }): JSX.Element {
  const data = () => props.value as { rule?: string; body?: string } | undefined;
  const parsed = () => parseInterruptBody(data()?.body ?? '');
  return (
    <details class="entry-body rule-interrupt">
      <summary>
        <span class="interrupt-rule">⚡ Rule fired: {data()?.rule}</span>
        <Show when={parsed().reason !== null}>
          <span class="interrupt-reason"> · {parsed().reason}</span>
        </Show>
      </summary>
      <div class="entry-prose" innerHTML={renderMarkdown(parsed().text, props.entryId)} />
    </details>
  );
}

