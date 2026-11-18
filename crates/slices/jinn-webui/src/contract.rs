//! The web client contract — the single source both sides derive from.
//!
//! Every element id, data attribute, class hook, and endpoint path the
//! browser client targets lives here as a `&'static str` constant. The
//! askama templates render these values (threaded through the template
//! models' `Default` impls), the router registers its routes from them,
//! and the TypeScript client in `webui/client/src/contract.ts` mirrors
//! them as literals. [`contract_tests`] holds the two gates that keep
//! the mirroring honest: a name-and-value parity check against the TS
//! file, and a referential-integrity check that every DOM-targeting
//! constant appears in rendered HTML.
//!
//! The SSE event names are **not** duplicated here — they are
//! [`SseKind::event_name`] in `jinn-webui-msg`, and the client's wire
//! module mirrors those (the same parity gate covers them through the
//! wire fixture).

/// The page shell's root — the element a session switch replaces.
pub const PAGE_ROOT_ID: &str = "page-root";

/// The sessions-list container inside the sidebar.
pub const SESSIONS_ID: &str = "sessions";

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
/// (`GET /web/model-picker/{id}?filter=…`) — the server-side substring
/// filter over model id and provider name; absent or empty renders
/// every row.
pub const MODEL_FILTER_QUERY_PARAM: &str = "filter";

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

/// The client entry module's served filename inside `/vendor/` —
/// what the page's `<script type="module">` loads. The remaining
/// modules (`api.js`, `render.js`, `sse.js`, `wire.js`) arrive
/// through the browser's import graph relative to this entry.
pub const VENDOR_APP_JS: &str = "main.js";

/// The vendor manifest row (and served path) of the client bundle.
pub const VENDOR_APP_JS_PATH: &str = "main.js";

/// The contract's DOM-targeting constants: id, attribute, class, and
/// action names. The referential-integrity gate renders the page and
/// asserts each of these appears in the HTML.
pub const DOM_CONSTANTS: &[&str] = &[
    PAGE_ROOT_ID,
    SESSIONS_ID,
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
];

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
    VENDOR_PREFIX,
];

/// The subset of [`CLIENT_PATHS`] the **browser client** builds
/// request URLs from — the mirror set `wire.ts` must carry and the
/// generated fixture checks. The other paths are server-rendered
/// (page navigation, the transcript pull the client reaches by URL,
/// not by string-building): they are contract, but not
/// client-contract. `MODEL_PICKER_PREFIX` joined when the picker's
/// filtering moved server-side — the client now builds picker pull
/// URLs (with the filter query) itself.
pub const CLIENT_URLS: &[&str] = &[
    SEND_PATH,
    CANCEL_PATH,
    MODEL_PATH,
    STREAM_PREFIX,
    SESSION_PAGE_PREFIX,
    MODEL_PICKER_PREFIX,
    MODELS_REFRESH_PATH,
    VENDOR_PREFIX,
];
