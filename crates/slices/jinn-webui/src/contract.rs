//! The web client contract — the single source both sides derive from.
//!
//! Every element id, data attribute, class hook, and endpoint path the
//! browser runtime targets lives here as a `&'static str` constant. The
//! askama templates render these values (threaded through the template
//! models' `Default` impls), the router registers its routes from them,
//! and the declarative Datastar attributes in those templates carry the
//! ids to the browser — there is no client source tree and no mirrored
//! constants file; the template interpolation is the enforcement (a
//! renamed constant fails the template at compile time, not a drifted
//! string match at test time). [`contract_tests`] holds the
//! referential-integrity gate that every DOM-targeting constant
//! appears in rendered HTML.
//!
//! The SSE event names are **not** duplicated here — they are
//! [`SseKind::event_name`] in `jinn-webui-msg`; the stream endpoint
//! translates envelopes into Datastar events, whose wire names the SDK
//! owns.

/// The page shell's root — the element a session switch replaces.
pub const PAGE_ROOT_ID: &str = "page-root";

/// The sessions-list container inside the sidebar.
pub const SESSIONS_ID: &str = "sessions";

/// The sessions filter input — lives in the page shell OUTSIDE the
/// `#sessions` swap target, so a sessions SSE push cannot wipe the
/// typed filter; a push arriving while a filter is active triggers a
/// re-pull carrying the same text.
pub const SESSIONS_FILTER_INPUT_ID: &str = "sessions-filter";

/// The scrollable transcript region (the wrap owns scrolling).
pub const TRANSCRIPT_WRAP_ID: &str = "transcript-wrap";

/// The transcript's entry container — chat appends here.
pub const TRANSCRIPT_ID: &str = "transcript";

/// The transcript's **streaming container** — the in-flight turn's
/// rendered tail swaps here. The last child of [`TRANSCRIPT_ID`], so
/// settled entries (above it) and streamed entries (inside it) never
/// interleave; the `transcript-full` inner replace clears it
/// structurally at every settle, because the full fragment carries the
/// empty container as its own last child.
pub const STREAMING_ID: &str = "streaming";

/// The composer-controls container — send↔cancel swaps here.
pub const CONTROLS_ID: &str = "composer-controls";

/// The model-picker container in the titlebar — the **button's** host.
/// The button fragment swaps here; the modal renders into its own
/// sibling host ([`MODEL_PICKER_MODAL_HOST_ID`]) so opening the modal
/// can never destroy the button that opened it.
pub const MODEL_PICKER_ID: &str = "model-picker";

/// The model-picker **modal's** host — an empty span sibling to
/// [`MODEL_PICKER_ID`] in the page shell. The modal fragment swaps into
/// this host on open and on every filter re-pull, leaving the button
/// and the titlebar untouched.
pub const MODEL_PICKER_MODAL_HOST_ID: &str = "model-picker-modal-host";

/// The same host spelled as a CSS selector — the swap-target header the
/// fragment pulls stamp (`Datastar-Selector`) and the Datastar bindings
/// in the templates name.
pub const MODEL_PICKER_MODAL_HOST_SELECTOR: &str = "#model-picker-modal-host";

/// The titlebar's model-picker button — the control that opens the
/// modal; its label is the viewed session's current model.
pub const MODEL_PICKER_BUTTON_ID: &str = "model-picker-button";

/// The model-picker's modal overlay — the server-rendered row list the
/// button opens; present in the page only after the pull renders it in.
pub const MODEL_PICKER_MODAL_ID: &str = "model-picker-modal";

/// The modal's filter input — each keystroke re-queries the server.
pub const MODEL_PICKER_FILTER_ID: &str = "model-picker-filter";

/// The modal's row container — the list the filter re-renders into.
pub const MODEL_PICKER_ROWS_ID: &str = "model-picker-rows";

/// The modal's refresh button — publishes `RefreshModels`; the rows
/// update when the cache lands via the `models` SSE event.
pub const MODEL_PICKER_REFRESH_ID: &str = "model-picker-refresh";

/// The attribute a modal row carries its model id in.
pub const MODEL_ID_DATA_ATTR: &str = "data-webui-model";

/// The attribute a modal row carries its marker in (`✗` unavailable /
/// `→` alias / `*` remote) — present only when a marker applies.
pub const MODEL_MARKER_ATTR: &str = "data-webui-marker";

/// The attribute an unavailable row carries — the client must treat it
/// as non-interactive (the server also refuses the switch).
pub const MODEL_UNAVAILABLE_ATTR: &str = "data-webui-unavailable";

/// The streaming spinner — rendered above the composer while the
/// session's turn is live; its absence is the idle signal.
pub const BUSY_SPINNER_ID: &str = "busy-spinner";

/// The composer form's container id (inside the controls fragment).
pub const COMPOSER_ID: &str = "composer";

/// The message textarea id.
pub const INPUT_ID: &str = "input";

/// The attribute a session row carries its id in.
pub const SESSION_ID_DATA_ATTR: &str = "data-session-id";

/// The attribute the page root carries the viewed session's id in
/// (empty on the landing page).
pub const PAGE_SESSION_DATA_ATTR: &str = "data-webui-session";

/// The attribute the transcript container carries its rendered-entry
/// cursor in — the `since` a fresh stream connection starts from, so
/// the inline render is never re-appended by catch-up.
pub const TRANSCRIPT_CURSOR_DATA_ATTR: &str = "data-webui-cursor";

/// The attribute a composer form names its action with (`send` /
/// `cancel`) — the client's submit delegation reads it.
pub const ACTION_DATA_ATTR: &str = "data-webui-action";

/// The send form's action name.
pub const ACTION_SEND: &str = "send";

/// The cancel form's action name.
pub const ACTION_CANCEL: &str = "cancel";

/// The queue form's action name — the composer's queue button, shown
/// beside cancel while the session is working. It posts the send
/// endpoint verbatim (the session actor queues a busy-phase enqueue),
/// so it is a value of [`ACTION_DATA_ATTR`], not a new route.
pub const ACTION_QUEUE: &str = "queue";

/// The model form's action name.
pub const ACTION_MODEL: &str = "model";

/// The refresh-models action's name (the modal's refresh button).
pub const ACTION_REFRESH_MODELS: &str = "refresh-models";

/// The class hook every session row carries.
pub const SESSION_ROW_CLASS: &str = "srow";

/// The working row's class hook, and the class of the spinner span it
/// renders ahead of the title — one hook for both, the span living
/// inside the row. Present only while the session's live phase is
/// working (streaming, tools, cancelling); its absence is the idle
/// signal.
pub const WORKING_CLASS: &str = "working";

/// The titlebar's title span — the inline rename editor's host. The
/// session page renders it with the viewed title; clicking it opens
/// the editor (the client swaps an input inside the span). Absent
/// content-wise on the landing page, whose "jinn" title is inert.
pub const SESSION_TITLE_ID: &str = "session-title";

/// The titlebar's menu region — the hamburger button's host span,
/// rendered left of the session title. The panel renders inside this
/// host too (a static fragment, like the model-picker button's host),
/// so the region owns one container on the page.
pub const MENU_ID: &str = "menu";

/// The menu's hamburger button — the titlebar control that opens the
/// panel. Visible at every viewport width.
pub const MENU_BUTTON_ID: &str = "menu-button";

/// The menu's item panel — the slide-in launcher list the hamburger
/// opens (full-height, from the left edge, over its own scrim). The
/// server renders it with the `hidden` attribute; the client toggles
/// visibility (the model-picker modal's lifecycle).
pub const MENU_PANEL_ID: &str = "menu-panel";

/// The menu's one item — closes the menu; below 768px it then opens
/// the session drawer (the class on the page root decides that).
pub const MENU_ITEM_SESSIONS_ID: &str = "menu-item-sessions";

/// The menu's new-session item — closes the menu and pulls the
/// server-rendered creation form into the modal host (the same
/// dismiss-then-open behavior the sessions item shows).
pub const MENU_ITEM_NEW_SESSION_ID: &str = "menu-item-new-session";

/// The new-session menu item's action name.
pub const ACTION_MENU_NEW_SESSION: &str = "menu-new-session";

/// The menu's persona item — closes the menu and pulls the persona
/// modal into its host for the viewed session. Rendered only when a
/// session is viewed: persona binds per-session and the landing page
/// views nothing (server-omitted, never client-hidden).
pub const MENU_ITEM_PERSONA_ID: &str = "menu-item-persona";

/// The persona menu item's action name.
pub const ACTION_MENU_PERSONA: &str = "menu-persona";

/// The new-session form submit's action name.
pub const ACTION_SESSION_CREATE: &str = "session-create";

/// The class the client toggles on the page root when the session
/// drawer is open — the drawer-open state hook. Deliberately **not**
/// in [`DOM_CONSTANTS`]: membership there asserts presence in a fresh
/// server render, and no server render ever emits this class — it is
/// client-mutated view state whose absence in the served markup is
/// exactly what makes a page swap reset the drawer structurally.
pub const DRAWER_OPEN_CLASS: &str = "drawer-open";

/// The titlebar's sessions button — beside the hamburger inside the
/// `#menu` span. One tap toggles the session drawer at every width;
/// below 768px it is the drawer's primary opener, at ≥768px the CSS
/// hides it (the sidebar is already static there). Wears the
/// [`SESSIONS_ATTENTION_CLASS`] and carries the
/// [`SESSIONS_BUTTON_COUNT_ID`] badge while the instance's unread
/// mark set is non-empty.
pub const SESSIONS_BUTTON_ID: &str = "sessions-button";

/// The sessions button's count badge — the number of sessions wearing
/// the unread mark. Hidden by the attention signal when the count is
/// zero.
pub const SESSIONS_BUTTON_COUNT_ID: &str = "sessions-button-count";

/// The class the client applies to [`SESSIONS_BUTTON_ID`] while the
/// unread count is above zero — the yellow fill, the same gold the
/// unread rows wear. Deliberately **not** in [`DOM_CONSTANTS`]: the
/// drawer-open precedent — a class applied by signal evaluation is
/// client state, and membership would assert a fresh-render fact the
/// served markup does not carry (the binding attribute names the
/// signal, not the class).
pub const SESSIONS_ATTENTION_CLASS: &str = "attention";

/// The menu's action name — the one item's action attribute value.
pub const ACTION_MENU_SESSIONS: &str = "menu-sessions";

// ── Web theme (color mode) ──────────────────────────────────────────

/// The menu's theme-control host id — the segmented Light/Dark/System
/// control inside the slide-in panel.
pub const MENU_ITEM_THEME_ID: &str = "menu-item-theme";
/// The theme control's Light option id.
pub const MENU_ITEM_THEME_LIGHT_ID: &str = "menu-item-theme-light";
/// The theme control's Dark option id.
pub const MENU_ITEM_THEME_DARK_ID: &str = "menu-item-theme-dark";
/// The theme control's System option id.
pub const MENU_ITEM_THEME_SYSTEM_ID: &str = "menu-item-theme-system";
/// The theme pick POST's endpoint — carries the chosen mode; the
/// response re-seeds the signal and patches the theme-color meta.
pub const THEME_PATH: &str = "/web/theme";
/// The active color mode's signal name (`light`/`dark`/`system` —
/// the values the CSS `data-theme` selectors and the persisted
/// `AppStateFile::web_theme` share).
pub const SIGNAL_WEB_THEME: &str = "webTheme";
/// The class marking the theme control's active option — signal-driven,
/// so the mark follows the mode without a re-render.
pub const THEME_ACTIVE_CLASS: &str = "theme-active";

// ── Datastar signal names ───────────────────────────────────────────
//
// The client runtime holds view-local state only; these names are the
// vocabulary the page's attributes and the server's signal patches
// share. They are contract (a rename breaks pushes and bindings
// invisibly), so they live beside the DOM constants.

/// The drawer-open toggle (the drawer class rides the page root).
pub const SIGNAL_DRAWER_OPEN: &str = "drawerOpen";
/// The menu panel's open toggle.
pub const SIGNAL_MENU_OPEN: &str = "menuOpen";
/// The model-picker modal's open toggle.
pub const SIGNAL_MODAL_OPEN: &str = "modalOpen";
/// The new-session modal's open toggle.
pub const SIGNAL_SESSION_NEW_OPEN: &str = "sessionNewOpen";
/// The model-switch POST's chosen provider id (stamped by the row).
pub const SIGNAL_PROVIDER_ID: &str = "providerId";
/// The model-picker filter text.
pub const SIGNAL_MODEL_FILTER: &str = "modelfilter";
/// The refresh-models spinner flag (set on click, cleared when the
/// cache-landed signal lands).
pub const SIGNAL_MODELS_REFRESHING: &str = "modelsRefreshing";
/// The model-cache-changed tick — every landing bumps it; the open
/// modal's effect re-pulls its rows in response.
pub const SIGNAL_MODELS_TICK: &str = "modelsTick";

/// The sessions-list tick — every pushed list refresh bumps it; the
/// list's effect re-pulls with the active filter (pushed lists are
/// signal-only: the pull remains the renderer).
pub const SIGNAL_SESSIONS_TICK: &str = "sessionsTick";
/// The titlebar sessions button's unread count — seeded at page
/// render and pushed alongside every sessions tick. The button's
/// attention class and its badge's visibility/text both key off it.
pub const SIGNAL_SESSIONS_ATTENTION: &str = "sessionsAttention";

/// The sessions filter's signal — bound by the filter input's
/// `data-bind`. HTML attribute names are ASCII case-insensitive, so a
/// `data-bind`-carried signal name reaches the runtime lowercased;
/// bound signals are single lowercase tokens (the expressions that
/// read them spell the same token).
pub const SIGNAL_SESSIONS_FILTER: &str = "sessionsfilter";
/// The session-switch interstitial flag — set true by a session-row
/// click that names a different session than the one viewed, ahead of
/// its activate POST. The activation response's wholesale page-root
/// swap re-seeds the signal false, which is the interstitial's only
/// clear path (no timer, no fetch-event listener).
pub const SIGNAL_SWITCHING: &str = "switching";
/// The viewed session's id (the page root's data attribute at boot).
pub const SIGNAL_VIEWED_SESSION: &str = "viewedSession";
/// The inline rename editor's draft title.
pub const SIGNAL_TITLE: &str = "title";
/// Whether the rename editor is open.
pub const SIGNAL_TITLE_EDITING: &str = "titleEditing";
/// Whether a rename commit is in flight (Enter/blur double-fire).
pub const SIGNAL_TITLE_COMMITTING: &str = "titleCommitting";
/// The composer's draft text.
pub const SIGNAL_TEXT: &str = "text";
/// The composer's pending image attachments — a JSON array of base64
/// data URLs in chip order. Client-local: the send clears it, a reload
/// drops it.
pub const SIGNAL_PENDING_IMAGES: &str = "images";
/// The hidden file input the attach button drives (`data-bind` reads its
/// files into the pending-images signal on `change`).
pub const IMAGE_FILE_INPUT_ID: &str = "image-file-input";
/// The attach button that clicks the hidden file input (photo library on
/// phones).
pub const IMAGE_ATTACH_BUTTON_ID: &str = "image-attach-button";
/// The strip above the textarea where pending-image chips render.
pub const IMAGE_CHIPS_ID: &str = "image-chips";
/// The new-session form's chosen project path.
pub const SIGNAL_SESSION_NEW_PROJECT: &str = "sessionnewproject";
/// The new-session form's chosen lifecycle name.
pub const SIGNAL_SESSION_NEW_LIFECYCLE: &str = "sessionnewlifecycle";
/// The new-session form's args draft.
pub const SIGNAL_SESSION_NEW_ARGS: &str = "sessionnewargs";

/// The drawer scrim — the full-viewport overlay behind the off-canvas
/// sidebar when the drawer is open. Static shell markup (inert at
/// boot, never swaps — the Static chrome rule), present in every
/// fresh render, so unlike [`DRAWER_OPEN_CLASS`] it **is** a
/// [`DOM_CONSTANTS`] member and the referential gate holds it.
pub const DRAWER_SCRIM_ID: &str = "drawer-scrim";

/// The menu scrim — the full-viewport overlay behind the slide-in
/// menu panel, at every viewport width. Static shell markup inside
/// `#page-root`, mirrored `hidden` like the drawer scrim, present in
/// every fresh render — a [`DOM_CONSTANTS`] member held by the
/// referential gate. Stacks above the drawer scrim (A6): the menu is
/// the entry surface.
pub const MENU_SCRIM_ID: &str = "menu-scrim";

/// The session-switch interstitial — the full-viewport loading
/// overlay shown while an activate POST triggered by a session-row
/// click is in flight (the `switching` signal drives its visibility).
/// Static shell markup inside `#page-root`, mirrored `hidden` like
/// the drawer and menu scrims, present in every fresh render — a
/// [`DOM_CONSTANTS`] member held by the referential gate. Stacks
/// above the menu panel (the topmost surface), since the click that
/// raises it may come from the drawer or the menu panel and must
/// cover both.
pub const SWITCH_OVERLAY_ID: &str = "switch-overlay";

/// The error-notice signal — a non-empty string renders the notice
/// banner. Set by the page root's fetch-error listener: a failed
/// activate or create POST dispatches the Datastar fetch `error`
/// event (a non-200 response carries no patches of its own), and the
/// listener clears `switching` and names the failure here. Cleared by
/// the next successful page swap (the swap re-seeds the signal empty).
pub const SIGNAL_ERROR_NOTICE: &str = "errorNotice";

/// The error-notice banner — the page-shell element the
/// [`SIGNAL_ERROR_NOTICE`] string renders in (its visibility rides a
/// `data-show` on the signal, its text a `data-text`). Static shell
/// markup inside `#page-root` beside the switch overlay; the failure
/// is visible on the current page without a reload — the self-healing
/// overlay's visible half.
pub const ERROR_NOTICE_ID: &str = "error-notice";

// ── New-session modal ───────────────────────────────────────────────

/// The new-session **modal's** host — an empty span sibling to the
/// model-picker modal host in the page shell (the same
/// host-not-container rule: opening this modal can never destroy the
/// menu that opened it). The form fragment swaps into this host on
/// open and on every lifecycle re-pull.
pub const SESSION_NEW_MODAL_HOST_ID: &str = "session-new-modal-host";

/// The same host spelled as a CSS selector — the swap-target header the
/// fragment pulls stamp (`Datastar-Selector`) and the Datastar bindings
/// in the templates name.
pub const SESSION_NEW_MODAL_HOST_SELECTOR: &str = "#session-new-modal-host";

/// The new-session modal overlay — the server-rendered form the
/// menu item opens; present in the page only after the pull renders
/// it in.
pub const SESSION_NEW_MODAL_ID: &str = "session-new-modal";

/// The modal's project `<select>` — one option per `[[project.entry]]`
/// entry (the raw configured path string is the option's value), or
/// the explanatory refusal row when none are configured.
pub const SESSION_NEW_PROJECT_SELECT_ID: &str = "session-new-project";

/// The modal's lifecycle `<select>` — the blank row first (empty
/// value), then one option per `[[session_lifecycle.script]]` entry.
pub const SESSION_NEW_LIFECYCLE_SELECT_ID: &str = "session-new-lifecycle";

/// The modal's args input — rendered only when the selected
/// lifecycle's setup declares parameters; its value is split with
/// the TUI arg popup's quote-preserving parser.
pub const SESSION_NEW_ARGS_INPUT_ID: &str = "session-new-args";

/// The modal's submit button — posts the create action.
pub const SESSION_NEW_SUBMIT_ID: &str = "session-new-submit";

/// The modal's explanatory-refusal wrapper — rendered instead of the
/// usable form when no `[[project.entry]]` entries are configured.
pub const SESSION_NEW_EMPTY_ID: &str = "session-new-empty";

/// The attribute a lifecycle option carries its parameter count in —
/// the client shows the args input when the selected option's count
/// is above zero.
pub const SESSION_NEW_PARAM_COUNT_ATTR: &str = "data-param-count";

// ── Persona modal ───────────────────────────────────────────────────

/// The persona **modal's** host — an empty span sibling to the other
/// modal hosts in the page shell (the same host-not-container rule:
/// opening this modal can never destroy the menu item that opened
/// it). The modal fragment swaps into this host on open and on every
/// filter re-pull.
pub const PERSONA_MODAL_HOST_ID: &str = "persona-modal-host";

/// The same host spelled as a CSS selector — the swap-target header
/// the fragment pulls stamp and the Datastar bindings name.
pub const PERSONA_MODAL_HOST_SELECTOR: &str = "#persona-modal-host";

/// The persona modal overlay — the server-rendered row list the menu
/// item opens; present in the page only after the pull renders it in.
pub const PERSONA_MODAL_ID: &str = "persona-modal";

/// The modal's filter input — each keystroke re-queries the server.
pub const PERSONA_FILTER_ID: &str = "persona-filter";

/// The modal's row container — the list the filter re-renders into.
pub const PERSONA_ROWS_ID: &str = "persona-rows";

/// The attribute a persona row carries its name in — the switch POST's
/// payload field, validated against the rows the modal renders.
pub const PERSONA_NAME_DATA_ATTR: &str = "data-webui-persona";

/// The attribute the active persona row carries — present only on the
/// row matching the session's current persona.
pub const PERSONA_ACTIVE_ATTR: &str = "data-webui-persona-active";

/// The persona modal's open toggle (view-local signal).
pub const SIGNAL_PERSONA_MODAL_OPEN: &str = "personaModalOpen";
/// The persona modal's filter text.
pub const SIGNAL_PERSONA_FILTER: &str = "personafilter";

/// The persona switch POST's action name.
pub const ACTION_PERSONA: &str = "persona";

/// The persona-picker pull's filter query parameter
/// (`GET /web/persona-picker/{id}?filter=…`) — the server-side fuzzy
/// filter over persona name and description; absent or empty renders
/// every row.
pub const PERSONA_FILTER_QUERY_PARAM: &str = "filter";

/// Route pattern — the persona-picker fragment pull.
pub const PERSONA_PICKER_ROUTE: &str = "/web/persona-picker/{id}";

/// The path prefix client code builds persona-picker URLs from.
pub const PERSONA_PICKER_PREFIX: &str = "/web/persona-picker/";

/// `POST` — switch the viewed session's persona (the same message set
/// the TUI confirm publishes).
pub const PERSONA_PATH: &str = "/web/persona";

// ── MCP servers modal ───────────────────────────────────────────────

/// The menu's MCP servers item — closes the menu and pulls the MCP
/// modal into its host for the viewed session. Rendered only when a
/// session is viewed: enablement binds per-session and the landing
/// page views nothing (server-omitted, never client-hidden), the
/// persona item's rule.
pub const MENU_ITEM_MCP_ID: &str = "menu-item-mcp";

/// The MCP menu item's action name.
pub const ACTION_MENU_MCP: &str = "menu-mcp";

/// The MCP **modal's** host — an empty span sibling to the other modal
/// hosts in the page shell (the host-not-container rule: opening this
/// modal can never destroy the menu item that opened it). The modal
/// fragment swaps into this host on open and on every tick re-pull.
pub const MCP_MODAL_HOST_ID: &str = "mcp-modal-host";

/// The same host spelled as a CSS selector — the swap-target header
/// the fragment pulls stamp and the Datastar bindings name.
pub const MCP_MODAL_HOST_SELECTOR: &str = "#mcp-modal-host";

/// The MCP modal overlay — the server-rendered row list the menu item
/// opens; present in the page only after the pull renders it in.
pub const MCP_MODAL_ID: &str = "mcp-modal";

/// The modal's row container — the list the tick re-pull re-renders.
pub const MCP_ROWS_ID: &str = "mcp-rows";

/// The attribute an MCP row carries its server name in — the toggle
/// POST's payload field, validated against the `[mcp.<name>]` set the
/// server holds.
pub const MCP_SERVER_DATA_ATTR: &str = "data-webui-server";

/// The MCP modal's filter input — every keystroke re-queries the pull
/// with the filter applied server-side (the persona/model pattern).
pub const MCP_FILTER_ID: &str = "mcp-filter";

/// The MCP filter text signal — bound to the filter input, read by the
/// tick effect's re-pull URL, and echoed by the toggle POST's response.
pub const SIGNAL_MCP_FILTER: &str = "mcpfilter";

/// The MCP modal's open toggle (view-local signal).
pub const SIGNAL_MCP_OPEN: &str = "mcpOpen";

/// The MCP-status tick — every runtime push bumps it; the open modal's
/// effect re-pulls its rows in response (the modelsTick pattern).
pub const SIGNAL_MCP_TICK: &str = "mcpTick";

/// The MCP toggle POST's action name.
pub const ACTION_MCP_TOGGLE: &str = "mcp-toggle";

/// Route pattern — the MCP servers modal fragment pull.
pub const MCP_PICKER_ROUTE: &str = "/web/mcp-servers/{id}";

/// The path prefix client code builds MCP-modal URLs from.
pub const MCP_PICKER_PREFIX: &str = "/web/mcp-servers/";

/// `POST` — toggle one MCP server's enablement for the viewed session
/// (the same message the TUI confirm publishes, the full desired set).
pub const MCP_TOGGLE_PATH: &str = "/web/mcp";

/// Route pattern — the MCP modal's **rows fragment** pull. The filter
/// keystrokes' and the `mcpTick` re-pull's target: it swaps the rows
/// container only, so the response never re-renders the filter input
/// (a host swap's `value="{{ filter }}"` would reset the input to the
/// stale pull's query mid-typing — the keystroke-drop bug).
pub const MCP_ROWS_ROUTE: &str = "/web/mcp-servers/rows/{id}";

/// The path prefix the modal's re-pull effect builds MCP-rows URLs
/// from.
pub const MCP_ROWS_PREFIX: &str = "/web/mcp-servers/rows/";

/// The MCP rows container spelled as a CSS selector — the re-pull's
/// swap target (never the modal host: the input must not re-render).
pub const MCP_ROWS_SELECTOR: &str = "#mcp-rows";

// ── Tools modal ─────────────────────────────────────────────────────

/// The menu's tools item — closes the menu and pulls the tools modal
/// into its host for the viewed session. Rendered only when a session
/// is viewed: the tool filter binds per-session and the landing page
/// views nothing (server-omitted, never client-hidden), the persona
/// item's rule.
pub const MENU_ITEM_TOOLS_ID: &str = "menu-item-tools";

/// The tools **modal's** host — an empty span sibling to the other
/// modal hosts in the page shell (the host-not-container rule: opening
/// this modal can never destroy the menu item that opened it). The
/// modal fragment swaps into this host on open; the revert snapshot's
/// stamp element rides beside the modal inside the same host, so a
/// toggle's modal patch can never touch it.
pub const TOOLS_MODAL_HOST_ID: &str = "tools-modal-host";

/// The same host spelled as a CSS selector — the swap-target header
/// the fragment pull stamps and the Datastar bindings name.
pub const TOOLS_MODAL_HOST_SELECTOR: &str = "#tools-modal-host";

/// The tools modal overlay — the server-rendered row list the menu
/// item opens; present in the page only after the pull renders it in.
pub const TOOLS_MODAL_ID: &str = "tools-modal";

/// The modal's row container — the list the toggle POST re-renders.
pub const TOOLS_ROWS_ID: &str = "tools-rows";

/// The revert snapshot's stamp element — the sibling span the open
/// pull renders inside the modal host, whose `data-init` patches the
/// revert signal once per open. Carrying an id is what makes a re-open
/// morph **update** the stamp (and re-run its `data-init`) instead of
/// stacking a second one beside it.
pub const TOOLS_REVERT_STAMP_ID: &str = "tools-revert-stamp";

/// The attribute a tools row carries its tool name in — the toggle
/// POST's payload field, validated against the rows the modal renders.
pub const TOOL_NAME_DATA_ATTR: &str = "data-webui-tool";

/// The tools modal's filter input — every keystroke re-queries the
/// **rows** pull with the filter applied server-side (the rows-only
/// retarget is the revert guarantee; the modal skeleton and the stamp
/// are never re-rendered by filtering).
pub const TOOLS_FILTER_ID: &str = "tools-filter";

/// The tools filter text signal — bound to the filter input, read by
/// the tick effect's rows re-pull URL, and echoed by the toggle POST's
/// response.
pub const SIGNAL_TOOLS_FILTER: &str = "toolsfilter";

/// The tools modal's open toggle (view-local signal).
pub const SIGNAL_TOOLS_OPEN: &str = "toolsOpen";

/// The open-time tool filter snapshot — a JSON `NameFilter` string the
/// open pull's stamp patches in, the dismissal posts back, and the
/// revert restores verbatim (absence rides as `null`). Toggle
/// re-renders never touch it: the stamp lives outside the swapped
/// modal.
pub const SIGNAL_TOOLS_REVERT: &str = "toolsRevert";

/// The tools tick — every MCP runtime push bumps it; the open modal's
/// effect re-pulls its **rows fragment** in response (the mcpTick
/// pattern). The payload is the write's timestamp (a signal with an
/// empty data line never dispatches in the browser). The rows-only
/// retarget is the revert guarantee: the stamp's `data-init` lives in
/// the modal skeleton the tick never re-renders.
pub const SIGNAL_TOOLS_TICK: &str = "toolsTick";

/// The tools toggle POST's action name.
pub const ACTION_TOOLS_TOGGLE: &str = "tools-toggle";

/// Route pattern — the tools modal fragment pull.
pub const TOOLS_PICKER_ROUTE: &str = "/web/tools/{id}";

/// The path prefix the menu item builds tools-modal URLs from.
pub const TOOLS_PICKER_PREFIX: &str = "/web/tools/";

/// `POST` — toggle one tool's enablement for the viewed session (the
/// TUI confirm's semantic applied per click: the filter becomes a deny
/// filter over the modal's off-rows with the clicked tool flipped).
pub const TOOLS_TOGGLE_PATH: &str = "/web/tools";

/// `POST` — dismiss the tools modal: restore the open-time filter the
/// revert snapshot signal carries.
pub const TOOLS_REVERT_PATH: &str = "/web/tools/revert";

/// Route pattern — the tools **rows fragment** pull. The `toolsTick`
/// effect's re-pull target: it swaps `#tools-rows` only, so the revert
/// stamp in the modal skeleton never re-initializes mid-open.
pub const TOOLS_ROWS_ROUTE: &str = "/web/tools/rows/{id}";

/// The path prefix the tick effect builds tools-rows URLs from.
pub const TOOLS_ROWS_PREFIX: &str = "/web/tools/rows/";

/// The rows container spelled as a CSS selector — the tick re-pull's
/// swap target (never the modal host: the stamp must not re-init).
pub const TOOLS_ROWS_SELECTOR: &str = "#tools-rows";

// ── Skills modal ────────────────────────────────────────────────────

/// The menu's Skills item — closes the menu and pulls the skills
/// modal into its host for the viewed session. Rendered only when a
/// session is viewed: enablement binds per-session and the landing
/// page views nothing (server-omitted, never client-hidden), the
/// persona item's rule.
pub const MENU_ITEM_SKILLS_ID: &str = "menu-item-skills";

/// The Skills menu item's action name.
pub const ACTION_MENU_SKILLS: &str = "menu-skills";

/// The skills **modal's** host — an empty span sibling to the other
/// modal hosts in the page shell (the host-not-container rule: opening
/// this modal can never destroy the menu item that opened it). The
/// modal fragment swaps into this host on open and on every tick
/// re-pull.
pub const SKILL_MODAL_HOST_ID: &str = "skills-modal-host";

/// The same host spelled as a CSS selector — the swap-target header
/// the fragment pulls stamp and the Datastar bindings name.
pub const SKILL_MODAL_HOST_SELECTOR: &str = "#skills-modal-host";

/// The skills modal overlay — the server-rendered row list the menu
/// item opens; present in the page only after the pull renders it in.
pub const SKILL_MODAL_ID: &str = "skills-modal";

/// The modal's row container — the list the tick re-pull re-renders.
pub const SKILL_ROWS_ID: &str = "skills-rows";

/// The attribute a skill row carries its name in — the toggle and
/// load POSTs' payload field, validated against the session's
/// discovered set.
pub const SKILL_NAME_DATA_ATTR: &str = "data-webui-skill";

/// The class hook a skill row's force-load button carries — the click
/// target the load POST rides, distinct from the row's own toggle
/// click.
pub const SKILL_LOAD_CLASS: &str = "skill-row-load";

/// The skills modal's filter input — every keystroke re-queries the
/// pull with the filter applied server-side (the persona/model
/// pattern).
pub const SKILL_FILTER_ID: &str = "skills-filter";

/// The skills filter text signal — bound to the filter input, read by
/// the tick effect's re-pull URL, and echoed by the toggle and load
/// POSTs' responses.
pub const SIGNAL_SKILLS_FILTER: &str = "skillsfilter";

/// The skills modal's open toggle (view-local signal).
pub const SIGNAL_SKILLS_OPEN: &str = "skillsOpen";

/// The skills tick — every discovery push bumps it; the open modal's
/// effect re-pulls its rows in response (the mcpTick pattern). The
/// payload is the scan's landing timestamp (a signal with an empty
/// data line never dispatches in the browser).
pub const SIGNAL_SKILLS_TICK: &str = "skillsTick";

/// The skill row's toggle POST action name.
pub const ACTION_SKILL_TOGGLE: &str = "skill-toggle";

/// The skill row's force-load POST action name.
pub const ACTION_SKILL_LOAD: &str = "skill-load";

/// Route pattern — the skills modal fragment pull.
pub const SKILL_PICKER_ROUTE: &str = "/web/skills/{id}";

/// The path prefix client code builds skills-modal URLs from.
pub const SKILL_PICKER_PREFIX: &str = "/web/skills/";

/// `POST` — toggle one skill's enablement for the viewed session
/// (mode-preserving `permit`/`withhold` on the session's filter; the
/// TUI toggle publishes nothing, and neither does this).
pub const SKILL_TOGGLE_PATH: &str = "/web/skills/toggle";

/// `POST` — force-load a skill into the viewed session's context
/// (the TUI `<c-l>` load verbatim: pinned pair, durable auto-enable,
/// `MarkSessionInteracted`, and the duplicate's transient notice).
pub const SKILL_LOAD_PATH: &str = "/web/skills/load";

/// Route pattern — the skills modal's **rows fragment** pull. The
/// filter keystrokes' and the `skillsTick` re-pull's target: it swaps
/// the rows container only, so the response never re-renders the
/// filter input (a host swap's `value="{{ filter }}"` would reset the
/// input to the stale pull's query mid-typing — the keystroke-drop
/// bug).
pub const SKILL_ROWS_ROUTE: &str = "/web/skills/rows/{id}";

/// The path prefix the modal's re-pull effect builds skills-rows URLs
/// from.
pub const SKILL_ROWS_PREFIX: &str = "/web/skills/rows/";

/// The skills rows container spelled as a CSS selector — the re-pull's
/// swap target (never the modal host: the input must not re-render).
pub const SKILL_ROWS_SELECTOR: &str = "#skills-rows";

// ── Endpoints ───────────────────────────────────────────────────────

/// The landing/session page path (the client's entry URL).
pub const WEB_PATH: &str = "/web";

/// `POST` — deliver text into a session.
pub const SEND_PATH: &str = "/web/send";

/// `POST` — cancel a session's live turn.
pub const CANCEL_PATH: &str = "/web/cancel";

/// `POST` — switch a session's model.
pub const MODEL_PATH: &str = "/web/model";

/// Route pattern — one session's page.
pub const SESSION_PAGE_ROUTE: &str = "/web/session/{id}";

/// Route pattern — activate (switch to) a session.
pub const ACTIVATE_ROUTE: &str = "/web/session/{id}/activate";

/// Route pattern — rename a session.
pub const RENAME_ROUTE: &str = "/web/session/{id}/rename";

/// Route pattern — one session's SSE stream.
pub const STREAM_ROUTE: &str = "/web/stream/{id}";

/// Route pattern — the sessions-list fragment pull.
pub const SESSIONS_ROUTE: &str = "/web/sessions";

/// Route pattern — the transcript cursor pull.
pub const TRANSCRIPT_ROUTE: &str = "/web/transcript/{id}";

/// Route pattern — the model-picker fragment.
pub const MODEL_PICKER_ROUTE: &str = "/web/model-picker/{id}";

/// Route pattern — one attachment's image bytes: the session, the entry
/// that carries it, and the attachment's ordinal within that entry.
pub const ATTACHMENT_ROUTE: &str = "/web/attachment/{session}/{entry}/{ordinal}";

/// Route pattern — the refresh-models action (`POST`): publishes
/// `RefreshModels` and returns the current button fragment.
pub const MODELS_REFRESH_ROUTE: &str = "/web/models/refresh";

/// The refresh-models action's path, for the client mirror set.
pub const MODELS_REFRESH_PATH: &str = "/web/models/refresh";

/// The model-picker pull's filter query parameter
/// (`GET /web/model-picker/{id}?filter=…`) — the server-side fuzzy
/// filter, the TUI's match core over model id and provider name;
/// absent or empty renders every row.
pub const MODEL_FILTER_QUERY_PARAM: &str = "filter";

/// Route pattern — the new-session form fragment pull
/// (`GET /web/session-new?lifecycle=…`) — the menu item's opening
/// modal and every lifecycle re-pull render through it.
pub const SESSION_NEW_ROUTE: &str = "/web/session-new";

/// The new-session form pull's lifecycle query parameter — absent or
/// empty selects the blank lifecycle (the form's first row).
pub const SESSION_NEW_LIFECYCLE_QUERY_PARAM: &str = "lifecycle";

/// `POST` — create a session (the shared creation path). Responds
/// with the new session's whole page, exactly as activation does.
pub const SESSION_CREATE_PATH: &str = "/web/session-new";

/// Route pattern — the embedded vendor assets.
pub const VENDOR_ROUTE: &str = "/vendor/{*path}";

/// The path prefix client code builds session-page URLs from.
pub const SESSION_PAGE_PREFIX: &str = "/web/session/";

/// The path suffix client code appends for an activate request.
pub const ACTIVATE_SUFFIX: &str = "/activate";

/// The path suffix client code appends for a rename request.
pub const RENAME_SUFFIX: &str = "/rename";

/// The path prefix client code builds SSE stream URLs from.
pub const STREAM_PREFIX: &str = "/web/stream/";

/// The path prefix client code builds transcript-pull URLs from.
pub const TRANSCRIPT_PREFIX: &str = "/web/transcript/";

/// The path prefix client code builds model-picker URLs from.
pub const MODEL_PICKER_PREFIX: &str = "/web/model-picker/";

/// The path prefix the transcript renderer builds attachment `<img>`
/// URLs from — `/web/attachment/{session}/{entry}/` + ordinal. Rendered
/// HTML only: no client code composes these (the fragments do).
pub const ATTACHMENT_PREFIX: &str = "/web/attachment/";

/// The vendor mount the client's script tag and the asset table use.
pub const VENDOR_PREFIX: &str = "/vendor/";

/// The Datastar browser bundle's served filename inside `/vendor/` —
/// what the page's `<script>` loads. The bundle is committed vendored
/// bytes (with the webapp install assets, the manifest and the icon
/// set), not build output: with the TypeScript client gone, nothing
/// in-repo regenerates them.
pub const VENDOR_APP_JS: &str = "datastar.js";

/// The image-attach helper's served filename inside `/vendor/` — the
/// one authored script besides the bundle (see `VENDOR_APP_JS` for why
/// the vendor tree is committed input).
pub const VENDOR_PASTE_JS: &str = "paste.js";

/// The contract's DOM-targeting constants: id, attribute, class, and
/// action names. The referential-integrity gate renders the page and
/// asserts each of these appears in the HTML.
pub const DOM_CONSTANTS: &[&str] = &[
    PAGE_ROOT_ID,
    SESSIONS_ID,
    SESSIONS_FILTER_INPUT_ID,
    TRANSCRIPT_WRAP_ID,
    TRANSCRIPT_ID,
    STREAMING_ID,
    CONTROLS_ID,
    MODEL_PICKER_ID,
    MODEL_PICKER_MODAL_HOST_ID,
    BUSY_SPINNER_ID,
    COMPOSER_ID,
    INPUT_ID,
    SIGNAL_PENDING_IMAGES,
    IMAGE_FILE_INPUT_ID,
    IMAGE_ATTACH_BUTTON_ID,
    IMAGE_CHIPS_ID,
    SESSION_ID_DATA_ATTR,
    PAGE_SESSION_DATA_ATTR,
    TRANSCRIPT_CURSOR_DATA_ATTR,
    ACTION_DATA_ATTR,
    ACTION_SEND,
    ACTION_CANCEL,
    ACTION_QUEUE,
    ACTION_MODEL,
    ACTION_REFRESH_MODELS,
    SESSION_ROW_CLASS,
    WORKING_CLASS,
    SESSION_TITLE_ID,
    MODEL_PICKER_BUTTON_ID,
    MODEL_PICKER_MODAL_ID,
    MODEL_PICKER_FILTER_ID,
    MODEL_PICKER_ROWS_ID,
    MODEL_PICKER_REFRESH_ID,
    MODEL_ID_DATA_ATTR,
    MODEL_MARKER_ATTR,
    MODEL_UNAVAILABLE_ATTR,
    MENU_ID,
    MENU_BUTTON_ID,
    MENU_PANEL_ID,
    SESSIONS_BUTTON_ID,
    SESSIONS_BUTTON_COUNT_ID,
    MENU_ITEM_SESSIONS_ID,
    MENU_ITEM_NEW_SESSION_ID,
    MENU_ITEM_PERSONA_ID,
    ACTION_MENU_PERSONA,
    SESSION_NEW_MODAL_HOST_ID,
    SESSION_NEW_MODAL_ID,
    SESSION_NEW_PROJECT_SELECT_ID,
    SESSION_NEW_LIFECYCLE_SELECT_ID,
    SESSION_NEW_ARGS_INPUT_ID,
    SESSION_NEW_SUBMIT_ID,
    SESSION_NEW_EMPTY_ID,
    SESSION_NEW_PARAM_COUNT_ATTR,
    PERSONA_MODAL_HOST_ID,
    PERSONA_MODAL_ID,
    PERSONA_FILTER_ID,
    PERSONA_ROWS_ID,
    PERSONA_NAME_DATA_ATTR,
    PERSONA_ACTIVE_ATTR,
    ACTION_PERSONA,
    ACTION_MENU_SESSIONS,
    ACTION_MENU_NEW_SESSION,
    MENU_ITEM_MCP_ID,
    ACTION_MENU_MCP,
    MCP_MODAL_HOST_ID,
    MCP_MODAL_ID,
    MCP_FILTER_ID,
    MCP_ROWS_ID,
    MCP_SERVER_DATA_ATTR,
    ACTION_MCP_TOGGLE,
    MENU_ITEM_TOOLS_ID,
    TOOLS_MODAL_HOST_ID,
    TOOLS_MODAL_ID,
    TOOLS_FILTER_ID,
    TOOLS_ROWS_ID,
    TOOLS_REVERT_STAMP_ID,
    TOOL_NAME_DATA_ATTR,
    ACTION_TOOLS_TOGGLE,
    MENU_ITEM_SKILLS_ID,
    ACTION_MENU_SKILLS,
    SKILL_MODAL_HOST_ID,
    SKILL_MODAL_ID,
    SKILL_FILTER_ID,
    SKILL_ROWS_ID,
    SKILL_NAME_DATA_ATTR,
    SKILL_LOAD_CLASS,
    ACTION_SKILL_TOGGLE,
    ACTION_SKILL_LOAD,
    DRAWER_SCRIM_ID,
    MENU_SCRIM_ID,
    SWITCH_OVERLAY_ID,
    ERROR_NOTICE_ID,
    MENU_ITEM_THEME_ID,
    MENU_ITEM_THEME_LIGHT_ID,
    MENU_ITEM_THEME_DARK_ID,
    MENU_ITEM_THEME_SYSTEM_ID,
    THEME_ACTIVE_CLASS,
    SIGNAL_WEB_THEME,
];

/// The sessions pull's query parameter (`GET /web/sessions?q=<text>`)
/// — the list's filter effect builds its re-pull URL with it.
pub const SESSIONS_FILTER_QUERY_PARAM: &str = "q";

/// The sessions pull's viewer query parameter
/// (`GET /web/sessions?viewed=<id>`) — the list's effect builds its
/// re-pull URL with it beside the filter, so a tick re-pull renders
/// the *viewed* session's blue row rather than the instance's active
/// one. Empty (the landing page) serializes as "no viewer".
pub const SESSIONS_VIEWED_QUERY_PARAM: &str = "viewed";

/// The contract's endpoint constants a client can build a URL from.
/// The serve-and-request gate issues a real HTTP request against each.
pub const CLIENT_PATHS: &[&str] = &[
    WEB_PATH,
    SEND_PATH,
    CANCEL_PATH,
    MODEL_PATH,
    SESSION_PAGE_PREFIX,
    ACTIVATE_SUFFIX,
    RENAME_SUFFIX,
    STREAM_PREFIX,
    TRANSCRIPT_PREFIX,
    MODEL_PICKER_PREFIX,
    ATTACHMENT_PREFIX,
    MODEL_FILTER_QUERY_PARAM,
    MODELS_REFRESH_PATH,
    SESSION_NEW_LIFECYCLE_QUERY_PARAM,
    PERSONA_PICKER_PREFIX,
    PERSONA_FILTER_QUERY_PARAM,
    PERSONA_PATH,
    MCP_PICKER_PREFIX,
    MCP_TOGGLE_PATH,
    MCP_ROWS_PREFIX,
    MCP_ROWS_SELECTOR,
    TOOLS_PICKER_PREFIX,
    TOOLS_TOGGLE_PATH,
    TOOLS_REVERT_PATH,
    TOOLS_ROWS_PREFIX,
    TOOLS_ROWS_SELECTOR,
    SKILL_PICKER_PREFIX,
    SKILL_TOGGLE_PATH,
    SKILL_LOAD_PATH,
    SKILL_ROWS_PREFIX,
    SKILL_ROWS_SELECTOR,
    SESSIONS_FILTER_QUERY_PARAM,
    SESSIONS_VIEWED_QUERY_PARAM,
    VENDOR_PREFIX,
    THEME_PATH,
];

/// The subset of [`CLIENT_PATHS`] the **browser runtime** builds
/// request URLs from — the vocabulary the e2e selectors spell and the
/// templates' Datastar attributes interpolate. The other paths are
/// server-rendered (page navigation, the transcript pull the runtime
/// reaches by URL, not by string-building): they are contract, but not
/// client-contract. `MODEL_PICKER_PREFIX` joined when the picker's
/// filtering moved server-side — the runtime now builds picker pull
/// URLs (with the filter query) itself.
pub const CLIENT_URLS: &[&str] = &[
    SEND_PATH,
    CANCEL_PATH,
    MODEL_PATH,
    STREAM_PREFIX,
    SESSION_PAGE_PREFIX,
    MODEL_PICKER_PREFIX,
    MODELS_REFRESH_PATH,
    SESSION_CREATE_PATH,
    PERSONA_PICKER_PREFIX,
    PERSONA_PATH,
    MCP_PICKER_PREFIX,
    MCP_TOGGLE_PATH,
    MCP_ROWS_PREFIX,
    MCP_ROWS_SELECTOR,
    TOOLS_PICKER_PREFIX,
    TOOLS_TOGGLE_PATH,
    TOOLS_REVERT_PATH,
    TOOLS_ROWS_PREFIX,
    TOOLS_ROWS_SELECTOR,
    SKILL_PICKER_PREFIX,
    SKILL_TOGGLE_PATH,
    SKILL_LOAD_PATH,
    SKILL_ROWS_PREFIX,
    SKILL_ROWS_SELECTOR,
    VENDOR_PREFIX,
    THEME_PATH,
];
