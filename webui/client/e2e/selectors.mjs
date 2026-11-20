//! Every selector the e2e specs use, derived from the wire contract.
//!
//! The contract's single source is the Rust `contract.rs`; these
//! strings are the ids the server's page template renders. A contract
//! rename therefore fails here as a visibly wrong selector, not as a
//! silently passing one.

/** The sessions-list container id (`wire.SESS​IONS_ID`). */
export const SESSIONS_ID = "#sessions";
/** The session-row class hook (`wire.SESSION_ROW_CLASS`). */
export const SESSION_ROW = ".srow";
/** The attribute on each row carrying its session id. */
export const SESSION_ID_ATTR = "data-session-id";
/** The working row's class hook and its spinner span
 * (`wire.WORKING_CLASS`) — the span renders ahead of the title while
 * the session's live phase is working. */
export const WORKING_ROW = ".srow.working";
/** The spinner span inside a working row. */
export const WORKING_SPINNER = ".srow.working .working";
/** One session row, addressed by its id attribute — the scratch db
 * accumulates sessions across the alphabetical run, so specs never
 * address rows by index or count. */
export const SESSION_ROW_BY_ID = (id) => `.srow[${SESSION_ID_ATTR}="${id}"]`;
/** The sidebar filter input id (`wire.SESSIONS_FILTER_INPUT_ID`). */
export const SESSIONS_FILTER = "#sessions-filter";
/** The page-root id (`wire.PAGE_ROOT_ID`). */
export const PAGE_ROOT = "#page-root";
/** The attribute on #page-root naming the viewed session. */
export const PAGE_SESSION_ATTR = "data-webui-session";
/** The transcript entry container id (`wire.TRANSCRIPT_ID`). */
export const TRANSCRIPT = "#transcript";
/** One transcript entry's article element and its kind class. */
export const ENTRY = (kind) => `article.entry.${kind}`;
/** The composer textarea id (`wire.INPUT_ID`). */
export const INPUT = "#input";
/** The composer send button (action = `wire.ACTION_SEND`). */
export const SEND_BUTTON = '[data-webui-action="send"]';
/** The composer cancel button (action = `wire.ACTION_CANCEL`). */
export const CANCEL_BUTTON = '[data-webui-action="cancel"]';
/** The streaming spinner id (`wire.BUSY_SPINNER_ID`). */
export const BUSY_SPINNER = "#busy-spinner";
/** The titlebar's `.m` model label span. */
export const MODEL_LABEL = "#model-status .m";
/** The model-picker button id (`wire.MODEL_PICKER_BUTTON_ID`). */
export const PICKER_BUTTON = "#model-picker-button";
/** The modal overlay id (`wire.MODEL_PICKER_MODAL_ID`). */
export const PICKER_MODAL = "#model-picker-modal";
/** The modal's filter input id (`wire.MODEL_PICKER_FILTER_ID`). */
export const PICKER_FILTER = "#model-picker-filter";
/** The modal's row container id (`wire.MODEL_PICKER_ROWS_ID`). */
export const PICKER_ROWS = "#model-picker-rows";
/** The modal's refresh button id (`wire.MODEL_PICKER_REFRESH_ID`). */
export const PICKER_REFRESH = "#model-picker-refresh";
/** One picker row, by its model id attr (`wire.MODEL_ID_DATA_ATTR`). */
export const PICKER_ROW = (model) => `[data-webui-model="${model}"]`;
/** The row's inner model-name span — the switch delegation's target. */
export const PICKER_ROW_MODEL_SPAN = (model) =>
  `[data-webui-model="${model}"] .model-row-model`;
/** The attribute an unavailable row carries. */
export const MODEL_UNAVAILABLE_ATTR = "data-webui-unavailable";
/** The session page path (`wire.SESSION_PAGE_PATH`). */
export const SESSION_PAGE_PATH = "/web/session/";

/** The titlebar's menu region host (`wire.MENU_ID`). */
export const MENU = "#menu";
/** The hamburger button id (`wire.MENU_BUTTON_ID`). */
export const MENU_BUTTON = "#menu-button";
/** The menu's item panel id (`wire.MENU_PANEL_ID`). */
export const MENU_PANEL = "#menu-panel";
/** The menu scrim id (`wire.MENU_SCRIM_ID`). */
export const MENU_SCRIM = "#menu-scrim";
/** The menu's sessions item id (`wire.MENU_ITEM_SESSIONS_ID`). */
export const MENU_ITEM_SESSIONS = "#menu-item-sessions";
/** The menu's new-session item id (`wire.MENU_ITEM_NEW_SESSION_ID`). */
export const MENU_ITEM_NEW_SESSION = "#menu-item-new-session";
/** The menu's persona item id (`wire.MENU_ITEM_PERSONA_ID`). */
export const MENU_ITEM_PERSONA = "#menu-item-persona";
/** The persona modal overlay id (`wire.PERSONA_MODAL_ID`). */
export const PERSONA_MODAL = "#persona-modal";
/** The persona modal's host span id (`wire.PERSONA_MODAL_HOST_ID`). */
export const PERSONA_MODAL_HOST = "#persona-modal-host";
/** The persona modal's filter input id (`wire.PERSONA_FILTER_ID`). */
export const PERSONA_FILTER = "#persona-filter";
/** The persona modal's row container id (`wire.PERSONA_ROWS_ID`). */
export const PERSONA_ROWS = "#persona-rows";
/** One persona row, by its persona name attr (`wire.PERSONA_NAME_DATA_ATTR`). */
export const PERSONA_ROW = (name) => `[data-webui-persona="${name}"]`;
/** The switch POST's endpoint (`wire.PERSONA_PATH`). */
export const PERSONA_PATH = "/web/persona";
/** The persona-picker pull's route prefix (`wire.PERSONA_PICKER_PREFIX`). */
export const PERSONA_PICKER_PREFIX = "/web/persona-picker/";
/**
 * The block-less scratch persona global-setup stages for the run —
 * the persona the warning-path spec pivots on.
 */
export const BLOCKLESS_PERSONA = "blockless-e2e";
/** The new-session modal overlay id (`wire.SESSION_NEW_MODAL_ID`). */
export const SESSION_NEW_MODAL = "#session-new-modal";
/** The new-session modal's host span id (`wire.SESSION_NEW_MODAL_HOST_ID`). */
export const SESSION_NEW_MODAL_HOST = "#session-new-modal-host";
/** The new-session project select id (`wire.SESSION_NEW_PROJECT_SELECT_ID`). */
export const SESSION_NEW_PROJECT = "#session-new-project";
/** The new-session lifecycle select id (`wire.SESSION_NEW_LIFECYCLE_SELECT_ID`). */
export const SESSION_NEW_LIFECYCLE = "#session-new-lifecycle";
/** The new-session args input id (`wire.SESSION_NEW_ARGS_INPUT_ID`). */
export const SESSION_NEW_ARGS = "#session-new-args";
/** The new-session submit button id (`wire.SESSION_NEW_SUBMIT_ID`). */
export const SESSION_NEW_SUBMIT = "#session-new-submit";
/** The new-session refusal wrapper id (`wire.SESSION_NEW_EMPTY_ID`). */
export const SESSION_NEW_EMPTY = "#session-new-empty";
/** The drawer scrim id (`wire.DRAWER_SCRIM_ID`). */
export const DRAWER_SCRIM = "#drawer-scrim";
/** The drawer-open class the client toggles on the page root. */
export const DRAWER_OPEN_CLASS = "drawer-open";

/**
 * The one model this tier's scratch config defines.
 *
 * Provider ids are `{name}/{model}`; the scratch providers.toml names
 * both `sample`, so the id is `sample/sample`.
 */
export const MODEL_ID = "sample/sample";
