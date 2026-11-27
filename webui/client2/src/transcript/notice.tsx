// A system, error, or thinking entry: plain text in the note style.

import type { JSX } from '@solidjs/web';
import type { EntryKind } from '../protocol';

export function NoticeView(props: { kind: EntryKind; text: string }): JSX.Element {
  return (
    <div class="entry-body">
      <span class="entry-role">{props.kind}</span>
      <pre class="entry-text">{props.text}</pre>
    </div>
  );
}

