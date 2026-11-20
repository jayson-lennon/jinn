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

/// The titlebar's model-status wrapper — the static `.m` label span
/// and the picker container rendered together as one fragment, so a
/// model switch re-renders both in one swap and the two can never
/// disagree after a switch.
pub const MODEL_STATUS_ID: &str = "model-status";

/// The model-picker's button — the titlebar control that opens the
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

/// The model form's action name.
pub const ACTION_MODEL: &str = "model";

/// The refresh-models action's name (the modal's refresh button).
pub const ACTION_REFRESH_MODELS: &str = "refresh-models";

/// The class hook every session row carries.
pub const SESSION_ROW_CLASS: &str = "srow";

/// The titlebar's title span — the inline rename editor's host. The
/// session page renders it with the viewed title; clicking it opens
/// the editor (the client swaps an input inside the span). Absent
/// content-wise on the landing page, whose "jinn" title is inert.
pub const SESSION_TITLE_ID: &str = "session-title";

/// The titlebar's menu region — the hamburger button's host span,
/// rendered left of the session title. The panel renders inside this
/// host too (a static fragment, like the model-status wrapper), so
/// the region owns one container on the page.
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

/// The new-session form submit's action name.
pub const ACTION_SESSION_CREATE: &str = "session-create";

/// The class the client toggles on the page root when the session
/// drawer is open — the drawer-open state hook. Deliberately **not**
/// in [`DOM_CONSTANTS`]: membership there asserts presence in a fresh
/// server render, and no server render ever emits this class — it is
/// client-mutated view state whose absence in the served markup is
/// exactly what makes a page swap reset the drawer structurally.
pub const DRAWER_OPEN_CLASS: &str = "drawer-open";

/// The menu's action name — the one item's action attribute value.
pub const ACTION_MENU_SESSIONS: &str = "menu-sessions";

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
pub const SIGNAL_MODEL_FILTER: &str = "modelFilter";
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
/// The new-session form's chosen project path.
pub const SIGNAL_SESSION_NEW_PROJECT: &str = "sessionNewProject";
/// The new-session form's chosen lifecycle name.
pub const SIGNAL_SESSION_NEW_LIFECYCLE: &str = "sessionNewLifecycle";
/// The new-session form's args draft.
pub const SIGNAL_SESSION_NEW_ARGS: &str = "sessionNewArgs";

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

// ── New-session modal ───────────────────────────────────────────────

/// The new-session **modal's** host — an empty span sibling to the
/// model-picker modal host in the page shell (the same
/// host-not-container rule: opening this modal can never destroy the
/// menu that opened it). The form fragment swaps into this host on
/// open and on every lifecycle re-pull.
pub const SESSION_NEW_MODAL_HOST_ID: &str = "session-new-modal-host";

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

/// The vendor mount the client's script tag and the asset table use.
pub const VENDOR_PREFIX: &str = "/vendor/";

/// The Datastar browser bundle's served filename inside `/vendor/` —
/// what the page's `<script>` loads. The bundle is committed vendored
/// bytes (the manifest's one row), not build output: with the
/// TypeScript client gone, nothing in-repo regenerates it.
pub const VENDOR_APP_JS: &str = "datastar.js";

/// The contract's DOM-targeting constants: id, attribute, class, and
/// action names. The referential-integrity gate renders the page and
/// asserts each of these appears in the HTML.
pub const DOM_CONSTANTS: &[&str] = &[
    PAGE_ROOT_ID,
    SESSIONS_ID,
    SESSIONS_FILTER_INPUT_ID,
    TRANSCRIPT_WRAP_ID,
    TRANSCRIPT_ID,
    CONTROLS_ID,
    MODEL_PICKER_ID,
    MODEL_PICKER_MODAL_HOST_ID,
    MODEL_STATUS_ID,
    BUSY_SPINNER_ID,
    COMPOSER_ID,
    INPUT_ID,
    SESSION_ID_DATA_ATTR,
    PAGE_SESSION_DATA_ATTR,
    TRANSCRIPT_CURSOR_DATA_ATTR,
    ACTION_DATA_ATTR,
    ACTION_SEND,
    ACTION_CANCEL,
    ACTION_MODEL,
    ACTION_REFRESH_MODELS,
    SESSION_ROW_CLASS,
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
    MENU_ITEM_SESSIONS_ID,
    MENU_ITEM_NEW_SESSION_ID,
    SESSION_NEW_MODAL_HOST_ID,
    SESSION_NEW_MODAL_ID,
    SESSION_NEW_PROJECT_SELECT_ID,
    SESSION_NEW_LIFECYCLE_SELECT_ID,
    SESSION_NEW_ARGS_INPUT_ID,
    SESSION_NEW_SUBMIT_ID,
    SESSION_NEW_EMPTY_ID,
    SESSION_NEW_PARAM_COUNT_ATTR,
    ACTION_MENU_SESSIONS,
    ACTION_MENU_NEW_SESSION,
    DRAWER_SCRIM_ID,
    MENU_SCRIM_ID,
];

/// The sessions pull's query parameter (`GET /web/sessions?q=<text>`)
/// — the list's filter effect builds its re-pull URL with it.
pub const SESSIONS_FILTER_QUERY_PARAM: &str = "q";

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
    MODEL_FILTER_QUERY_PARAM,
    MODELS_REFRESH_PATH,
    SESSION_NEW_LIFECYCLE_QUERY_PARAM,
    VENDOR_PREFIX,
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
    VENDOR_PREFIX,
];
