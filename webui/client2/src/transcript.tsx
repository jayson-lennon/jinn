// The transcript: one entry of any of the thirteen kinds, rendered.
//
// The kind to render is the envelope's own name — not the persisted map's
// tag — because that is the one field that is exact for every kind (a
// transient's persisted map reads back as a system entry). The map then
// supplies the fields, under the keys the session store has always written.
//
// Text a model or a tool produced goes through the markdown pipeline; text
// that is a name, an id, or an argument is escaped and shown verbatim — it
// is data, not prose.

import { For, Show } from '@solidjs/web';
import type { JSX } from '@solidjs/web';
import type { EntryEnvelope, EntryKind } from './protocol';
import { renderMarkdown, safeHref } from './markdown';

/** The route prefix an attachment's `<img>` is served from. */
const ATTACHMENT_PREFIX = '/web2/attachment/';

/** The persisted map's single value, whatever its tag. */
function body(envelope: EntryEnvelope): unknown {
  const inner = envelope.entry.kind as Record<string, unknown>;
  const tag = Object.keys(inner)[0];
  return tag === undefined ? {} : inner[tag];
}

function str(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function opt(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

/** A tool call's arguments, pretty-printed when they are valid JSON. */
function prettyArgs(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed === '') return '';
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2);
  } catch {
    return raw;
  }
}

export function TranscriptEntry(props: { envelope: EntryEnvelope; sessionId: string | null }): JSX.Element {
  const kind = (): EntryKind => props.envelope.kind;
  return (
    <article class={`entry entry-${kind()}`} data-entry-id={props.envelope.entry.id}>
      <Switch kind={kind()} envelope={props.envelope} sessionId={props.sessionId} />
    </article>
  );
}

/** One arm per kind. There is no fallback: an unknown kind renders nothing. */
function Switch(props: { kind: EntryKind; envelope: EntryEnvelope; sessionId: string | null }): JSX.Element {
  switch (props.kind) {
    case 'user':
      return <UserView envelope={props.envelope} sessionId={props.sessionId} />;
    case 'system':
    case 'error':
    case 'transient':
      return <NoticeView kind={props.kind} text={str(body(props.envelope))} />;
    case 'assistant':
    case 'thinking':
      return <ProseView kind={props.kind} text={str(body(props.envelope))} />;
    case 'actor':
      return <ActorView value={body(props.envelope)} />;
    case 'tool_call':
      return <ToolCallView value={body(props.envelope)} />;
    case 'tool_result':
      return <ToolResultView value={body(props.envelope)} />;
    case 'compaction':
      return <CompactionView value={body(props.envelope)} />;
    case 'annotation':
      return <AnnotationView value={body(props.envelope)} />;
    case 'rule_interrupt':
      return <RuleInterruptView value={body(props.envelope)} />;
    case 'intersession':
      return <IntersessionView value={body(props.envelope)} />;
  }
}

function UserView(props: { envelope: EntryEnvelope; sessionId: string | null }): JSX.Element {
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
      <Show when={(data()?.display ?? '') !== ''}>
        <pre class="entry-text">{data()?.display}</pre>
      </Show>
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

function NoticeView(props: { kind: EntryKind; text: string }): JSX.Element {
  return (
    <div class="entry-body">
      <span class="entry-role">{props.kind}</span>
      <pre class="entry-text">{props.text}</pre>
    </div>
  );
}

function ProseView(props: { kind: EntryKind; text: string }): JSX.Element {
  // Markdown needs a real element to hold its sanitized HTML.
  const html = (): string => renderMarkdown(props.text);
  return (
    <div class="entry-body">
      <span class="entry-role">{props.kind === 'thinking' ? 'thinking' : 'assistant'}</span>
      <div class="entry-prose" innerHTML={html()} />
    </div>
  );
}

function ActorView(props: { value: unknown }): JSX.Element {
  const data = () => props.value as { source?: string; text?: string } | undefined;
  return (
    <div class="entry-body">
      <span class="entry-role">actor · {data()?.source}</span>
      <pre class="entry-text">{data()?.text}</pre>
    </div>
  );
}

function ToolCallView(props: { value: unknown }): JSX.Element {
  const data = () =>
    props.value as { name?: string; arguments?: string; child_session?: string } | undefined;
  return (
    <details class="entry-body tool">
      <summary>
        <span class="entry-role">tool call</span> <code>{data()?.name}</code>
      </summary>
      <pre class="entry-text">{prettyArgs(data()?.arguments ?? '')}</pre>
      <Show when={opt(data()?.child_session)}>
        {(child) => <span class="entry-note">child session {child()}</span>}
      </Show>
    </details>
  );
}

function ToolResultView(props: { value: unknown }): JSX.Element {
  const data = () =>
    props.value as {
      name?: string;
      content?: string;
      status?: string;
      full_content?: string | null;
    } | undefined;
  const status = (): string => (data()?.status ?? 'Pending').toLowerCase();
  return (
    <details class={`entry-body tool result-${status()}`}>
      <summary>
        <span class="entry-role">tool result</span> <code>{data()?.name}</code>
        <span class="entry-status">{status()}</span>
      </summary>
      <pre class="entry-text">{data()?.content}</pre>
      <Show when={str(data()?.full_content) !== ''}>
        <details>
          <summary>full output</summary>
          <pre class="entry-text">{data()?.full_content}</pre>
        </details>
      </Show>
    </details>
  );
}

function CompactionView(props: { value: unknown }): JSX.Element {
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
      <div class="entry-prose" innerHTML={renderMarkdown(data()?.summary ?? '')} />
    </details>
  );
}

function AnnotationView(props: { value: unknown }): JSX.Element {
  const citations = () =>
    (Array.isArray(props.value) ? props.value : []) as { url?: string; title?: string }[];
  return (
    <ul class="entry-body citations">
      <For each={citations()}>
        {(citation) => (
          <li>
            {/* A citation's URL is model output: it obeys the same link rule as
                markdown, so a `javascript:` citation is never clickable. */}
            <Show when={safeHref(citation.url ?? null)} fallback={<span>{citation.title || citation.url}</span>}>
              {(href) => (
                <a href={href()} target="_blank" rel="noopener noreferrer">
                  {citation.title || citation.url}
                </a>
              )}
            </Show>
          </li>
        )}
      </For>
    </ul>
  );
}

function RuleInterruptView(props: { value: unknown }): JSX.Element {
  const data = () => props.value as { rule?: string; body?: string } | undefined;
  return (
    <div class="entry-body">
      <span class="entry-role">rule · {data()?.rule}</span>
      <div class="entry-prose" innerHTML={renderMarkdown(data()?.body ?? '')} />
    </div>
  );
}

function IntersessionView(props: { value: unknown }): JSX.Element {
  const data = () =>
    props.value as { source_name?: string | null; source_session?: string; text?: string } | undefined;
  return (
    <div class="entry-body">
      <span class="entry-role">from {data()?.source_name ?? data()?.source_session ?? 'session'}</span>
      <div class="entry-prose" innerHTML={renderMarkdown(data()?.text ?? '')} />
    </div>
  );
}
