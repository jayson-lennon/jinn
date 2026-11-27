// The transcript's scroll box: the viewport the entries render into, the
// two jump buttons over its corners, the follow rule that keeps a reader
// at the bottom under a live stream, and the older pages a scroll to the
// top asks for.
//
// The entries themselves — one component per kind — are `transcript.tsx`'s.
// This file owns the box around them and nothing inside them: its own view
// state (which ends the view is off) lives here and never reaches the store
// or the socket.

import { For, Show, createRenderEffect, createSignal, onCleanup, onSettled } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientMsg, EntryEnvelope } from '../protocol/protocol';
import type { ClientStore } from '../store/store';
import { beginFork } from '../session_load';
import { clearJump } from '../dash/dash';
import { TranscriptEntry, useEntryMenuDismiss } from './transcript';

// ---------------------------------------------------------------------------
// Transcript
// ---------------------------------------------------------------------------

/** How close to an edge, in pixels, still counts as being at it. One value
 *  drives both the follow rule and the two jump buttons, so a hidden
 *  jump-to-end always means the stream keeps the reader at the end. */
const EDGE_BAND_PX = 80;

export function Transcript(props: { store: ClientStore; send: (msg: ClientMsg) => void }): JSX.Element {
  let node: HTMLElement | undefined;
  // Which ends of the transcript the view is off. The page's own view state:
  // it lives here and never reaches the store or the socket.
  const [offStart, setOffStart] = createSignal(false);
  const [offEnd, setOffEnd] = createSignal(false);
  // At most one entry menu is open across the whole transcript, so the
  // open id is the transcript's — the way the rail owns its row menu.
  const [menuFor, setMenuFor] = createSignal<string | null>(null);
  const toggleMenu = (id: string): void => {
    setMenuFor(menuFor() === id ? null : id);
  };
  useEntryMenuDismiss(menuFor, () => setMenuFor(null));
  // Follow the stream: when the newest entry grows and the reader is at
  // the bottom, keep them there. A reader who scrolled up is left alone.
  const isNearBottom = (): boolean =>
    node !== undefined && node.scrollHeight - node.scrollTop - node.clientHeight < EDGE_BAND_PX;
  // Reads the scroll box and records which ends the view is off. Called on
  // every scroll, after the content changes, and on resize.
  const measure = (): void => {
    if (node === undefined) return;
    setOffStart(node.scrollTop > EDGE_BAND_PX);
    setOffEnd(node.scrollHeight - node.scrollTop - node.clientHeight > EDGE_BAND_PX);
  };
  // An instant scroll: the reader asked for an edge and lands on it now.
  const jumpTo = (top: number): void => {
    if (node === undefined) return;
    node.scrollTop = top;
    measure();
  };
  createRenderEffect(
    // Compute (tracked): the number of entries and the newest one's text.
    () => {
      const entries = props.store.state.entries;
      const last = entries[entries.length - 1];
      return { count: entries.length, tail: last === undefined ? '' : textOf(last) };
    },
    // Effect (untracked): keep a reader who is at the bottom at the bottom.
    // The decision is read before the microtask is queued: it asks where the
    // reader is now, before the new entry has grown the box under them.
    (now, prev) => {
      const follow = (prev === undefined || now.count !== prev.count || now.tail !== prev.tail) && isNearBottom();
      queueMicrotask(() => {
        if (!node) return;
        if (follow) node.scrollTop = node.scrollHeight;
        // After the layout the new entry produced: the end may now be out
        // of reach, and no scroll event follows a content change.
        measure();
      });
      // And once now, for a reader who was not followed: the content grew
      // while they read, and the next scroll may be a long time coming.
      measure();
    },
  );
  // Older entries, on scroll. The transcript is a bounded tail; the top of
  // it is the edge of what the page holds, not the start of the session.
  const oldestHeld = (): string | undefined => props.store.state.entries[0]?.entry.id;
  const requestOlder = (): void => {
    const s = props.store.state;
    // A jump streams its own pages in; a landing one can leave the reader
    // near the top, which must not start a second load beside it.
    if (s.loadingMore || s.jumpTarget !== null || s.entriesBefore === 0 || !s.synced) return;
    const before = oldestHeld();
    if (before === undefined) return;
    props.store.set((v) => {
      v.loadingMore = true;
    });
    props.send({ type: 'more_entries', before });
  };
  // Scroll anchoring across a prepend: the browser keeps `scrollTop` fixed,
  // which would jump the reader up by the page's height. Record the height
  // before, and restore the delta after the rows land.
  let heightBeforePage: number | null = null;
  createRenderEffect(
    () => props.store.state.loadingMore,
    (loading, prev) => {
      if (props.store.state.jumpTarget !== null) {
        heightBeforePage = null;
        return;
      }
      if (loading && node) {
        heightBeforePage = node.scrollHeight;
        return;
      }
      if (prev === true && !loading && node && heightBeforePage !== null) {
        const delta = node.scrollHeight - heightBeforePage;
        heightBeforePage = null;
        queueMicrotask(() => {
          if (node) node.scrollTop += delta;
        });
      }
    },
  );
  // A pin's jump: the target may be pages back, and the reply streams them
  // in one at a time. Each landing page changes the entry count, so this is
  // the moment to look: once the target is held, it goes to the top of the
  // box and the jump is over. Until then the reader is left where they are.
  // The target is tracked too, so a pin the page already holds — which the
  // server answers with nothing — is scrolled to at once rather than left
  // waiting on a page that will never come.
  createRenderEffect(
    () => [props.store.state.entries.length, props.store.state.jumpTarget] as const,
    () => {
      const target = props.store.state.jumpTarget;
      if (target === null || node === undefined) return;
      // A microtask, so the rows this change produced are laid out first.
      queueMicrotask(() => {
        if (node === undefined) return;
        const hit = node.querySelector<HTMLElement>(`[data-entry-id="${target}"]`);
        if (hit === null) return;
        jumpTo(hit.offsetTop - node.offsetTop);
        // The jump is over only once its own scroll has settled: the scroll
        // it makes lands the reader at the top, and the load-older guard
        // must still hold for that event — or it loads the whole session.
        // A macrotask later, the guard is released by clearing the target.
        setTimeout(() => clearJump(props.store), 0);
      });
    },
  );
  // One scroll handler carries both behaviours: the jump buttons' measure
  // and the older-page request, so neither can be wired without the other.
  const onTranscriptScroll = (): void => {
    measure();
    if (node && node.scrollTop < 160) requestOlder();
  };
  // The box resizes when the window does, when the phone's drawer opens,
  // and when the chips strip appears above the composer — each of those
  // moves an edge under a reader who has not scrolled, so no scroll event
  // reports it. The observer does. It is set up once the page has settled
  // (the ref is attached by then) and torn down when this component goes.
  onSettled(() => {
    measure();
    if (node === undefined) return;
    const observer = new ResizeObserver(() => measure());
    observer.observe(node);
    return () => observer.disconnect();
  });
  return (
    <div class="transcript-wrap">
      <section class="transcript" data-testid="transcript" ref={node} onScroll={onTranscriptScroll}>
        <Show when={props.store.state.loadingMore}>
          <div class="older-spinner" data-testid="older-spinner">
            <span class="spinner spinner-small" aria-hidden="true" /> loading older entries…
          </div>
        </Show>
        <Show when={!props.store.state.loadingMore && props.store.state.entriesBefore > 0}>
          <button type="button" class="older-more" data-testid="older-more" onClick={requestOlder}>
            load older entries
          </button>
        </Show>
        {/* Keyed by entry id, not by the envelope object: an upsert replaces
            the envelope in place, and a reference-keyed list would mount a
            second row for the same entry — one user entry, two images. */}
        <For each={props.store.state.entries}>
          {(envelope) => (
            <TranscriptEntry
              envelope={envelope}
              sessionId={props.store.state.sessionId}
              menu={{
                openFor: menuFor,
                toggle: toggleMenu,
                send: props.send,
                beginFork: (source) => beginFork(props.store, source),
              }}
            />
          )}
        </For>
      </section>
      {/* The two jump buttons float over the box's corners as its siblings,
          so they never scroll away with the entries. Each is shown only
          while the view is off that end. */}
      <Show when={offStart()}>
        <button
          type="button"
          class="jump jump-start"
          data-testid="jump-start"
          aria-label="Jump to start"
          onClick={() => jumpTo(0)}
        >
          <ArrowToLineIcon direction="up" />
        </button>
      </Show>
      <Show when={offEnd()}>
        <button
          type="button"
          class="jump jump-end"
          data-testid="jump-end"
          aria-label="Jump to end"
          onClick={() => jumpTo(node === undefined ? 0 : node.scrollHeight)}
        >
          <ArrowToLineIcon direction="down" />
        </button>
      </Show>
    </div>
  );
}

/** An arrow pointing at a bar — up for the start of the transcript, down
 *  for its end. Inline SVG: the glyph is identical on every OS and takes
 *  its colour from the button, where a Unicode arrow falls back to another
 *  font on some phones. */
function ArrowToLineIcon(props: { direction: 'up' | 'down' }): JSX.Element {
  const up = props.direction === 'up';
  return (
    <svg
      class="jump-icon"
      viewBox="0 0 24 24"
      width="18"
      height="18"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {/* The bar: above an up arrow, below a down arrow. */}
      <path d={up ? 'M5 3h14' : 'M5 21h14'} />
      {/* The arrow: shaft, then its head. */}
      <path d={up ? 'M12 21V8' : 'M12 3v13'} />
      <path d={up ? 'm6 14 6-6 6 6' : 'm6 10 6 6 6-6'} />
    </svg>
  );
}

/** A crude text signature of an entry, enough to notice a stream growing. */
function textOf(envelope: EntryEnvelope): string {
  const inner = envelope.entry.kind as Record<string, unknown>;
  const value = inner[Object.keys(inner)[0] ?? ''];
  if (typeof value === 'string') return value;
  if (typeof value === 'object' && value !== null) {
    const text = (value as { text?: unknown; content?: unknown }).text ?? (value as { content?: unknown }).content;
    if (typeof text === 'string') return text;
  }
  return '';
}
