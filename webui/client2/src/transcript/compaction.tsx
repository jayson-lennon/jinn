// A compaction entry: what was summarized, and what the summary says.

import { Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import { renderMarkdown } from '../markdown/markdown';
import { body, str } from './shared';

export function CompactionView(props: { value: unknown; entryId: string }): JSX.Element {
  const data = () =>
    props.value as {
      summary?: string;
      tokens_before?: number;
      tokens_after?: number;
      entries_compacted?: number;
      model_used?: string;
    } | undefined;
  return (
    <details class="entry-body compaction">
      <summary>
        <span class="entry-role">compaction</span> {data()?.entries_compacted} entries ·{' '}
        {data()?.tokens_before} → {data()?.tokens_after} tokens
      </summary>
      <div class="entry-prose" innerHTML={renderMarkdown(data()?.summary ?? '', props.entryId)} />
    </details>
  );
}

/**
 * The sources a web-search model cited: a disclosure collapsed to its count,
 * as in the terminal, so the "where did this come from" record is one line
 * until the reader asks for it. Each row names the source and shows its
 * address beside it in muted text.
 */
