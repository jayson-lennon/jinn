// A user entry: its text, and the attachments it carries.

import { For, Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { EntryEnvelope } from '../protocol/protocol';
import { renderMarkdown } from '../markdown';
import { ATTACHMENT_PREFIX, body } from './shared';

export function UserView(props: { envelope: EntryEnvelope; sessionId: string | null }): JSX.Element {
  const data = () =>
    body(props.envelope) as {
      display?: string;
      expanded?: string;
      attachments?: unknown[];
    };
  // The attachments already rode the socket inside the entry. The page
  // reads only how many there are; the bytes come from the attachment
  // route, because an `<img>` needs an address and a data URL built from
  // the socket's array would double the cost of every image.
  const attachments = (): unknown[] => {
    const list = data()?.attachments;
    return Array.isArray(list) ? list : [];
  };
  return (
    <div class="entry-body">
      <span class="entry-role">you</span>
      {/* A user's own text is rendered as markdown, the same as an
          assistant's: a pasted address becomes a link, a pasted table a
          table. It passes through the same two barriers as model output,
          because pasted text is no more trusted than generated text. */}
      <Show when={(data()?.display ?? '') !== ''}>
        <div class="entry-prose" innerHTML={renderMarkdown(data()?.display ?? '', props.envelope.entry.id)} />
      </Show>
      {/* The expanded prompt is the literal text sent to the model, kept as
          the user wrote it so `@path` tokens and whitespace read exactly. */}
      <Show when={(data()?.expanded ?? '') !== (data()?.display ?? '')}>
        <details>
          <summary>expanded</summary>
          <pre class="entry-text">{data()?.expanded}</pre>
        </details>
      </Show>
      {/* Rendered as plain elements beside the markdown pipeline, never
          through it: the sanitizer allows only http(s) image sources and
          would strip the route's relative address. */}
      <Show when={attachments().length > 0}>
        <div class="entry-attachments" data-testid="entry-attachments">
          <For each={attachments()}>
            {(_attachment, index) => (
              <img
                class="attachment"
                data-testid="attachment"
                src={`${ATTACHMENT_PREFIX}${props.sessionId ?? ''}/${props.envelope.entry.id}/${index()}`}
                alt="attached image"
                loading="lazy"
              />
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

