// An assistant entry: prose, rendered as markdown.

import type { JSX } from '@solidjs/web';
import type { EntryEnvelope, EntryKind } from '../protocol/protocol';
import { renderMarkdown } from '../markdown/markdown';
import { body, str } from './shared';

export function ProseView(props: { kind: EntryKind; envelope: EntryEnvelope }): JSX.Element {
  // Markdown needs a real element to hold its sanitized HTML. The entry id
  // rides along so each entry's footnote anchors are its own.
  const html = (): string => renderMarkdown(str(body(props.envelope)), props.envelope.entry.id);
  return (
    <div class="entry-body">
      <span class="entry-role">{props.kind}</span>
      <div class="entry-prose" innerHTML={html()} />
    </div>
  );
}

/**
 * A transient notice, such as the models-refresh summary, which arrives as a
 * markdown table: rendered as prose rather than plain text so its table and
 * links are real. It is never persisted, so it carries no footnote anchors
 * and needs no entry id for them.
 */
