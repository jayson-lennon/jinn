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
import type { LastSeen } from './unread';
import type {
  AppletRow,
  DeltaField,
  EntryEnvelope,
  McpRow,
  ModelRow,
  PersonaRow,
  Phase,
  PickerRow,
  ServerMsg,
  SessionRow,
  SkillRow,
  Theme,
  ToolRow,
  ImageUpload,
  LifecycleOption,
} from './protocol';

export interface ViewState {
  /** The instance's active session, or null before the first snapshot. */
  sessionId: string | null;
  phase: Phase;
  /** How many messages wait behind the running turn. */
  queueLen: number;
  modelLabel: string;
  title: string;
  /** The history revision the page last saw, echoed back on reconnect. */
  revision: number;
  entries: EntryEnvelope[];
  /** How many entries of the session precede the oldest one held: the
   *  transcript is a bounded tail, and this is how much older history a
   *  scroll to the top can still ask for. */
  entriesBefore: number;
  /** Whether a page of older entries is in flight, so the top of the
   *  transcript shows a spinner and a second request cannot fire. */
  loadingMore: boolean;
  sessions: SessionRow[];
  /** The library members the sidebar's Library view lists. */
  library: SessionRow[];
  /** Which sidebar list is showing. */
  railView: 'sessions' | 'library';
  /** The persisted color mode, as the server last reported it. */
  theme: Theme;
  /** The composer's draft. In the store, not the component, so a snapshot
   *  for another session can leave it alone: a session switch never costs
   *  the user their unsent text. */
  draft: string;
  /** The composer's pending images, in chip order. */
  chips: ImageUpload[];
  /** True while the model picker is waiting for a refresh to land. */
  refreshing: boolean;
  /** The new-session form's choices, once asked for. */
  newSession: { projects: string[]; lifecycles: LifecycleOption[] } | null;
  pickerRows: PickerRow[];
  pickerFilter: string;
  modelRows: ModelRow[];
  modelFilter: string;
  /** The skills modal's rows, for the filter the page last asked for. */
  skillRows: SkillRow[];
  skillFilter: string;
  /** The tools modal's rows, for the filter the page last asked for. */
  toolRows: ToolRow[];
  toolFilter: string;
  /** The MCP servers modal's rows, for the filter the page last asked for. */
  mcpRows: McpRow[];
  mcpFilter: string;
  /** The persona modal's rows, for the filter the page last asked for. */
  personaRows: PersonaRow[];
  personaFilter: string;
  /** The applets modal's rows, for the filter the page last asked for. */
  appletRows: AppletRow[];
  appletFilter: string;
  /** The applets server's serving phase, as the last rows frame carried it. */
  appletServing: { serving: boolean; bind: string };
  /** Transient notices, newest last. A toast dismisses itself, and a tap
   *  dismisses it at once. Not the session's: a snapshot leaves them. */
  toasts: Toast[];
  /** The session the page asked to activate and is still waiting on — the
   *  loading overlay's cause. Null when nothing is being waited for. */
  loadingSessionId: string | null;
  /** True once a snapshot or a resume has landed on this connection. */
  synced: boolean;
  /** When the page last saw each session (RFC 3339), for the unread mark.
   *  A row is unread when its settle time is newer than its entry here. */
  lastSeen: Record<string, string>;
}

/** One transient notice the page shows until it is dismissed. */
export interface Toast {
  readonly id: number;
  readonly message: string;
}

const INITIAL_STATE: ViewState = {
  sessionId: null,
  phase: 'Idle',
  queueLen: 0,
  modelLabel: '',
  title: '',
  revision: 0,
  entries: [],
  entriesBefore: 0,
  loadingMore: false,
  sessions: [],
  library: [],
  railView: 'sessions',
  theme: 'system',
  draft: '',
  chips: [],
  refreshing: false,
  newSession: null,
  pickerRows: [],
  pickerFilter: '',
  modelRows: [],
  modelFilter: '',
  skillRows: [],
  skillFilter: '',
  toolRows: [],
  toolFilter: '',
  mcpRows: [],
  mcpFilter: '',
  personaRows: [],
  personaFilter: '',
  appletRows: [],
  appletFilter: '',
  appletServing: { serving: false, bind: '' },
  toasts: [],
  loadingSessionId: null,
  synced: false,
  lastSeen: {},
};

/** The reactive store: read `state` as a plain object; write through `set`. */
export interface ClientStore {
  state: Store<ViewState>;
  set: StoreSetter<ViewState>;
}

/**
 * One line on the console for an unread-mark decision. Off unless the
 * page is opened with `?debug=unread`: the mark is a quiet feature and
 * normal operation logs nothing, but a mark that will not clear is only
 * diagnosable from the two times behind it.
 */
const UNREAD_DEBUG: boolean = (() => {
  try {
    return new URLSearchParams(window.location.search).get('debug') === 'unread';
  } catch {
    return false;
  }
})();

function debugUnread(message: string, fields: Record<string, unknown>): void {
  if (!UNREAD_DEBUG) return;
  console.debug(`[unread] ${message}`, fields);
}

/** entry id -> its index in `entries`. Rebuilt on every structural change. */
let positions = new Map<string, number>();

function reindex(entries: readonly EntryEnvelope[]): void {
  positions = new Map(entries.map((envelope, index) => [envelope.entry.id, index]));
}

/** Where the last-seen map persists, so a mark survives a reload. */
const LAST_SEEN_KEY = 'jinn.web2.lastSeen';

/**
 * The saved last-seen map, or an empty one. Storage can be refused
 * (private mode, a quota) and a saved value can be stale or corrupt;
 * either way the page starts with no memory and marks nothing as read
 * that it cannot prove it saw.
 */
function loadLastSeen(): Record<string, string> {
  try {
    const raw = window.localStorage.getItem(LAST_SEEN_KEY);
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(
        (entry): entry is [string, string] =>
          typeof entry[1] === 'string' && Number.isFinite(Date.parse(entry[1])),
      ),
    );
  } catch {
    return {};
  }
}

/** The row's settle time when it is the viewed session's, in either list. */
function viewedRowSettledAt(
  sessionId: string,
  sessions: readonly SessionRow[],
  library: readonly SessionRow[],
): string | null {
  const row = [...sessions, ...library].find((r) => r.session_id === sessionId);
  return row === undefined || row.last_settled_at === null ? null : row.last_settled_at;
}

/**
 * Persists the map, less any session no longer listed: archived sessions
 * must not accumulate. A refused write leaves the in-memory map intact.
 */
function saveLastSeen(lastSeen: LastSeen, sessions: readonly SessionRow[], library: readonly SessionRow[]): void {
  const listed = new Set([...sessions, ...library].map((row) => row.session_id));
  const kept = Object.fromEntries(
    Object.entries(lastSeen).filter(([id]) => listed.has(id)),
  );
  try {
    window.localStorage.setItem(LAST_SEEN_KEY, JSON.stringify(kept));
  } catch {
    // As `theme.ts`: the hint is an optimization, never a correctness need.
  }
}

export function createStore(initial: ViewState = INITIAL_STATE): ClientStore {
  // A fresh object, never the shared default: the loaded map belongs to
  // this store alone.
  const [state, set] = createStoreValue({ ...initial, lastSeen: loadLastSeen() });
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

/** Whether an entry id is the page's own placeholder for a just-sent message. */
export function isOptimistic(id: string): boolean {
  return id.startsWith(OPTIMISTIC_PREFIX);
}
let optimisticCounter = 0;

/**
 * Applies one frame: the browser's single dispatch. Every `ServerMsg`
 * variant has exactly one arm and there is no fallback, so a new frame
 * kind is a type error until it is handled here.
 *
 * A snapshot is always applied. It replaces which session the page views
 * and that session's transcript; the draft and pending images are not
 * the transcript's and are left as they are, so following a switch made
 * elsewhere never discards what the user is typing.
 */
export function apply(store: ClientStore, msg: ServerMsg): void {
  switch (msg.type) {
    case 'snapshot': {
      // Read before the setter: whether this snapshot is the one the page
      // is waiting on, and so must end the loading overlay.
      const loadingId = store.state.loadingSessionId;
      store.set((s) => {
        s.sessionId = msg.session_id;
        s.phase = msg.phase.phase;
        s.queueLen = msg.phase.queue_len;
        s.modelLabel = msg.model_label;
        s.title = msg.title;
        s.revision = msg.revision;
        s.entries = msg.entries;
        s.entriesBefore = msg.entries_before;
        s.loadingMore = false;
        s.sessions = msg.sessions;
        s.library = msg.library;
        s.theme = msg.theme;
        s.synced = true;
        // Not the draft or the chips: they belong to the user, not to the
        // session, and a switch must not take unsent text with it.
        // A different session's picker rows are not the ones the page was
        // looking at; an open modal re-asks for its own rows on open.
        s.skillRows = [];
        s.skillFilter = '';
        s.toolRows = [];
        s.toolFilter = '';
        s.mcpRows = [];
        s.mcpFilter = '';
        s.personaRows = [];
        s.personaFilter = '';
        // The page is now viewing this session: whatever settled before
        // this moment is seen. One arm covers a click on the page and a
        // switch made elsewhere that the page follows — both land here.
        // The page is now viewing this session: whatever settled before
        // this moment is seen. One arm covers a click on the page and a
        // switch made elsewhere that the page follows — both land here.
        // The time recorded is the server's own settle time for the
        // session, never the browser's clock: the two run on different
        // machines and drift, and a browser that is behind would record
        // a "seen" older than the settle and never clear the mark.
        if (msg.session_id !== null) {
          const settled = viewedRowSettledAt(msg.session_id, msg.sessions, msg.library);
          const seen = settled ?? new Date().toISOString();
          s.lastSeen[msg.session_id] = seen;
          if (UNREAD_DEBUG) {
            debugUnread('snapshot: marked seen', {
              session: msg.session_id,
              seenAt: seen,
              rowSettledAt: settled,
            });
          }
        }
        // Only the snapshot the page is waiting for ends the wait. One
        // naming another session — the old session's, answered when a disk
        // load outlived the handler's own wait — leaves the overlay up until
        // the real one lands.
        if (loadingId !== null && msg.session_id === loadingId) {
          s.loadingSessionId = null;
        }
      });
      // Indexed from the frame's own array, not the store's. Reading the
      // store's array here left the index naming the session just left,
      // so an upsert for the new session was appended instead of
      // replacing its copy from the snapshot, and the entry showed twice.
      reindex(msg.entries);
      if (msg.session_id !== null) saveLastSeen(store.state.lastSeen, msg.sessions, msg.library);
      return;
    }
    case 'theme': {
      store.set((s) => {
        s.theme = msg.theme;
      });
      return;
    }
    case 'new_session_options': {
      store.set((s) => {
        s.newSession = { projects: msg.projects, lifecycles: msg.lifecycles };
      });
      return;
    }
    case 'entries_page': {
      if (store.state.sessionId !== msg.session_id) return;
      prependPage(store, msg.entries, msg.entries_before);
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
        s.queueLen = msg.queue_len;
      });
      return;
    }
    case 'sessions': {
      // While a session is viewed, each settle it reports is seen: copy
      // the server's own time, so the comparison is exact rather than
      // against the browser's clock. The viewed session's row is in
      // whichever list holds it. A frame with no such settle changes
      // nothing, and the hot path then skips the storage write.
      const viewed = store.state.sessionId;
      const settled = viewed === null ? null : viewedRowSettledAt(viewed, msg.rows, msg.library);
      // Only a settle the map does not already hold is a change; the same
      // time arrives on every history append of the viewed session.
      const changed = settled !== null && store.state.lastSeen[viewed ?? ''] !== settled;
      store.set((s) => {
        s.sessions = msg.rows;
        s.library = msg.library;
        if (viewed !== null && settled !== null) {
          s.lastSeen[viewed] = settled;
        }
      });
      if (changed) {
        saveLastSeen(store.state.lastSeen, msg.rows, msg.library);
      }
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
      // A push for a session the page has left, or for text the user has
      // typed past, is not the view the page is holding.
      if (store.state.sessionId !== null && msg.session_id !== store.state.sessionId) return;
      if (msg.filter !== store.state.modelFilter) return;
      store.set((s) => {
        s.modelRows = msg.rows;
        // A push is how the page learns its refresh finished.
        s.refreshing = false;
      });
      return;
    }
    case 'skill_rows': {
      // A frame for a session the page has left, or for text the user
      // has typed past, is not the view the page is holding.
      if (store.state.sessionId !== null && msg.session_id !== store.state.sessionId) return;
      if (msg.filter !== store.state.skillFilter) return;
      store.set((s) => {
        s.skillRows = msg.rows;
      });
      return;
    }
    case 'tool_rows': {
      // The same two guards as the skills frame: the viewed session, and
      // the filter the page is holding right now.
      if (store.state.sessionId !== null && msg.session_id !== store.state.sessionId) return;
      if (msg.filter !== store.state.toolFilter) return;
      store.set((s) => {
        s.toolRows = msg.rows;
      });
      return;
    }
    case 'mcp_rows': {
      if (store.state.sessionId !== null && msg.session_id !== store.state.sessionId) return;
      if (msg.filter !== store.state.mcpFilter) return;
      store.set((s) => {
        s.mcpRows = msg.rows;
      });
      return;
    }
    case 'persona_rows': {
      if (store.state.sessionId !== null && msg.session_id !== store.state.sessionId) return;
      if (msg.filter !== store.state.personaFilter) return;
      store.set((s) => {
        s.personaRows = msg.rows;
      });
      return;
    }
    case 'applet_rows': {
      // The saved-app catalog is instance-wide, so there is no session to
      // match; the filter alone says whether this is the view the page is
      // holding. A push from an open modal re-asks in the same filter, so
      // it is never dropped by this guard.
      if (msg.filter !== store.state.appletFilter) return;
      store.set((s) => {
        s.appletRows = msg.rows;
        s.appletServing = { serving: msg.serving, bind: msg.bind };
      });
      return;
    }
    case 'error': {
      // Every refusal and every failure the server reports is one notice,
      // and it ends any wait: the request it answers will never bring a
      // snapshot, and the overlay must not outlive the answer.
      pushToast(store, msg.message);
      store.set((s) => {
        s.loadingSessionId = null;
      });
      return;
    }
    case 'ping': {
      // A heartbeat carries no fact. Its arrival is the whole effect: the
      // socket's liveness timer resets on every frame.
      return;
    }
  }
  // No fallback arm: a message kind without a case above must fail the
  // type check here, not fall through unapplied.
  const unhandled: never = msg;
  return void unhandled;
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

/** Replaces the composer's draft. */
export function setDraft(store: ClientStore, text: string): void {
  store.set((s) => {
    s.draft = text;
  });
}

/** Adds pending images to the composer, in the order given. */
export function addChips(store: ClientStore, images: ImageUpload[]): void {
  store.set((s) => {
    s.chips = [...s.chips, ...images];
  });
}

/** Removes one pending image, by its position in chip order. */
export function removeChip(store: ClientStore, index: number): void {
  store.set((s) => {
    s.chips = s.chips.filter((_, at) => at !== index);
  });
}

/** Clears the draft and every pending image — what a successful send does. */
export function clearComposer(store: ClientStore): void {
  store.set((s) => {
    s.draft = '';
    s.chips = [];
  });
}

let toastCounter = 0;

/** Stacks one notice. The toast component owns its lifetime. */
export function pushToast(store: ClientStore, message: string): void {
  const id = toastCounter++;
  store.set((s) => {
    s.toasts = [...s.toasts, { id, message }];
  });
}

/** Removes one notice, by id. */
export function dismissToast(store: ClientStore, id: number): void {
  store.set((s) => {
    s.toasts = s.toasts.filter((t) => t.id !== id);
  });
}

/** Marks `id` as the session the page is waiting on — the loading overlay's cause. */
export function beginSessionLoad(store: ClientStore, id: string): void {
  store.set((s) => {
    s.loadingSessionId = id;
  });
}

/** Ends the wait, whatever ended it. */
export function clearSessionLoad(store: ClientStore): void {
  store.set((s) => {
    s.loadingSessionId = null;
  });
}

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
