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

/** The core entry's persisted kind map, one member per `ChatEntryKind`
 *  variant, keyed by the snake_case kind name the envelope carries.
 *
 *  Five variants are a bare string — `System`, `Error`, `Assistant`,
 *  `Thinking`, `Transient` — and their body IS the text. The rest are
 *  structs, and their text is one named field of the body. That difference
 *  is the whole reason this type exists: typed as `unknown`, a consumer
 *  cannot tell a string variant from a struct one, and the delta handler
 *  once read `Assistant` as `{ text }` and silently dropped every chunk.
 *
 *  A struct's fields are the ones the page reads, transcribed from the Rust
 *  enum; the rest of its fields stay `unknown`. This is mirrored by hand,
 *  like the rest of this file, so a variant changed on the server is a
 *  change here. */
export type EntryKindMap = {
  user: { User: { display: string; expanded: string } };
  system: { System: string };
  error: { Error: string };
  assistant: { Assistant: string };
  actor: { Actor: { source: string; text: string } };
  thinking: { Thinking: string };
  tool_call: { ToolCall: { id: string; name: string; arguments: string } };
  tool_result: {
    ToolResult: {
      id: string;
      name: string;
      content: string;
      status: 'Pending' | 'Success' | 'Failure';
      truncation?: { truncated_by: string; total_lines: number; total_bytes: number };
    };
  };
  transient: { Transient: string };
  compaction: {
    Compaction: {
      summary: string;
      tokens_before: number;
      tokens_after: number;
      entries_compacted: number;
      model_used: string;
    };
  };
  annotation: { Annotation: { citations: { url: string; title: string }[] } };
  rule_interrupt: { RuleInterrupt: { rule: string; body: string } };
  intersession: {
    Intersession: {
      source_session: string;
      source_name: string | null;
      target_session: string;
      text: string;
    };
  };
};

/** The body of one entry kind: the text itself for a string variant, the
 *  struct for a struct variant. */
export type EntryKindBody<K extends EntryKind = EntryKind> = EntryKindMap[K][keyof EntryKindMap[K]];

/** A persisted kind map whose kind is not yet known: one of the thirteen
 *  single-key maps, and only ever one. This is the wire's own shape when a
 *  frame is still untyped, and what a map built at runtime is typed as. */
export type AnyEntryKindMap = EntryKindMap[keyof EntryKindMap];

/** One transcript entry: its kind name, and the core entry as persisted —
 * minus two fields the page never reads from the socket. A tool result's
 * `full_content` is fetched from `/tool-output/...` when its disclosure
 * opens (its `truncation` meta stays, as the marker that there is one), and
 * an attachment's `data` bytes are fetched from `/attachment/...`. */
export interface EntryEnvelope<K extends EntryKind = EntryKind> {
  kind: K;
  entry: {
    id: string;
    kind: EntryKindMap[K];
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
  /** Whether the session's teardown command is running. Independent of
   *  `is_busy`: a teardown does not change the session's phase. */
  is_tearing_down: boolean;
  /** Whether the session is a library member (listed under Library). */
  in_library: boolean;
  /** When the session last landed on Idle, as RFC 3339 — or null when it
   *  has not settled since the server started, or is a subagent's. */
  last_settled_at: string | null;
  /** The session's persisted direct parent, when it was forked or spawned
   *  under one. */
  parent_session: string | null;
  /** What kind of session this is. A subagent or a bot is a conversation
   *  the user did not type into, and the page names it in two places. */
  origin: SessionOrigin;
  /** The row's depth in the Sessions list's tree: 0 for a root, one more
   *  per level under a listed parent. Always 0 for a library row — that
   *  list is flat. An older server omits it, so read it as `?? 0`. */
  depth: number;
}

/** The kind of session a row is, as the core's origin spells it. */
export type SessionOrigin = 'user' | 'fork' | 'subagent' | 'bot';

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

/** The view the market modal asks its rows in: the filter, the sort
 * column and direction, and which token mix PROJ projects under. Every
 * rows frame echoes the view it answers, so the page can drop a frame
 * for a view it has typed past. */
export interface MarketView {
  filter: string;
  sort: MarketSort;
  dir: MarketDir;
  mix: MarketMix;
}

/** A sortable market column. */
export type MarketSort = 'proj' | 'in' | 'out' | 'cache' | 'eci' | 'vals' | 'model';

/** A sort direction. */
export type MarketDir = 'asc' | 'desc';

/** Which detail column the endpoint rows are sorted by. */
export type DetailSort = 'endpoint' | 'proj' | 'in' | 'out' | 'cache' | 'lat' | 'tp' | 'up';

/** The detail page's sort. The Default row is pinned to the top in
 * every sort. */
export interface DetailView {
  sort: DetailSort;
  dir: MarketDir;
}

/** Which token mix PROJ projects under: the shipped constant, or the
 * session tree's measured usage. */
export type MarketMix = 'fixed' | 'usage';

/** One row of the model market, every cell pre-formatted by the server —
 * the same strings the terminal's table renders. */
export interface MarketRow {
  id: string;
  author: string;
  slug: string;
  variant: string | null;
  /** The name's base (`author/slug`), split into matched and plain runs. */
  name_runs: TextRun[];
  /** The `:variant` suffix as its own runs, so the page can dim it.
   * Empty when no variant. */
  variant_runs: TextRun[];
  proj: string;
  in_m: string;
  out_m: string;
  cache_m: string;
  eci: string;
  vals: string;
  /** Whether this is the active session's model. */
  is_active: boolean;
}

/** One endpoint of a model's detail: the auto-route sentinel, or a
 * provider's offer. */
export interface MarketEndpoint {
  tag: string | null;
  name: string;
  proj: string;
  in_m: string;
  out_m: string;
  cache_m: string;
  latency: string;
  throughput: string;
  uptime: string;
  is_default: boolean;
}

/** One prompt template, as the composer's `#` popup lists it. The body
 * never crosses the wire: it reaches the model through the `#name`
 * expansion at send time. */
export interface PromptTemplate {
  name: string;
  description: string;
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

/** A task's status, in the task vocabulary's own wire words. */
export type TaskStatus = 'Pending' | 'Completed' | 'Deferred' | 'Cancelled';

/** The whole session tree's totals; sent only while the tree has more than one session. */
export interface TreeStats {
  session_count: number;
  cache_hit_percent: number | null;
  tokens_sent: string;
  tokens_received: string;
  cost: string;
  turn_count: number;
  working: string;
}

/** Every figure the terminal status bar shows for the session, pre-formatted.
 *  The wire frame adds `session_id`; the page keeps only the figures. */
export interface SessionStats {
  cwd: string;
  cache_hit_percent: number | null;
  tokens_sent: string;
  tokens_received: string;
  context_label: string;
  cost: string;
  turn_count: number;
  working: string;
  model: string;
  tree: TreeStats | null;
}

/** One pinned entry, labeled as the terminal sidebar labels it. */
export interface DashPinRow {
  entry_id: string;
  text: string;
}

export interface DashTaskRow {
  description: string;
  status: TaskStatus;
}

export interface DashTaskPhase {
  description: string;
  tasks: DashTaskRow[];
  is_active: boolean;
  has_pending_work: boolean;
}

/** Every phase with the tasks beneath it. The wire frame adds `session_id`. */
export interface DashTasks {
  phases: DashTaskPhase[];
  done: number;
  total: number;
}

export interface DashMcpRow {
  name: string;
  badge: McpBadge;
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
  | { type: 'market_open'; view: MarketView }
  | { type: 'market_rows'; view: MarketView }
  | { type: 'market_detail'; model_id: string; view: DetailView }
  | { type: 'market_use'; model_id: string; tag: string | null }
  | { type: 'market_refresh' }
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
  | { type: 'dash_open' }
  | { type: 'dash_close' }
  | { type: 'jump_to_entry'; entry_id: string }
  | { type: 'cancel_jump' }
  | { type: 'applet_rows'; filter: string }
  | { type: 'refresh_models' }
  | { type: 'set_theme'; theme: Theme }
  | { type: 'set_library'; session_id: string; in_library: boolean }
  | { type: 'archive_session'; session_id: string }
  | { type: 'teardown_session'; session_id: string }
  | { type: 'new_session_options' }
  | {
      type: 'create_session';
      project: string;
      lifecycle: string;
      args: string;
    }
  /** Register or refresh this browser's push subscription. Sent on every
   *  load, so a row that went stale under another key or another origin is
   *  replaced by the one that works now. */
  | { type: 'push_subscribe'; endpoint: string; keys: { p256dh: string; auth: string } }
  | { type: 'push_unsubscribe'; endpoint: string }
  /** A notification was tapped: activate the session it names. */
  | { type: 'notification_opened'; session_id: string };

/** Server -> browser. Exactly one fact per frame. */
export type ServerMsg =
  | {
      type: 'snapshot';
      protocol: number;
      session_id: string | null;
      phase: { session_id: string; phase: Phase; queue_len: number };
      model_label: string;
      title: string;
      /** The session's persona, as the status bar shows it. Empty when no
       *  session is active. */
      persona: string;
      /** The active session's context usage figure, the terminal's own
       *  (empty when no session is active). The snapshot carries it so the
       *  status bar is whole on arrival; the tick pushes it only on change. */
      context_label: string;
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
      /** The active session's prompt templates, in the store's own order —
       * what the composer's `#` popup filters as the user types. */
      prompt_templates: PromptTemplate[];
    }
  | { type: 'entry_upsert'; session_id: string; index: number; entry: EntryEnvelope }
  | {
      type: 'entries_page';
      session_id: string;
      entries: EntryEnvelope[];
      /** How many entries precede this page's oldest. */
      entries_before: number;
    }
  | {
      type: 'entry_delta';
      session_id: string;
      entry_id: string;
      field: DeltaField;
      /** Where `chunk` starts, as a byte offset into the field's UTF-8 text.
       *  The page applies a chunk by this offset rather than appending it,
       *  because a whole-entry upsert can deliver the same words first. */
      offset_bytes: number;
      chunk: string;
    }
  | { type: 'phase'; session_id: string; phase: Phase; queue_len: number }
  | { type: 'sessions'; rows: SessionRow[]; library: SessionRow[]; hydrating: boolean }
  | { type: 'model'; session_id: string; label: string }
  | { type: 'title'; session_id: string; title: string }
  /** The status bar's two facts, pushed when either changed. The figure is
   *  the server's own formatted string — the terminal's — so the page
   *  renders it and computes nothing. */
  | { type: 'context_usage'; session_id: string; persona: string; label: string }
  | ({ type: 'session_stats'; session_id: string } & SessionStats)
  | { type: 'dash_pin_rows'; session_id: string; rows: DashPinRow[] }
  | ({ type: 'dash_tasks'; session_id: string } & DashTasks)
  | { type: 'dash_mcp_rows'; session_id: string; rows: DashMcpRow[] }
  | { type: 'picker_rows'; filter: string; rows: PickerRow[] }
  | { type: 'model_rows'; session_id: string; filter: string; rows: ModelRow[] }
  | {
      type: 'market_rows';
      session_id: string;
      view: MarketView;
      rows: MarketRow[];
      total: number;
      loading: boolean;
      updated: string | null;
      mix_label: string;
      active_id: string | null;
      mix_degraded: boolean;
    }
  | {
      type: 'market_detail';
      session_id: string;
      model_id: string;
      view: DetailView;
      context_length: number | null;
      is_free: boolean;
      has_overrides: boolean;
      endpoints: MarketEndpoint[];
      loading: boolean;
      pinned_tag: string | null;
    }
  | { type: 'market_stale'; message: string }
  | { type: 'skill_rows'; session_id: string; filter: string; rows: SkillRow[] }
  | { type: 'tool_rows'; session_id: string; filter: string; rows: ToolRow[] }
  | { type: 'mcp_rows'; session_id: string; filter: string; rows: McpRow[] }
  | { type: 'persona_rows'; session_id: string; filter: string; rows: PersonaRow[] }
  | { type: 'applet_rows'; filter: string; serving: boolean; bind: string; rows: AppletRow[] }
  | { type: 'prompt_templates'; session_id: string; templates: PromptTemplate[] }
  | { type: 'new_session_options'; projects: string[]; lifecycles: LifecycleOption[] }
  | { type: 'theme'; theme: Theme }
  /** What the page needs to subscribe to push: whether the server can send
   *  pushes, and the key it signs them with. Arrives in the `hello` reply.
   *  A null key means the server holds none — notifications are then
   *  unavailable for a reason other than the page's scheme. */
  | { type: 'push_info'; supported: boolean; public_key: string | null }
  | { type: 'error'; message: string }
  | { type: 'current' }
  | { type: 'ping' };
