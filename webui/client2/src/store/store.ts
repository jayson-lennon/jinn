// The client store: the browser's single owner of view state.
//
// Everything the page shows lives here, and only here. A frame from the
// server is *applied* to the store — never rendered directly — so the
// rendered page is always a pure function of the store and can be rebuilt
// from a snapshot after a reconnect without carrying anything else.
//
//
// This file is the store's shape and its one dispatch. The transcript's
// mutations live in `transcript/transcript_entries.ts`, the session lists in
// `rail/session_rows.ts`, the pickers' rows in `pickers/picker_rows.ts`, and
// so on — each `apply*` arm below delegates to its topic's folder; a frame
// kind is handled here or nowhere.

import { createStore as createStoreValue } from 'solid-js';
import type { Store, StoreSetter } from 'solid-js';
import type { Toast } from '../overlays/toast_state';
import { loadLastSeen } from '../rail/last_seen';
import type { PromptCompletion } from '../composer/prompts';
import { applySnapshot } from './snapshot';
import { applyEntriesPage, applyEntryDelta, applyEntryUpsert } from '../transcript/transcript_entries';
import { applySessions, applyTitle } from '../rail/session_rows';
import { applyDashMcpRows, applyDashPinRows, applyDashTasks, applySessionStats, clearJumpTarget } from '../dash/dash';
import {
  applyAppletRows,
  applyMcpRows,
  applyModelRows,
  applyPersonaRows,
  applyPickerRows,
  applySkillRows,
  applyToolRows,
} from '../pickers/picker_rows';
import { applyError } from '../overlays/toast_state';
import type {
  AppletRow,
  DashMcpRow,
  DashPinRow,
  DashTasks,
  EntryEnvelope,
  McpRow,
  ModelRow,
  PersonaRow,
  Phase,
  PickerRow,
  PromptTemplate,
  ServerMsg,
  SessionStats,
  SessionRow,
  SkillRow,
  Theme,
  ToolRow,
  ImageUpload,
  LifecycleOption,
} from '../protocol/protocol';

export interface ViewState {
  /** The instance's active session, or null before the first snapshot. */
  sessionId: string | null;
  phase: Phase;
  /** How many messages wait behind the running turn. */
  queueLen: number;
  modelLabel: string;
  title: string;
  /** The active session's persona, as the snapshot and a `context_usage`
   *  frame carry it. */
  persona: string;
  /** The context usage figure, as the server formats it: the terminal's own
   *  string. The page computes nothing; an empty string means no figure has
   *  arrived yet for this session. */
  contextLabel: string;
  /** True while the dash is open. View state the page owns; the server is
   *  told so its tick knows whether to push the statistics. */
  dashOpen: boolean;
  /** The dash's session statistics, as the last `session_stats` frame
   *  carried them; null until one arrives. */
  dashStats: SessionStats | null;
  /** The dash's pinned rows, in the sidebar's order and labels. */
  dashPins: DashPinRow[];
  /** The dash's task list; null until a `dash_tasks` frame arrives. */
  dashTasks: DashTasks | null;
  /** The dash's enabled MCP servers with their live status. */
  dashMcp: DashMcpRow[];
  /** The entry a pin tap asked the transcript to scroll to once it is
   *  held, or null. Cleared when the scroll happens, when the jump is
   *  refused, or by a snapshot. */
  jumpTarget: string | null;
  /** True while a pin's jump is under way: the page asked for history up to
   *  the entry and has not reached it. Raises the navigating overlay, which
   *  blocks the page until the jump lands, is refused, or is cancelled. */
  navigating: boolean;
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
  /** The viewed session's prompt templates, name and description — the
   * composer's `#` popup filters these in the browser. */
  promptTemplates: PromptTemplate[];
  /** The open `#` completion at the composer's caret, or null when the
   * caret sits in no token. */
  promptCompletion: PromptCompletion | null;
  /** Where the user moved the `#` popup's highlight with the arrow keys, as
   * an index into its filtered rows. Null means the default — the last row,
   * the best match — which is where it starts and where it returns on any
   * keystroke or list change. */
  promptHighlight: number | null;
  /** Transient notices, newest last. A toast dismisses itself, and a tap
   *  dismisses it at once. Not the session's: a snapshot leaves them. */
  toasts: Toast[];
  /** The session the page asked to activate and is still waiting on — the
   *  loading overlay's cause. Null when nothing is being waited for. */
  loadingSessionId: string | null;
  /** True once a snapshot or a resume has landed on this connection. */
  synced: boolean;
  /** True while the instance is still loading its saved sessions at
   *  startup. Set by every snapshot and every session-list frame, never by
   *  the page on its own. */
  hydrating: boolean;
  /** When the page last saw each session (RFC 3339), for the unread mark.
   *  A row is unread when its settle time is newer than its entry here. */
  lastSeen: Record<string, string>;
}

const INITIAL_STATE: ViewState = {
  sessionId: null,
  phase: 'Idle',
  queueLen: 0,
  modelLabel: '',
  title: '',
  persona: '',
  contextLabel: '',
  dashOpen: false,
  dashStats: null,
  dashPins: [],
  dashTasks: null,
  dashMcp: [],
  jumpTarget: null,
  navigating: false,
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
  promptTemplates: [],
  promptCompletion: null,
  promptHighlight: null,
  toasts: [],
  loadingSessionId: null,
  synced: false,
  hydrating: true,
  lastSeen: {},
};

/** The reactive store: read `state` as a plain object; write through `set`. */
export interface ClientStore {
  state: Store<ViewState>;
  set: StoreSetter<ViewState>;
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

/**
 * Applies one frame: the browser's single dispatch. Every `ServerMsg`
 * variant has exactly one arm and there is no fallback, so a new frame
 * kind is a type error until it is handled here.
 *
 * An arm with a body worth naming lives in its topic file and is a
 * one-line delegation; the arms that stay inline set one or two fields
 * and have no state of their own to carry.
 */
export function apply(store: ClientStore, msg: ServerMsg): void {
  switch (msg.type) {
    case 'snapshot': {
      applySnapshot(store, msg);
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
      applyEntriesPage(store, msg);
      return;
    }
    case 'entry_upsert': {
      applyEntryUpsert(store, msg);
      return;
    }
    case 'entry_delta': {
      applyEntryDelta(store, msg);
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
      applySessions(store, msg);
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
      applyTitle(store, msg);
      return;
    }
    case 'context_usage': {
      if (!belongsToViewedSession(store, msg.session_id)) return;
      store.set((s) => {
        s.persona = msg.persona;
        s.contextLabel = msg.label;
      });
      return;
    }
    case 'session_stats':
      applySessionStats(store, msg);
      return;
    case 'dash_pin_rows':
      applyDashPinRows(store, msg);
      return;
    case 'dash_tasks':
      applyDashTasks(store, msg);
      return;
    case 'dash_mcp_rows':
      applyDashMcpRows(store, msg);
      return;
    case 'picker_rows': {
      applyPickerRows(store, msg);
      return;
    }
    case 'model_rows': {
      applyModelRows(store, msg);
      return;
    }
    case 'skill_rows': {
      applySkillRows(store, msg);
      return;
    }
    case 'tool_rows': {
      applyToolRows(store, msg);
      return;
    }
    case 'mcp_rows': {
      applyMcpRows(store, msg);
      return;
    }
    case 'persona_rows': {
      applyPersonaRows(store, msg);
      return;
    }
    case 'applet_rows': {
      applyAppletRows(store, msg);
      return;
    }
    case 'prompt_templates': {
      // A frame for a session the page has left is not the list it shows.
      // The completion itself is left alone: an open popup re-filters
      // against the new list in place, and its highlight returns to the
      // best match of that new list.
      if (store.state.sessionId !== null && msg.session_id !== store.state.sessionId) return;
      store.set((s) => {
        s.promptTemplates = msg.templates;
        s.promptHighlight = null;
      });
      return;
    }
    case 'error': {
      applyError(store, msg);
      // A jump the server could not serve must not leave the transcript
      // waiting on an entry that will never arrive.
      clearJumpTarget(store);
      return;
    }
    case 'current': {
      // The answer to a `hello` that missed nothing. It carries no fact —
      // the transcript the page holds is already the server's — and its
      // arrival is the whole effect: the socket ends its handshake wait.
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
export function belongsToViewedSession(store: ClientStore, sessionId: string): boolean {
  const viewed = store.state.sessionId;
  return viewed === null || viewed === sessionId;
}
