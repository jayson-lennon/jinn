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

import { For, Show, createSignal, onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientMsg, EntryEnvelope, EntryKind } from './protocol';
import { renderMarkdown, safeHref } from './markdown';
import { isOptimistic } from './store';

/** The route prefix an attachment's `<img>` is served from. */
const ATTACHMENT_PREFIX = '/web2/attachment/';

/** What an entry's `⋮` needs to open its menu and act on the entry. */
export interface EntryMenuProps {
  /** The id of the entry whose menu is open, or null when none is. */
  openFor: () => string | null;
  /** Opens the named entry's menu, or closes it when it is already open. */
  toggle: (id: string) => void;
  /** Sends one command. */
  send: (msg: ClientMsg) => void;
}

/**
 * Registers the two listeners that close an open entry menu: Escape, and
 * any click outside one. The transcript owns them so the whole list shares
 * one open menu; the popup and its button stop their own clicks, so a
 * click that reaches the document is outside. Escape is stopped when it
 * closes a menu, so it does not also dismiss an overlay underneath.
 */
export function useEntryMenuDismiss(openFor: () => string | null, close: () => void): void {
  const onKeydown = (event: KeyboardEvent): void => {
    if (event.key !== 'Escape' || openFor() === null) return;
    close();
    event.stopPropagation();
  };
  document.addEventListener('keydown', onKeydown);
  const onClick = (): void => {
    if (openFor() !== null) close();
  };
  document.addEventListener('click', onClick);
  onCleanup(() => {
    document.removeEventListener('keydown', onKeydown);
    document.removeEventListener('click', onClick);
  });
}

/** A pinned entry carries a pin position; the server omits it otherwise. */
function isPinned(envelope: EntryEnvelope): boolean {
  return envelope.entry.pin_position != null;
}

/**
 * Turns a freshly opened popup upward when opening it downward would leave
 * the transcript's scroll box. The kebab is at the entry's bottom-right, so
 * for the last entry — the one most often acted on — a downward popup
 * would open into the composer and be cut off.
 */
function fitPopup(node: HTMLElement): void {
  const scroller = node.closest('.transcript');
  if (scroller === null) return;
  const limit = scroller.getBoundingClientRect().bottom;
  if (node.getBoundingClientRect().bottom > limit) node.classList.add('up');
}

export function TranscriptEntry(props: { envelope: EntryEnvelope; sessionId: string | null; menu: EntryMenuProps }): JSX.Element {
  const kind = (): EntryKind => props.envelope.kind;
  const id = (): string => props.envelope.entry.id;
  const pinned = (): boolean => isPinned(props.envelope);
  const open = (): boolean => props.menu.openFor() === id();
  // A just-sent message is shown before the server names it; its id is the
  // page's own, so it can carry no menu until the real entry replaces it.
  const real = (): boolean => !isOptimistic(id());
  return (
    <article
      class={`entry entry-${kind()}${pinned() ? ' pinned' : ''}`}
      data-entry-id={id()}
    >
      <span class="pin-mark" aria-hidden="true">📌</span>
      <Switch kind={kind()} envelope={props.envelope} sessionId={props.sessionId} />
      <Show when={real()}>
        <span class="entry-menu">
          <button
            type="button"
            class="entry-menu-btn"
            data-testid="entry-menu"
            aria-label="Entry actions"
            aria-haspopup="true"
            aria-expanded={open() ? 'true' : 'false'}
            onClick={(e) => {
              // A tool row's summary is a disclosure toggle one sibling
              // over; neither default nor bubble may reach it.
              e.preventDefault();
              e.stopPropagation();
              props.menu.toggle(id());
            }}
          >
            ⋮
          </button>
          <Show when={open()}>
            <div
              class="entry-menu-popup"
              role="menu"
              data-testid="entry-menu-popup"
              ref={fitPopup}
              onClick={(e) => e.stopPropagation()}
            >
              <button
                type="button"
                class="entry-menu-item"
                role="menuitem"
                data-testid="entry-menu-pin"
                onClick={() => {
                  props.menu.send({ type: 'toggle_pin', entry_id: id() });
                  props.menu.toggle(id());
                }}
              >
                {pinned() ? 'Unpin' : 'Pin'}
              </button>
            </div>
          </Show>
        </span>
      </Show>
    </article>
  );
}

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

/** One arm per kind. There is no fallback: an unknown kind renders nothing. */
function Switch(props: { kind: EntryKind; envelope: EntryEnvelope; sessionId: string | null }): JSX.Element {
  switch (props.kind) {
    case 'user':
      return <UserView envelope={props.envelope} sessionId={props.sessionId} />;
    case 'system':
    case 'error':
    case 'thinking':
      return <NoticeView kind={props.kind} text={str(body(props.envelope))} />;
    case 'transient':
      return <TransientView envelope={props.envelope} />;
    case 'assistant':
      return <ProseView kind={props.kind} envelope={props.envelope} />;
    case 'actor':
      return <ActorView value={body(props.envelope)} />;
    case 'tool_call':
      return <ToolCallView value={body(props.envelope)} />;
    case 'tool_result':
      return <ToolResultView value={body(props.envelope)} />;
    case 'compaction':
      return <CompactionView value={body(props.envelope)} entryId={props.envelope.entry.id} />;
    case 'annotation':
      return <AnnotationView envelope={props.envelope} />;
    case 'rule_interrupt':
      return <RuleInterruptView value={body(props.envelope)} entryId={props.envelope.entry.id} />;
    case 'intersession':
      return <IntersessionView value={body(props.envelope)} entryId={props.envelope.entry.id} />;
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

function NoticeView(props: { kind: EntryKind; text: string }): JSX.Element {
  return (
    <div class="entry-body">
      <span class="entry-role">{props.kind}</span>
      <pre class="entry-text">{props.text}</pre>
    </div>
  );
}

function ProseView(props: { kind: EntryKind; envelope: EntryEnvelope }): JSX.Element {
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
function TransientView(props: { envelope: EntryEnvelope }): JSX.Element {
  const html = (): string => renderMarkdown(str(body(props.envelope)));
  return (
    <div class="entry-body">
      <span class="entry-role">transient</span>
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

function CompactionView(props: { value: unknown; entryId: string }): JSX.Element {
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
function AnnotationView(props: { envelope: EntryEnvelope }): JSX.Element {
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
function citationTitle(citation: { url?: string; title?: string }): string {
  return citation.title || '(untitled)';
}

function RuleInterruptView(props: { value: unknown; entryId: string }): JSX.Element {
  const data = () => props.value as { rule?: string; body?: string } | undefined;
  return (
    <div class="entry-body">
      <span class="entry-role">rule · {data()?.rule}</span>
      <div class="entry-prose" innerHTML={renderMarkdown(data()?.body ?? '', props.entryId)} />
    </div>
  );
}

function IntersessionView(props: { value: unknown; entryId: string }): JSX.Element {
  const data = () =>
    props.value as { source_name?: string | null; source_session?: string; text?: string } | undefined;
  return (
    <div class="entry-body">
      <span class="entry-role">from {data()?.source_name ?? data()?.source_session ?? 'session'}</span>
      <div class="entry-prose" innerHTML={renderMarkdown(data()?.text ?? '', props.entryId)} />
    </div>
  );
}
