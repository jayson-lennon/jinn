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
/** The transcript's scroll container id (`wire.TRANSCRIPT_WRAP_ID`). */
export const TRANSCRIPT_WRAP = "#transcript-wrap";
/** One transcript entry's article element and its kind class. */
export const ENTRY = (kind) => `article.entry.${kind}`;
/** The composer textarea id (`wire.INPUT_ID`). */
export const INPUT = "#input";
/** The composer container id (`wire.COMPOSER_ID`). */
export const COMPOSER = "#composer";
/** The composer send button (action = `wire.ACTION_SEND`). */
export const SEND_BUTTON = '[data-webui-action="send"]';
/** The composer cancel button (action = `wire.ACTION_CANCEL`). */
export const CANCEL_BUTTON = '[data-webui-action="cancel"]';
/** The composer queue button (action = `wire.ACTION_QUEUE`) — busy
 * fragment only, rendered before the cancel button. */
export const QUEUE_BUTTON = '[data-webui-action="queue"]';
/** The streaming spinner id (`wire.BUSY_SPINNER_ID`). */
export const BUSY_SPINNER = "#busy-spinner";
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
/** The menu's MCP item id (`wire.MENU_ITEM_MCP_ID`). */
export const MENU_ITEM_MCP = "#menu-item-mcp";
/** The menu's tools item id (`wire.MENU_ITEM_TOOLS_ID`). */
export const MENU_ITEM_TOOLS = "#menu-item-tools";
/** The MCP modal overlay id (`wire.MCP_MODAL_ID`). */
export const MCP_MODAL = "#mcp-modal";
/** The MCP modal's host span id (`wire.MCP_MODAL_HOST_ID`). */
export const MCP_MODAL_HOST = "#mcp-modal-host";
/** The MCP modal's row container id (`wire.MCP_ROWS_ID`). */
export const MCP_ROWS = "#mcp-rows";
/** One MCP server row, by its server-name attr (`wire.MCP_SERVER_DATA_ATTR`). */
export const MCP_ROW = (name) => `[data-webui-server="${name}"]`;
/** The enabled row class (`wire.MCP_ROW_ENABLED_CLASS`). */
export const MCP_ROW_ENABLED = "mcp-row-enabled";
/** The status-badge class hooks (`wire.MCP_STATUS_*`). */
export const MCP_BADGE_OFF = "mcp-status-off";
export const MCP_BADGE_STARTING = "mcp-status-starting";
export const MCP_BADGE_RUNNING = "mcp-status-running";
/** The toggle POST's endpoint (`wire.MCP_TOGGLE_PATH`). */
export const MCP_TOGGLE_PATH = "/web/mcp";
/** The MCP modal pull's route prefix (`wire.MCP_PICKER_PREFIX`). */
export const MCP_PICKER_PREFIX = "/web/mcp-servers/";
/** The one scratch MCP server the tier's jinn.toml configures. */
export const MCP_SERVER = "echo";
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
/** The tools modal overlay id (`wire.TOOLS_MODAL_ID`). */
export const TOOLS_MODAL = "#tools-modal";
/** The tools modal's host span id (`wire.TOOLS_MODAL_HOST_ID`). */
export const TOOLS_MODAL_HOST = "#tools-modal-host";
/** The tools modal's row container id (`wire.TOOLS_ROWS_ID`). */
export const TOOLS_ROWS = "#tools-rows";
/** One tool row, by its tool-name attr (`wire.TOOL_NAME_DATA_ATTR`). */
export const TOOL_ROW = (name) => `[data-webui-tool="${name}"]`;
/** The off-row class (`tools-row-off`) — a row the session's live
 * filter withholds. */
export const TOOL_ROW_OFF = "tools-row-off";
/** The tools modal's rows-only pull prefix (`wire.TOOLS_ROWS_PREFIX`). */
export const TOOLS_ROWS_PREFIX = "/web/tools/rows/";
/** The toggle POST's endpoint (`wire.TOOLS_TOGGLE_PATH`). */
export const TOOLS_TOGGLE_PATH = "/web/tools";
/** The revert POST's endpoint (`wire.TOOLS_REVERT_PATH`). */
export const TOOLS_REVERT_PATH = "/web/tools/revert";
/** The tools-picker pull's route prefix (`wire.TOOLS_PICKER_PREFIX`). */
export const TOOLS_PICKER_PREFIX = "/web/tools/";
/**
 * The one builtin tool the tools spec pivots on — present in every
 * run's registry regardless of MCP state (the scratch run may carry
 * echo-server tools from the MCP spec running earlier).
 */
export const TOOLS_SPEC_TOOL = "get_time";
/**
 * The block-less scratch persona global-setup stages for the run —
 * the persona the warning-path spec pivots on.
 */
export const BLOCKLESS_PERSONA = "blockless-e2e";
/** The skills modal overlay id (`wire.SKILL_MODAL_ID`). */
export const SKILL_MODAL = "#skills-modal";
/** The skills modal's host span id (`wire.SKILL_MODAL_HOST_ID`). */
export const SKILL_MODAL_HOST = "#skills-modal-host";
/** The skills modal's row container id (`wire.SKILL_ROWS_ID`). */
export const SKILL_ROWS = "#skills-rows";
/** One skill row, by its skill-name attr (`wire.SKILL_NAME_DATA_ATTR`). */
export const SKILL_ROW = (name) => `[data-webui-skill="${name}"]`;
/** The enabled row class hook (the skills modal's TUI-parity mark). */
export const SKILL_ROW_ENABLED = "skill-row-enabled";
/** The row's force-load button class hook (`wire.SKILL_LOAD_CLASS`). */
export const SKILL_LOAD_BUTTON = ".skill-row-load";
/** The skills menu item id (`wire.MENU_ITEM_SKILLS_ID`). */
export const MENU_ITEM_SKILLS = "#menu-item-skills";
/** The toggle POST's endpoint (`wire.SKILL_TOGGLE_PATH`). */
export const SKILL_TOGGLE_PATH = "/web/skills/toggle";
/** The load POST's endpoint (`wire.SKILL_LOAD_PATH`). */
export const SKILL_LOAD_PATH = "/web/skills/load";
/** The skills modal pull's route prefix (`wire.SKILL_PICKER_PREFIX`). */
export const SKILL_PICKER_PREFIX = "/web/skills/";
/**
 * The scratch skill global-setup stages for the run — the row every
 * skills spec pivots on (discovered from `<root>/.agents/skills`,
 * so it renders with no source badge and, on the boot session's
 * absent filter, enabled).
 */
export const SKILL = "e2e-scratch-skill";
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
