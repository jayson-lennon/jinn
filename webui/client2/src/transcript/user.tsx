// A user entry: its text, and the attachments it carries.

import { For, Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { EntryEnvelope } from '../protocol/protocol';
import { renderMarkdown } from '../markdown/markdown';
import { expandedDiffers, splitCanvasMetrics, stripCanvasMetrics } from './raw_blocks';
import { ATTACHMENT_PREFIX, body } from './shared';

export function UserView(props: { envelope: EntryEnvelope; sessionId: string | null }): JSX.Element {
  const data = () =>
    body(props.envelope) as {
      display?: string;
      expanded?: string;
      attachments?: unknown[];
    };
  const display = (): string => data()?.display ?? '';
  const expanded = (): string => data()?.expanded ?? '';
  // The canvas-metrics block the enqueue path prepends to a message after
  // canvas activity is jinn's own bookkeeping, not the user's words. It is
  // split off the display text and shown as a collapsed disclosure, the
  // terminal's quiet formatter in the page's own disclosure shape, so the
  // tags never reach the prose and the message reads as what the user typed.
  const metrics = () => splitCanvasMetrics(display());
  const prose = (): string => metrics()?.rest ?? display();
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
      <Show when={metrics() !== null}>
        <details class="canvas-metrics" data-testid="canvas-metrics">
          <summary>
            canvas metrics · {metrics()!.events} events
            <Show when={metrics()!.artifact !== ''}> · {metrics()!.artifact}</Show>
          </summary>
          <pre class="entry-text" data-testid="canvas-metrics-body">{metrics()!.lines.join('\n')}</pre>
        </details>
      </Show>
      {/* A user's own text is rendered as markdown, the same as an
          assistant's: a pasted address becomes a link, a pasted table a
          table. It passes through the same two barriers as model output,
          because pasted text is no more trusted than generated text. */}
      <Show when={prose() !== ''}>
        <div class="entry-prose" innerHTML={renderMarkdown(prose(), props.envelope.entry.id)} />
      </Show>
      {/* The expanded prompt is the literal text sent to the model, kept as
          the user wrote it so `@path` tokens and whitespace read exactly. It
          is offered only when a real expansion differs from the display
          text: the metrics block is written into both, and a difference
          that is only the block would repeat it here. */}
      <Show when={expandedDiffers(display(), expanded())}>
        <details>
          <summary>expanded</summary>
          <pre class="entry-text">{stripCanvasMetrics(expanded())}</pre>
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
