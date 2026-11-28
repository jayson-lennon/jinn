// A thinking entry: one collapsed header line that opens the full text.

import { Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import { isThinkingOpen, toggleThinking } from './thinking_state';

/**
 * A thinking entry, collapsed to one header line until the reader opens it.
 *
 * The text still streams into the store while collapsed, so the body is
 * whole the moment the header is clicked; only its rendering is withheld.
 */
export function ThinkingView(props: {
  id: string;
  text: string;
  open: () => boolean;
  toggle: () => void;
}): JSX.Element {
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
          {props.text}
        </pre>
      </Show>
    </div>
  );
}
