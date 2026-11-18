//! The wire contract — the TypeScript mirror of the server's
//! `jinn-webui-msg` types.
//!
//! Every string here MUST equal the corresponding Rust constant in
//! `crates/slices/jinn-webui/src/contract.rs`, and the SSE event
//! names MUST equal `SseKind::event_name()` in `jinn-webui-msg`. Two
//! drift tests hold the line: the Rust side greps this file
//! (`contract_tests.rs`), and `wire.test.ts` checks the runtime
//! values against a fixture. A rename on either side fails a test
//! before any browser sees the breakage.
//!
//! There is no client state module and none may be added: the server
//! is authoritative, the DOM and the URL are the client's only
//! memory.

/** `/vendor/` — the prefix every vendored asset is served under. */
export const VENDOR_PREFIX = "/vendor/";

/** The page-root id — the element a session switch replaces. */
export const PAGE_ROOT_ID = "page-root";

/** The attribute on #page-root naming the viewed session's id. */
export const PAGE_SESSION_DATA_ATTR = "data-webui-session";

/** The sessions-list container id. */
export const SESSIONS_ID = "sessions";

/** The session-row class hook — what click delegation selects. */
export const SESSION_ROW_CLASS = "srow";

/** The attribute on each row carrying its session id. */
export const SESSION_ID_DATA_ATTR = "data-session-id";

/** The transcript scroll-wrap id. */
export const TRANSCRIPT_WRAP_ID = "transcript-wrap";

/** The transcript entry container id. */
export const TRANSCRIPT_ID = "transcript";

/** The attribute on #transcript carrying the inline cursor. */
export const TRANSCRIPT_CURSOR_DATA_ATTR = "data-webui-cursor";

/** The composer-controls container id. */
export const CONTROLS_ID = "composer-controls";

/** The composer form container id. */
export const COMPOSER_ID = "composer";

/** The message textarea id. */
export const INPUT_ID = "input";

/** The model-picker container id. */
export const MODEL_PICKER_ID = "model-picker";

/** The streaming spinner — present above the composer while busy. */
export const BUSY_SPINNER_ID = "busy-spinner";

/** The attribute naming an element's action (send/cancel/model). */
export const ACTION_DATA_ATTR = "data-webui-action";

/** Action names — the contract's action vocabulary. */
export const ACTION_SEND = "send";
export const ACTION_CANCEL = "cancel";
export const ACTION_MODEL = "model";

/** The SSE stream path for one session. */
export const STREAM_PATH = "/web/stream/";

/** Action endpoints — the URLs the api module posts to. */
export const SEND_PATH = "/web/send";
export const CANCEL_PATH = "/web/cancel";
export const MODEL_PATH = "/web/model";

/** One session's page path (also the activate endpoint with POST). */
export const SESSION_PAGE_PATH = "/web/session/";

/**
 * The SSE event names, mirrored from `SseKind::event_name()`.
 * Dispatch keys: the stream listeners are registered per name.
 */
export const SSE_EVENTS = {
  chat: "chat",
  controls: "controls",
  sessions: "sessions",
  transcriptFull: "transcript-full",
} as const;

/** One SSE event name — the values of {@link SSE_EVENTS}. */
export type SseEventName = (typeof SSE_EVENTS)[SseEventKey];

/** The keys of {@link SSE_EVENTS}. */
export type SseEventKey = keyof typeof SSE_EVENTS;
