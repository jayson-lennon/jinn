// The wire contract, mirrored from `jinn-webui2-msg`.
//
// Every field here is byte-for-byte what the Rust enums serialize. The
// shapes were read off real frames, not inferred: bodies are flattened
// into the tagged object (`{type, ...body}`), a phase is a bare PascalCase
// string (`"Working"`), and an entry carries its own kind name beside the
// core's persisted map so that every kind — transient included — survives
// the round trip.

export type Phase = 'Idle' | 'Streaming' | 'Working' | 'Cancelling';

export type EntryKind =
  | 'user'
  | 'system'
  | 'error'
  | 'assistant'
  | 'actor'
  | 'thinking'
  | 'tool_call'
  | 'tool_result'
  | 'transient'
  | 'compaction'
  | 'annotation'
  | 'rule_interrupt'
  | 'intersession';

/** One transcript entry: its kind name, and the core entry as persisted. */
export interface EntryEnvelope {
  kind: EntryKind;
  entry: {
    id: string;
    kind: Record<string, unknown>;
    timing?: unknown;
    pin_position?: unknown;
    context_override?: string;
    context_history?: unknown[];
    token_count?: number | null;
    model_used?: string | null;
    model_requested?: string | null;
  };
}

export interface SessionRow {
  session_id: string;
  title: string;
  is_active: boolean;
  is_busy: boolean;
}

export interface PickerRow {
  session_id: string;
  title: string;
  state: 'Loaded' | 'Archived';
  parent_id: string | null;
  project: string | null;
}

export interface ModelRow {
  provider_id: string;
  model: string;
  provider_name: string;
  is_available: boolean;
  is_active: boolean;
}

export type DeltaField = 'text' | 'thinking' | 'args' | 'output';

/** Browser -> server. Exactly one object per user gesture or connect. */
export type ClientMsg =
  | { type: 'hello'; last_entry_id: string | null; revision: number | null }
  | { type: 'send'; text: string }
  | { type: 'queue'; text: string }
  | { type: 'cancel' }
  | { type: 'activate_session'; session_id: string }
  | { type: 'switch_model'; provider_id: string }
  | { type: 'rename'; title: string }
  | { type: 'picker_rows'; filter: string }
  | { type: 'model_rows'; filter: string };

/** Server -> browser. Exactly one fact per frame. */
export type ServerMsg =
  | {
      type: 'snapshot';
      protocol: number;
      session_id: string | null;
      phase: { session_id: string; phase: Phase };
      model_label: string;
      title: string;
      revision: number;
      entries: EntryEnvelope[];
      sessions: SessionRow[];
    }
  | { type: 'entry_upsert'; session_id: string; index: number; entry: EntryEnvelope }
  | { type: 'entry_delta'; session_id: string; entry_id: string; field: DeltaField; chunk: string }
  | { type: 'phase'; session_id: string; phase: Phase }
  | { type: 'sessions'; rows: SessionRow[] }
  | { type: 'model'; session_id: string; label: string }
  | { type: 'title'; session_id: string; title: string }
  | { type: 'picker_rows'; filter: string; rows: PickerRow[] }
  | { type: 'model_rows'; filter: string; rows: ModelRow[] }
  | { type: 'error'; message: string }
  | { type: 'ping' };
