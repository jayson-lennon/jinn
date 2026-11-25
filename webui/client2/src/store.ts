// The client store: the browser's single owner of view state.
//
// Everything the page shows lives here, and only here. A frame from the
// server is *applied* to the store — never rendered directly — so the
// rendered page is always a pure function of the store and can be rebuilt
// from a snapshot after a reconnect without carrying anything else.
//
// The transcript is one array of envelopes in history order. An
// `entry_upsert` carries the server's own index, so the client never
// re-derives ordering; an `entry_delta` names its entry by id, resolved
// through a positions map so a streamed token never scans the transcript.

import { createStore as createStoreValue } from 'solid-js';
import type { Store, StoreSetter } from 'solid-js';
import type {
  DeltaField,
  EntryEnvelope,
  ModelRow,
  Phase,
  PickerRow,
  ServerMsg,
  SessionRow,
} from './protocol';

export interface ViewState {
  /** The instance's active session, or null before the first snapshot. */
  sessionId: string | null;
  phase: Phase;
  modelLabel: string;
  title: string;
  /** The history revision the page last saw, echoed back on reconnect. */
  revision: number;
  entries: EntryEnvelope[];
  sessions: SessionRow[];
  pickerRows: PickerRow[];
  pickerFilter: string;
  modelRows: ModelRow[];
  modelFilter: string;
  /** The most recent refusal, if any. */
  error: string | null;
  /** True once a snapshot or a resume has landed on this connection. */
  synced: boolean;
}

const INITIAL_STATE: ViewState = {
  sessionId: null,
  phase: 'Idle',
  modelLabel: '',
  title: '',
  revision: 0,
  entries: [],
  sessions: [],
  pickerRows: [],
  pickerFilter: '',
  modelRows: [],
  modelFilter: '',
  error: null,
  synced: false,
};

/** The reactive store: read `state` as a plain object; write through `set`. */
export interface ClientStore {
  state: Store<ViewState>;
  set: StoreSetter<ViewState>;
}

/** entry id -> its index in `entries`. Rebuilt on every structural change. */
let positions = new Map<string, number>();

function reindex(entries: readonly EntryEnvelope[]): void {
  positions = new Map(entries.map((envelope, index) => [envelope.entry.id, index]));
}

export function createStore(initial: ViewState = INITIAL_STATE): ClientStore {
  const [state, set] = createStoreValue(initial);
  return { state, set };
}

/** Whether a session is running a turn — the server's own definition. */
export function isBusy(phase: Phase): boolean {
  return phase !== 'Idle';
}

/** The newest real entry id the page holds, for the next `hello`. */
export function newestEntryId(entries: readonly EntryEnvelope[]): string | null {
  for (let i = entries.length - 1; i >= 0; i -= 1) {
    const id = entries[i]?.entry.id;
    if (id !== undefined && !id.startsWith(OPTIMISTIC_PREFIX)) return id;
  }
  return null;
}

const OPTIMISTIC_PREFIX = 'optimistic-';
let optimisticCounter = 0;

/**
 * Applies one frame: the browser's single dispatch. Every `ServerMsg`
 * variant has exactly one arm and there is no fallback, so a new frame
 * kind is a type error until it is handled here.
 */
export function apply(store: ClientStore, msg: ServerMsg): void {
  switch (msg.type) {
    case 'snapshot': {
      store.set((s) => {
        s.sessionId = msg.session_id;
        s.phase = msg.phase.phase;
        s.modelLabel = msg.model_label;
        s.title = msg.title;
        s.revision = msg.revision;
        s.entries = msg.entries;
        s.sessions = msg.sessions;
        s.synced = true;
      });
      reindex(store.state.entries);
      return;
    }
    case 'entry_upsert': {
      if (!belongsToViewedSession(store, msg.session_id)) return;
      upsert(store, msg.index, msg.entry);
      return;
    }
    case 'entry_delta': {
      if (!belongsToViewedSession(store, msg.session_id)) return;
      appendDelta(store, msg.entry_id, msg.field, msg.chunk);
      return;
    }
    case 'phase': {
      if (!belongsToViewedSession(store, msg.session_id)) return;
      store.set((s) => {
        s.phase = msg.phase;
      });
      return;
    }
    case 'sessions': {
      store.set((s) => {
        s.sessions = msg.rows;
      });
      return;
    }
    case 'model': {
      if (!belongsToViewedSession(store, msg.session_id)) return;
      store.set((s) => {
        s.modelLabel = msg.label;
      });
      return;
    }
    case 'title': {
      if (!belongsToViewedSession(store, msg.session_id)) return;
      store.set((s) => {
        s.title = msg.title;
        // The sidebar row shows the same title; one message updates both
        // places it is displayed, so they cannot disagree.
        s.sessions = s.sessions.map((row) =>
          row.session_id === msg.session_id ? { ...row, title: msg.title } : row,
        );
      });
      return;
    }
    case 'picker_rows': {
      // A late answer to a filter the user has typed past is dropped.
      if (msg.filter !== store.state.pickerFilter) return;
      store.set((s) => {
        s.pickerRows = msg.rows;
      });
      return;
    }
    case 'model_rows': {
      if (msg.filter !== store.state.modelFilter) return;
      store.set((s) => {
        s.modelRows = msg.rows;
      });
      return;
    }
    case 'error': {
      store.set((s) => {
        s.error = msg.message;
      });
      return;
    }
    case 'ping': {
      // A heartbeat carries no fact. Its arrival is the whole effect: the
      // socket's liveness timer resets on every frame.
      return;
    }
  }
}

/**
 * A frame that names a session the page has left is discarded: the fact it
 * describes belongs to a transcript the user is no longer looking at. A
 * page with no session yet accepts anything.
 */
function belongsToViewedSession(store: ClientStore, sessionId: string): boolean {
  const viewed = store.state.sessionId;
  return viewed === null || viewed === sessionId;
}

function upsert(store: ClientStore, index: number, envelope: EntryEnvelope): void {
  store.set((s) => {
    // A real user entry is the server catching up with a sent message:
    // that is the moment every optimistic placeholder is superseded.
    const base = envelope.kind === 'user' ? dropOptimistic(s.entries) : s.entries;
    const next = [...base];

    // The entry may already be known (an in-place rewrite) or relocated (a
    // compaction reordered the history); either way it ends up exactly
    // once, where the server says it sits.
    const known = positions.get(envelope.entry.id);
    if (known !== undefined) {
      const at = next.findIndex((e) => e.entry.id === envelope.entry.id);
      if (at !== -1) next.splice(at, 1);
    }
    next.splice(clamp(index, next.length), 0, envelope);
    s.entries = next;
  });
  reindex(store.state.entries);
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
  store.set((s) => {
    s.entries = [...s.entries, envelope];
  });
  reindex(store.state.entries);
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
