// A tool result: its output, and the full output behind a disclosure.


/** Full tool outputs already fetched, by entry id. An output never changes
 * once its entry lands, and re-opening a disclosure must be instant. */
const toolOutputs = new Map<string, string>();

import { createSignal, Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientMsg } from '../protocol/protocol';
import { TOOL_OUTPUT_PREFIX, body } from './shared';

export function ToolResultView(props: { value: unknown; sessionId: string | null; entryId: string }): JSX.Element {
  const data = () =>
    props.value as {
      name?: string;
      content?: string;
      status?: string;
      truncation?: unknown;
    } | undefined;
  const status = (): string => (data()?.status ?? 'Pending').toLowerCase();
  // The truncation meta is the marker: a result that has one has a full
  // output to fetch, and the bytes stay off the socket until asked for.
  const hasFullOutput = (): boolean => data()?.truncation != null;
  const [fullOutput, setFullOutput] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);

  const loadFullOutput = async (): Promise<void> => {
    const cached = toolOutputs.get(props.entryId);
    if (cached !== undefined) {
      setFullOutput(cached);
      return;
    }
    if (props.sessionId === null) return;
    setLoading(true);
    try {
      const response = await fetch(`${TOOL_OUTPUT_PREFIX}${props.sessionId}/${props.entryId}`);
      const text = response.ok ? await response.text() : null;
      if (text !== null) toolOutputs.set(props.entryId, text);
      setFullOutput(text);
    } catch {
      setFullOutput(null);
    } finally {
      setLoading(false);
    }
  };

  return (
    <details class={`entry-body tool result-${status()}`}>
      <summary>
        <span class="entry-role">tool result</span> <code>{data()?.name}</code>
        <span class="entry-status">{status()}</span>
      </summary>
      <pre class="entry-text">{data()?.content}</pre>
      <Show when={hasFullOutput()}>
        <details
          class="tool-output"
          data-testid="tool-output"
          onToggle={(event) => {
            if (event.currentTarget.open && fullOutput() === null && !loading()) void loadFullOutput();
          }}
        >
          <summary>full output</summary>
          <Show
            when={!loading()}
            fallback={
              <span class="tool-output-spinner" data-testid="tool-output-spinner">
                <span class="spinner spinner-small" aria-hidden="true"><i /></span> loading full output…
              </span>
            }
          >
            <Show when={fullOutput() !== null} fallback={<p class="tool-output-missing">full output unavailable</p>}>
              <pre class="entry-text">{fullOutput()}</pre>
            </Show>
          </Show>
        </details>
      </Show>
    </details>
  );
}

