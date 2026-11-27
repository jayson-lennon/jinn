// An annotation entry: the citations an assistant turn made.

import { For, Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { EntryEnvelope } from '../protocol';
import { safeHref } from '../markdown';
import { body, citationTitle } from './shared';

export function AnnotationView(props: { envelope: EntryEnvelope }): JSX.Element {
  const citations = () =>
    (Array.isArray(body(props.envelope)) ? body(props.envelope) : []) as { url?: string; title?: string }[];
  return (
    <details class="entry-body sources">
      <summary>
        <span class="entry-role">sources</span> Sources ({citations().length})
      </summary>
      <For each={citations()}>
        {(citation) => (
          <div class="citation">
            {/* A citation's URL is model output: it obeys the same link rule as
                markdown, so a `javascript:` citation is never clickable and
                keeps its address as plain text. */}
            <Show when={safeHref(citation.url ?? null)} fallback={<span class="citation-title">{citationTitle(citation)}</span>}>
              {(href) => (
                <a class="citation-title" href={href()} target="_blank" rel="noopener noreferrer">
                  {citationTitle(citation)}
                </a>
              )}
            </Show>
            <span class="citation-url">{citation.url}</span>
          </div>
        )}
      </For>
    </details>
  );
}

/** A citation's name, or `(untitled)` when the model gave it none. */
