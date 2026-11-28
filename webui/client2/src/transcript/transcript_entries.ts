// The transcript's entry state: the bounded tail the page holds, its
// position index, and the mutations a frame performs on it.
//
// The transcript is one array of envelopes in history order. An
// `entry_upsert` carries the server's own index, so the client never
// re-derives ordering; an `entry_delta` names its entry by id, resolved
// through the positions map so a streamed token never scans the transcript.
//
// The index is module-level and lives here with every function that
// rebuilds or reads it: the array that is installed and the index that
// names it must be rebuilt together, or an upsert lands at the wrong slot.

import type { ClientStore } from '../store/store';
import { belongsToViewedSession } from '../store/store';
import type { AnyEntryKindMap, DeltaField, EntryEnvelope, EntryKind, ServerMsg } from '../protocol/protocol';
import { debugStream } from '../protocol/debug_stream';
import { body } from './shared';

/** entry id -> its index in `entries`. Rebuilt on every structural change. */
let positions = new Map<string, number>();

function reindex(entries: readonly EntryEnvelope[]): void {
  positions = new Map(entries.map((envelope, index) => [envelope.entry.id, index]));
}

/** The snapshot installs a whole transcript; its index follows it. */
export function indexSnapshot(entries: readonly EntryEnvelope[]): void {
  reindex(entries);
}

const OPTIMISTIC_PREFIX = 'optimistic-';

/** Whether an entry id is the page's own placeholder for a just-sent message. */
export function isOptimistic(id: string): boolean {
  return id.startsWith(OPTIMISTIC_PREFIX);
}

/** The newest real entry id the page holds, for the next `hello`. */
export function newestEntryId(entries: readonly EntryEnvelope[]): string | null {
  for (let i = entries.length - 1; i >= 0; i -= 1) {
    const id = entries[i]?.entry.id;
    if (id !== undefined && !id.startsWith(OPTIMISTIC_PREFIX)) return id;
  }
  return null;
}

/** `entries_page`: a page of older entries for the viewed session. */
export function applyEntriesPage(store: ClientStore, msg: Extract<ServerMsg, { type: 'entries_page' }>): void {
  if (store.state.sessionId !== msg.session_id) return;
  prependPage(store, msg.entries, msg.entries_before);
}

/** `entry_upsert`: one entry at the index the server names. */
export function applyEntryUpsert(store: ClientStore, msg: Extract<ServerMsg, { type: 'entry_upsert' }>): void {
  if (!belongsToViewedSession(store, msg.session_id)) return;
  upsert(store, msg.index, msg.entry);
}

/** `entry_delta`: one streamed chunk appended to a growing field. */
export function applyEntryDelta(store: ClientStore, msg: Extract<ServerMsg, { type: 'entry_delta' }>): void {
  if (!belongsToViewedSession(store, msg.session_id)) {
    debugStream('drop', { entry: msg.entry_id, reason: 'delta is for a session the page is not viewing', session: msg.session_id });
    return;
  }
  appendDelta(store, msg.entry_id, msg.field, msg.offset_bytes, msg.chunk);
}

function upsert(store: ClientStore, index: number, envelope: EntryEnvelope): void {
  // The array that is installed, so the index below is rebuilt from the
  // very array the transcript now holds and not from a read-back of the
  // store, which can lag the write by one frame.
  let installed: EntryEnvelope[] = [];
  store.set((s) => {
    // A real user entry is the server catching up with a sent message:
    // that is the moment every optimistic placeholder is superseded.
    const base = envelope.kind === 'user' ? dropOptimistic(s.entries) : s.entries;
    const next = [...base];

    // An entry is in the transcript exactly once, at the index the server
    // names. A same-id copy is removed by search rather than through the
    // index: the first real upsert drops the optimistic placeholder, and a
    // second upsert for the same entry can arrive before the index
    // catches up — gated on the index, it skipped the removal and appended
    // a second copy, so one user entry with an image showed twice.
    const at = next.findIndex((e) => e.entry.id === envelope.entry.id);
    if (at !== -1) next.splice(at, 1);
    // The index is live: it counts from the session's first entry, while
    // the page holds only a tail. Shift it by what precedes the tail, and
    // an entry inside the tail lands at its own slot while a new one at
    // the end still lands at the end.
    next.splice(clamp(index - s.entriesBefore, next.length), 0, envelope);
    s.entries = next;
    installed = next;
  });
  reindex(installed);
}

/** Prepends one page of older entries, keeping the index offset true. */
function prependPage(store: ClientStore, page: EntryEnvelope[], entriesBefore: number): void {
  let installed: EntryEnvelope[] = [];
  store.set((s) => {
    s.entries = [...page, ...s.entries];
    s.entriesBefore = entriesBefore;
    s.loadingMore = false;
    installed = s.entries;
  });
  reindex(installed);
}

/** A copy of the transcript without optimistic placeholders. */
function dropOptimistic(entries: EntryEnvelope[]): EntryEnvelope[] {
  if (!entries.some((e) => e.entry.id.startsWith(OPTIMISTIC_PREFIX))) return entries;
  return entries.filter((e) => !e.entry.id.startsWith(OPTIMISTIC_PREFIX));
}

function clamp(index: number, length: number): number {
  return Math.max(0, Math.min(index, length));
}

/** Appends a streamed chunk to one of the four fields that grow.
 *
 * Every way out is named on the stream trace (`?debug=stream`): a chunk
 * that is refused is logged with the reason it was, so a frozen stream is
 * followed to the one return that took it. */
function appendDelta(
  store: ClientStore,
  entryId: string,
  field: DeltaField,
  offsetBytes: number,
  chunk: string,
): void {
  const index = positions.get(entryId);
  if (index === undefined) {
    debugStream('drop', { entry: entryId, reason: 'no position for entry' });
    return;
  }
  const envelope = store.state.entries[index];
  if (!envelope) {
    debugStream('drop', { entry: entryId, reason: 'position names no entry', index });
    return;
  }
  if (GROWING_FIELD[envelope.kind] !== field) {
    debugStream('drop', { entry: entryId, reason: 'field does not grow on this kind', kind: envelope.kind, field });
    return;
  }

  const current = streamedText(envelope);
  // The chunk is the field's text from its own offset on, so the text before
  // that offset is kept and the rest is the chunk: a whole-entry upsert may
  // have delivered some or all of these words already, and whatever it did
  // deliver is replaced rather than repeated.
  const at = byteOffsetToIndex(current, offsetBytes);
  const next = current.slice(0, at) + chunk;
  if (next === current) {
    debugStream('apply', { entry: entryId, field, from: current.length, to: current.length, repeated: true });
    return;
  }
  store.set((s) => {
    s.entries[index] = withStreamedText(envelope, next);
  });
  debugStream('apply', { entry: entryId, field, from: current.length, to: next.length, offset: offsetBytes });
}

/** The envelope with its streamed text set to `text`, its shape unchanged. */
function withStreamedText(envelope: EntryEnvelope, text: string): EntryEnvelope {
  const value: unknown = body(envelope);
  if (typeof value === 'string') {
    const tag = Object.keys(envelope.entry.kind)[0];
    return { ...envelope, entry: { ...envelope.entry, kind: { [tag]: text } as AnyEntryKindMap } };
  }
  const tag = Object.keys(envelope.entry.kind)[0];
  const key = WIRE_KEY[envelope.kind] ?? '';
  return {
    ...envelope,
    entry: {
      ...envelope.entry,
      kind: { [tag]: { ...(value as Record<string, unknown>), [key]: text } } as AnyEntryKindMap,
    },
  };
}

/** The one field of each streamed kind that grows, keyed by the kind name
 *  the envelope carries. A delta names its field on the wire; a kind grows
 *  exactly one, so a delta can never land on the wrong one. */
const GROWING_FIELD: Partial<Record<EntryKind, DeltaField>> = {
  assistant: 'text',
  thinking: 'thinking',
  tool_call: 'args',
  tool_result: 'output',
};

/** The text of a streamed kind, as it stands. */
function streamedText(envelope: EntryEnvelope): string {
  const value: unknown = body(envelope);
  if (typeof value === 'string') return value;
  if (typeof value === 'object' && value !== null) {
    const text = (value as Record<string, unknown>)[WIRE_KEY[envelope.kind as EntryKind] ?? ''];
    if (typeof text === 'string') return text;
  }
  return '';
}

/** The struct key each struct kind's text lives under. */
const WIRE_KEY: Partial<Record<EntryKind, string>> = {
  tool_call: 'arguments',
  tool_result: 'content',
};

/**
 * The string index that a UTF-8 byte offset names in `text`.
 *
 * A delta's offset is in bytes, because the server's cursor is; a
 * JavaScript string is indexed in UTF-16 code units, and the two agree only
 * for ASCII. The prefix of that many bytes is decoded and its length taken.
 *
 * A well-formed offset is always on a character boundary, since the server
 * slices a `&str`. One that is not can only come from a malformed frame or
 * from text the page holds differing from what was sliced, and it must land
 * before the character it would split, never inside it: a permissive decoder
 * would turn the dangling bytes into a replacement character and read one
 * code unit too far. So the decoder is fatal, and on a split the offset
 * steps back a byte at a time until it is whole.
 */
export function byteOffsetToIndex(text: string, offsetBytes: number): number {
  const bytes = new TextEncoder().encode(text).slice(0, offsetBytes);
  const decoder = new TextDecoder('utf-8', { fatal: true });
  for (let end = bytes.length; end > 0; end -= 1) {
    try {
      return decoder.decode(bytes.subarray(0, end)).length;
    } catch {
      // The offset split a character: try one byte earlier.
    }
  }
  return 0;
}

let optimisticCounter = 0;

/**
 * Adds an optimistic user entry so a message appears the instant it is
 * sent. It has no real id; the server's own entry replaces it.
 */
export function addOptimistic(store: ClientStore, text: string): void {
  const envelope: EntryEnvelope = {
    kind: 'user',
    entry: {
      id: `${OPTIMISTIC_PREFIX}${optimisticCounter++}`,
      kind: { User: { display: text, expanded: text } },
      context_override: 'default',
      context_history: [],
    },
  };
  let installed: EntryEnvelope[] = [];
  store.set((s) => {
    s.entries = [...s.entries, envelope];
    installed = s.entries;
  });
  reindex(installed);
}

/** The text a user entry displays. */
export function readUserText(envelope: EntryEnvelope): string {
  const inner = envelope.entry.kind as { User?: { display?: string } };
  return inner.User?.display ?? '';
}

/** The text a single-string entry displays (system, error, assistant...). */
export function readText(envelope: EntryEnvelope): string {
  return streamedText(envelope);
}
