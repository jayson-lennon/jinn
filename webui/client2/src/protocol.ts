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

/** One transcript entry: its kind name, and the core entry as persisted —
 * minus two fields the page never reads from the socket. A tool result's
 * `full_content` is fetched from `/web2/tool-output/...` when its disclosure
 * opens (its `truncation` meta stays, as the marker that there is one), and
 * an attachment's `data` bytes are fetched from `/web2/attachment/...`. */
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
  /** Whether the entry is in the model's context right now — the server's
   * `is_in_context`, pin and override included. The page reads it and never
   * re-derives it; a frame from before the field existed carries none and
   * reads as in context. */
  in_context?: boolean;
}

export interface SessionRow {
  session_id: string;
  title: string;
  is_active: boolean;
  is_busy: boolean;
  /** Whether the session is a library member (listed under Library). */
  in_library: boolean;
  /** When the session last landed on Idle, as RFC 3339 — or null when it
   *  has not settled since the server started, or is a subagent's. */
  last_settled_at: string | null;
}

/** The page's color mode. */
export type Theme = 'light' | 'dark' | 'system';

/** One pending composer image, as base64 bytes. */
export interface ImageUpload {
  name: string;
  data_base64: string;
}

/** One lifecycle the new-session form can list. */
export interface LifecycleOption {
  name: string;
  /** How many arguments its setup requires; 0 hides the arguments field. */
  param_count: number;
  /** Each argument as the setup's template spells it, in order. */
  params: string[];
}

export interface PickerRow {
  session_id: string;
  title: string;
  state: 'loaded' | 'archived';
  parent_id: string | null;
  project: string | null;
  /** The row's depth in the tree: 0 for a root, one more per level under a parent. */
  depth: number;
  /** When the session was last modified, as an ISO timestamp. */
  updated_at: string;
  /** The title, split into matched and plain runs for the filter. */
  title_runs: TextRun[];
}

export interface ModelRow {
  provider_id: string;
  model: string;
  provider_name: string;
  is_available: boolean;
  is_active: boolean;
  /** The model name, split into matched and plain runs for the filter. */
  model_runs: TextRun[];
  /** The provider name, split the same way. */
  provider_runs: TextRun[];
}

/** One run of a skill row's text: matched by the filter, or plain. */
export interface TextRun {
  text: string;
  matched: boolean;
}

/** One row of the skills modal. */
export interface SkillRow {
  name: string;
  description: string;
  /** Whether the skill passes the active session's skill filter. */
  enabled: boolean;
  /** Whether the skill's body is already pinned in the session's context. */
  loaded: boolean;
  /** Whether the skill was discovered from a project-local directory. */
  is_project: boolean;
  /** The name, split into matched and plain runs for the filter. */
  name_runs: TextRun[];
  /** The description, split the same way. */
  description_runs: TextRun[];
}

/** One row of the tools modal. */
export interface ToolRow {
  name: string;
  /** The tool description's first sentence. */
  description: string;
  /** Whether the session's tool filter currently permits the tool. */
  enabled: boolean;
  name_runs: TextRun[];
  description_runs: TextRun[];
}

/** The MCP server row's status badge, as the server derives it. */
export type McpBadge = 'off' | 'starting' | 'running' | 'dead';

/** One row of the MCP servers modal. */
export interface McpRow {
  name: string;
  /** The launch summary (command and args). */
  description: string;
  /** Whether the session has this server enabled. */
  enabled: boolean;
  /** The status badge the row shows. */
  badge: McpBadge;
  name_runs: TextRun[];
  description_runs: TextRun[];
}

/** One row of the persona modal. */
export interface PersonaRow {
  name: string;
  description: string;
  /** Whether this row is the session's current persona. */
  is_active: boolean;
  name_runs: TextRun[];
  description_runs: TextRun[];
}

/** One row of the applets modal. */
export interface AppletRow {
  /** The applet's directory id — its URL path segment. */
  id: string;
  title: string;
  /** The title, split into matched and plain runs for the filter. */
  title_runs: TextRun[];
  /** Where the row opens while the applets server is serving; `null` for a
   *  disabled row (offline, or an id the validity gate refused). */
  href: string | null;
}

export type DeltaField = 'text' | 'thinking' | 'args' | 'output';

/** Browser -> server. Exactly one object per user gesture or connect. */
export type ClientMsg =
  | { type: 'hello'; last_entry_id: string | null; revision: number | null }
  | { type: 'send'; text: string; images: ImageUpload[] }
  | { type: 'queue'; text: string; images: ImageUpload[] }
  | { type: 'cancel' }
  | { type: 'activate_session'; session_id: string }
  | { type: 'switch_model'; provider_id: string }
  | { type: 'rename'; title: string }
  | { type: 'picker_rows'; filter: string }
  | { type: 'model_rows'; filter: string }
  | { type: 'skill_rows'; filter: string }
  | { type: 'toggle_skill'; skill: string }
  | { type: 'load_skill'; skill: string }
  | { type: 'tool_rows'; filter: string }
  | { type: 'toggle_tool'; tool: string }
  | { type: 'mcp_rows'; filter: string }
  | { type: 'toggle_mcp'; server: string }
  | { type: 'persona_rows'; filter: string }
  | { type: 'switch_persona'; persona: string }
  | { type: 'toggle_pin'; entry_id: string }
  | { type: 'toggle_context'; entry_id: string }
  | { type: 'fork'; entry_id: string }
  | { type: 'more_entries'; before: string }
  | { type: 'applet_rows'; filter: string }
  | { type: 'refresh_models' }
  | { type: 'set_theme'; theme: Theme }
  | { type: 'set_library'; session_id: string; in_library: boolean }
  | { type: 'archive_session'; session_id: string }
  | { type: 'new_session_options' }
  | {
      type: 'create_session';
      project: string;
      lifecycle: string;
      args: string;
    };

/** Server -> browser. Exactly one fact per frame. */
export type ServerMsg =
  | {
      type: 'snapshot';
      protocol: number;
      session_id: string | null;
      phase: { session_id: string; phase: Phase; queue_len: number };
      model_label: string;
      title: string;
      revision: number;
      entries: EntryEnvelope[];
      /** How many entries of the session precede the oldest one carried. */
      entries_before: number;
      sessions: SessionRow[];
      library: SessionRow[];
      /** True while the instance is still loading its saved sessions at
       *  startup, so the rows above are a partial list. */
      hydrating: boolean;
      theme: Theme;
    }
  | { type: 'entry_upsert'; session_id: string; index: number; entry: EntryEnvelope }
  | {
      type: 'entries_page';
      session_id: string;
      entries: EntryEnvelope[];
      /** How many entries precede this page's oldest. */
      entries_before: number;
    }
  | { type: 'entry_delta'; session_id: string; entry_id: string; field: DeltaField; chunk: string }
  | { type: 'phase'; session_id: string; phase: Phase; queue_len: number }
  | { type: 'sessions'; rows: SessionRow[]; library: SessionRow[]; hydrating: boolean }
  | { type: 'model'; session_id: string; label: string }
  | { type: 'title'; session_id: string; title: string }
  | { type: 'picker_rows'; filter: string; rows: PickerRow[] }
  | { type: 'model_rows'; session_id: string; filter: string; rows: ModelRow[] }
  | { type: 'skill_rows'; session_id: string; filter: string; rows: SkillRow[] }
  | { type: 'tool_rows'; session_id: string; filter: string; rows: ToolRow[] }
  | { type: 'mcp_rows'; session_id: string; filter: string; rows: McpRow[] }
  | { type: 'persona_rows'; session_id: string; filter: string; rows: PersonaRow[] }
  | { type: 'applet_rows'; filter: string; serving: boolean; bind: string; rows: AppletRow[] }
  | { type: 'new_session_options'; projects: string[]; lifecycles: LifecycleOption[] }
  | { type: 'theme'; theme: Theme }
  | { type: 'error'; message: string }
  | { type: 'ping' };
