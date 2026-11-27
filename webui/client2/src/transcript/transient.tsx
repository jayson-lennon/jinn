// A transient entry: server-side state that is not persisted with the session.

import { Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { EntryEnvelope } from '../protocol/protocol';
import { renderMarkdown } from '../markdown/markdown';
import { body, str } from './shared';

export function TransientView(props: { envelope: EntryEnvelope }): JSX.Element {
  const html = (): string => renderMarkdown(str(body(props.envelope)));
  return (
    <div class="entry-body">
      <span class="entry-role">transient</span>
      <div class="entry-prose" innerHTML={html()} />
    </div>
  );
}

