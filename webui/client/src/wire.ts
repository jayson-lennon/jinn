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

/** The sidebar filter input's id — outside the #sessions swap target. */
export const SESSIONS_FILTER_INPUT_ID = "sessions-filter";

/** The sessions-list fragment pull (accepts `?q=<filter text>`). */
export const SESSIONS_ROUTE = "/web/sessions";

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

/** The model-picker container id — the **button's** host. */
export const MODEL_PICKER_ID = "model-picker";

/**
 * The model-picker **modal's** host id — an empty span sibling to the
 * picker container in the page shell; the modal fragment swaps into
 * it, so opening the modal never destroys the button that opened it.
 */
export const MODEL_PICKER_MODAL_HOST_ID = "model-picker-modal-host";

/**
 * The titlebar's model-status wrapper id — the static `.m` label and
 * the picker container rendered as one fragment; a switch re-renders
 * both in one swap.
 */
export const MODEL_STATUS_ID = "model-status";

/** The model-picker's titlebar button id — opens the modal. */
export const MODEL_PICKER_BUTTON_ID = "model-picker-button";

/**
 * The model-picker's modal overlay id — the server-rendered row list
 * the button opens; present in the page only after the pull renders
 * it in.
 */
export const MODEL_PICKER_MODAL_ID = "model-picker-modal";

/** The modal's filter input id — each keystroke re-queries the server. */
export const MODEL_PICKER_FILTER_ID = "model-picker-filter";

/** The modal's row container id — the list the filter re-renders into. */
export const MODEL_PICKER_ROWS_ID = "model-picker-rows";

/**
 * The modal's refresh button id — publishes `RefreshModels`; the rows
 * update when the cache lands via the `models` SSE event.
 */
export const MODEL_PICKER_REFRESH_ID = "model-picker-refresh";

/** The attribute a modal row carries its model id in. */
export const MODEL_ID_DATA_ATTR = "data-webui-model";

/**
 * The attribute a modal row carries its marker in (`✗` unavailable /
 * `→` alias / `*` remote) — present only when a marker applies.
 */
export const MODEL_MARKER_ATTR = "data-webui-marker";

/**
 * The attribute an unavailable row carries — the row is
 * non-interactive (the server also refuses the switch).
 */
export const MODEL_UNAVAILABLE_ATTR = "data-webui-unavailable";

/** The titlebar title span — the inline rename editor's host. */
export const SESSION_TITLE_ID = "session-title";

/** The titlebar's menu region — the hamburger button's host span. */
export const MENU_ID = "menu";

/** The menu's hamburger button id — opens the panel, all widths. */
export const MENU_BUTTON_ID = "menu-button";

/**
 * The menu's item panel id — the list the hamburger opens. The server
 * renders it `hidden`; the client toggles visibility.
 */
export const MENU_PANEL_ID = "menu-panel";

/** The menu's one item id — "Sessions"; closes the menu. */
export const MENU_ITEM_SESSIONS_ID = "menu-item-sessions";

/**
 * The drawer-open class the client toggles on #page-root — never
 * rendered by the server, so a page swap structurally resets the
 * drawer. Deliberately outside the generated fixture's DOM set (the
 * referential gate asserts fresh-render presence); parity with the
 * Rust constant is hand-maintained.
 */
export const DRAWER_OPEN_CLASS = "drawer-open";

/** The menu item's action name (`data-webui-action` value). */
export const ACTION_MENU_SESSIONS = "menu-sessions";

/** The streaming spinner — present above the composer while busy. */
export const BUSY_SPINNER_ID = "busy-spinner";

/** The attribute naming an element's action (send/cancel/model/refresh). */
export const ACTION_DATA_ATTR = "data-webui-action";

/** Action names — the contract's action vocabulary. */
export const ACTION_SEND = "send";
export const ACTION_CANCEL = "cancel";
export const ACTION_MODEL = "model";
export const ACTION_REFRESH_MODELS = "refresh-models";

/** The SSE stream path for one session. */
export const STREAM_PATH = "/web/stream/";

/** Action endpoints — the URLs the api module posts to. */
export const SEND_PATH = "/web/send";
export const CANCEL_PATH = "/web/cancel";
export const MODEL_PATH = "/web/model";
export const MODELS_REFRESH_PATH = "/web/models/refresh";

/**
 * The model-picker pull's prefix — the client builds picker URLs from
 * it (open, and one per filter keystroke, `?filter=…` appended).
 */
export const MODEL_PICKER_PREFIX = "/web/model-picker/";

/** The model-picker pull's filter query parameter. */
export const MODEL_FILTER_QUERY_PARAM = "filter";

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
  models: "models",
  transcriptFull: "transcript-full",
} as const;

/** One SSE event name — the values of {@link SSE_EVENTS}. */
export type SseEventName = (typeof SSE_EVENTS)[SseEventKey];

/** The keys of {@link SSE_EVENTS}. */
export type SseEventKey = keyof typeof SSE_EVENTS;
