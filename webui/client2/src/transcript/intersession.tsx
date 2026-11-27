// An inter-session entry: a note left by another session, carried in here.

import { Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import { renderMarkdown } from '../markdown/markdown';
import { body, str } from './shared';

export function IntersessionView(props: { value: unknown; entryId: string }): JSX.Element {
  const data = () =>
    props.value as { source_name?: string | null; source_session?: string; text?: string } | undefined;
  return (
    <div class="entry-body">
      <span class="entry-role">from {data()?.source_name ?? data()?.source_session ?? 'session'}</span>
      <div class="entry-prose" innerHTML={renderMarkdown(data()?.text ?? '', props.entryId)} />
    </div>
  );
}
