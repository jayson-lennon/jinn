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
import type { DeltaField, EntryEnvelope, ServerMsg } from '../protocol/protocol';

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
  if (!belongsToViewedSession(store, msg.session_id)) return;
  appendDelta(store, msg.entry_id, msg.field, msg.chunk);
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

/** Appends a streamed chunk to one of the four fields that grow. */
function appendDelta(store: ClientStore, entryId: string, field: DeltaField, chunk: string): void {
  const index = positions.get(entryId);
  if (index === undefined) return;
  const envelope = store.state.entries[index];
  if (!envelope) return;

  const inner = envelope.entry.kind as Record<string, Record<string, unknown> | undefined>;
  const tag = Object.keys(inner)[0];
  const body = tag === undefined ? undefined : inner[tag];
  if (typeof body !== 'object' || body === null) return;

  // Each streamed kind stores its growing text under a known key; the tag
  // names which one, so a delta can never land on the wrong field.
  const key = GROWING_FIELD[tag];
  if (key === undefined || key !== WIRE_FIELD[field]) return;
  const current = body[key];
  if (typeof current !== 'string') return;

  const grown = { ...body, [key]: current + chunk };
  store.set((s) => {
    s.entries[index] = { ...envelope, entry: { ...envelope.entry, kind: { [tag]: grown } } };
  });
}

/** The wire's `field` -> the entry key it grows. */
const WIRE_FIELD: Record<DeltaField, string> = {
  text: 'text',
  thinking: 'text',
  args: 'arguments',
  output: 'content',
};

/** The persisted map's tag -> the one field of it that streams. */
const GROWING_FIELD: Record<string, string> = {
  Assistant: 'text',
  Thinking: 'text',
  ToolCall: 'arguments',
  ToolResult: 'content',
};

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
  const inner = envelope.entry.kind as Record<string, unknown>;
  const tag = Object.keys(inner)[0];
  if (tag === undefined) return '';
  const body = inner[tag];
  if (typeof body === 'string') return body;
  if (typeof body === 'object' && body !== null) {
    const text = (body as { text?: unknown }).text;
    if (typeof text === 'string') return text;
  }
  return '';
}
