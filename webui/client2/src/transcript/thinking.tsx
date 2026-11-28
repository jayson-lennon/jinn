// A thinking entry: one collapsed header line that opens the full text.

import { Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { EntryEnvelope } from '../protocol/protocol';
import { body, str } from './shared';

/**
 * A thinking entry, collapsed to one header line until the reader opens it.
 *
 * The text still streams into the store while collapsed, so the body is
 * whole the moment the header is clicked; only its rendering is withheld.
 * The text is derived from the envelope on each render rather than handed
 * over as a string: a plain value is read once when the entry first mounts
 * and never again, which froze the body at its first chunk.
 */
export function ThinkingView(props: {
  envelope: EntryEnvelope;
  open: () => boolean;
  toggle: () => void;
}): JSX.Element {
  const text = (): string => str(body(props.envelope));
  return (
    <div class="entry-body thinking" data-open={props.open() ? 'true' : 'false'}>
      <button
        type="button"
        class="entry-role thinking-header"
        data-testid="thinking-header"
        aria-expanded={props.open() ? 'true' : 'false'}
        onClick={props.toggle}
      >
        <span class="thinking-chevron" aria-hidden="true">
          {props.open() ? '▾' : '▸'}
        </span>
        Thinking
      </button>
      <Show when={props.open()}>
        <pre class="entry-text entry-prose" data-testid="thinking-body">
          {text()}
        </pre>
      </Show>
    </div>
  );
}
