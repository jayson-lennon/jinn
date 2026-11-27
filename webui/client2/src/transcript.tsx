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
import { ActorView } from './transcript/actor';
import { AnnotationView } from './transcript/annotation';
import { CompactionView } from './transcript/compaction';
import { IntersessionView } from './transcript/intersession';
import { NoticeView } from './transcript/notice';
import { ProseView } from './transcript/prose';
import { RuleInterruptView } from './transcript/rule_interrupt';
import { ToolCallView } from './transcript/tool_call';
import { ToolResultView } from './transcript/tool_result';
import { TransientView } from './transcript/transient';
import { UserView } from './transcript/user';
import { body, str } from './transcript/shared';
import { isOptimistic } from './transcript_entries';

/** The route prefix an attachment's `<img>` is served from. */
/** What an entry's `⋮` needs to open its menu and act on the entry. */
export interface EntryMenuProps {
  /** The id of the entry whose menu is open, or null when none is. */
  openFor: () => string | null;
  /** Opens the named entry's menu, or closes it when it is already open. */
  toggle: (id: string) => void;
  /** Sends one command. */
  send: (msg: ClientMsg) => void;
  /** Raises the loading overlay for a fork of the session `source`. */
  beginFork: (source: string | null) => void;
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

/** Whether the entry is in the model's context, as the server reports it.
 * A frame that predates the field, or a just-sent optimistic entry, carries
 * none and reads as in context — nothing is dimmed until the server says
 * it is out. */
function isInContext(envelope: EntryEnvelope): boolean {
  return envelope.in_context !== false;
}

/**
 * Turns a freshly opened popup upward when opening it downward would leave
 * the visible area. The kebab is at the entry's bottom-right, so for the
 * last entry — the one most often acted on — a downward popup would open
 * into the composer and be cut off.
 *
 * The measurement waits one frame: the `ref` runs while the popup is still
 * detached, before it has a parent or a layout, so an immediate read sees
 * an empty box and never flips. The bound is the viewport, not only the
 * transcript's scroll box — on a phone the scroller already ends at the
 * screen's edge, and the viewport is the one the user actually sees.
 */
function fitPopup(node: HTMLElement): void {
  requestAnimationFrame(() => {
    const limit = Math.min(
      window.innerHeight,
      node.closest('.transcript')?.getBoundingClientRect().bottom ?? window.innerHeight,
    );
    if (node.getBoundingClientRect().bottom > limit) node.classList.add('up');
  });
}

export function TranscriptEntry(props: { envelope: EntryEnvelope; sessionId: string | null; menu: EntryMenuProps }): JSX.Element {
  const kind = (): EntryKind => props.envelope.kind;
  const id = (): string => props.envelope.entry.id;
  const pinned = (): boolean => isPinned(props.envelope);
  const inContext = (): boolean => isInContext(props.envelope);
  // A pin overrides exclusion, so a pinned entry is never drawn as out of
  // context whatever its override says.
  const excluded = (): boolean => !pinned() && !inContext();
  const open = (): boolean => props.menu.openFor() === id();
  // A just-sent message is shown before the server names it; its id is the
  // page's own, so it can carry no menu until the real entry replaces it.
  const real = (): boolean => !isOptimistic(id());
  return (
    <article
      class={`entry entry-${kind()}${pinned() ? ' pinned' : ''}${excluded() ? ' excluded' : ''}`}
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
              {/* A pin overrides exclusion, so a pinned entry offers no
                  context toggle: it would silently do nothing. The label
                  names the action, read from the effective state. */}
              <Show when={!pinned()}>
                <button
                  type="button"
                  class="entry-menu-item"
                  role="menuitem"
                  data-testid="entry-menu-context"
                  onClick={() => {
                    props.menu.send({ type: 'toggle_context', entry_id: id() });
                    props.menu.toggle(id());
                  }}
                >
                  {inContext() ? 'Exclude from context' : 'Include in context'}
                </button>
              </Show>
              <button
                type="button"
                class="entry-menu-item"
                role="menuitem"
                data-testid="entry-menu-fork"
                onClick={() => {
                  props.menu.send({ type: 'fork', entry_id: id() });
                  props.menu.beginFork(props.sessionId);
                  props.menu.toggle(id());
                }}
              >
                Fork
              </button>
            </div>
          </Show>
        </span>
      </Show>
    </article>
  );
}

/** The persisted map's single value, whatever its tag. */

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
      return (
        <ToolResultView value={body(props.envelope)} sessionId={props.sessionId} entryId={props.envelope.entry.id} />
      );
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
