//! The askama page and fragment templates.
//!
//! Compile-time-checked: a template naming a field the model lacks
//! fails the build, which is the reason this stack was chosen. The
//! page shell is inert markup — data attributes and ids from
//! [`crate::contract`]; every behavior (stream subscription, fragment
//! swaps, action posts) lives in the TypeScript client compiled from
//! `webui/client/`. Every dynamic region is a fragment template the
//! handlers render and the client swaps into the page.
//!
//! Escaping: askama escapes by extension — `.html` templates escape
//! every interpolation — so a session title carrying markup cannot
//! break the page. Pre-rendered HTML (the markdown bodies) rides
//! through askama's `|safe` filter *only* on fields the transcript
//! module produced, and that module is where the escaping contract
//! lives.

use askama::Template;
use jinn_webui_msg::SessionListItem;

/// The contract constants the templates interpolate. Kept as a local
/// re-export so template expressions stay short.
use crate::contract;

/// The page shell's base stylesheet: the design tokens and the
/// cross-region chrome (flex layout, titlebar, transcript entry
/// styling is the transcript region's; see [`crate::region`]'s `css`
/// join). Region styles live on the region and arrive through
/// [`page_style`], so a region change never edits this constant.
pub const BASE_CSS: &str = r#"
:root { --bg: #14161a; --fg: #d7dae0; --muted: #8a8f98; --accent: #4c8bd8;
        --border: #2a2e35; --panel: #1b1e24; --err: #e5534b; --ok: #57ab5a; }
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg);
       font: 15px/1.55 system-ui, sans-serif; }
#page-root { display: flex; height: 100vh; height: 100dvh; }
#main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
#titlebar { padding: 10px 18px; padding-left: calc(18px + env(safe-area-inset-left));
            padding-right: calc(18px + env(safe-area-inset-right));
            padding-top: calc(10px + env(safe-area-inset-top));
            border-bottom: 1px solid var(--border);
            display: flex; align-items: baseline; gap: 12px; }
#titlebar .t { font-weight: 600; min-width: 0; overflow: hidden;
               text-overflow: ellipsis; white-space: nowrap; }
#titlebar .m { color: var(--muted); font-size: 12px; }
#menu { position: relative; display: inline-flex; }
#menu-button { background: var(--panel); color: var(--fg);
               border: 1px solid var(--border); border-radius: 6px;
               padding: 6px 10px; font: inherit; line-height: 1;
               cursor: pointer; }
#menu-button:hover { border-color: var(--accent); }
#menu-panel button { background: transparent; color: var(--fg); text-align: left;
                     border: 0; border-radius: 5px; padding: 8px 12px;
                     font: inherit; cursor: pointer; white-space: nowrap; }
#menu-panel button:hover { background: #23272f; }
"#;

/// The stylesheet the page shell inlines: the base chrome, then every
/// registered region's fragment in registry order (registration order
/// is the equal-specificity tie-breaker; regions are appended at the
/// tail, so an established region's rules never lose to a later one).
#[must_use]
pub fn page_style() -> String {
    let mut css = String::from(BASE_CSS);
    for region in crate::region::regions() {
        css.push('\n');
        css.push_str(region.css());
    }
    css
}

/// The entry module's cache-busting URL — `main.<hash>.js`, the hash
/// being the manifest row's sha256 prefix.
///
/// # Panics
///
/// Panics when the entry module is not a manifest row — a contract
/// break caught at the first page render, in tests, not in the wild.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "a missing entry row is a contract break that must panic loudly at the \
              first page render, not a fallible path to thread through"
)]
pub fn app_js_versioned_path() -> String {
    crate::vendor::versioned_path(contract::VENDOR_APP_JS)
        .expect("the client entry module is a manifest row")
}

/// The full page shell. Rendered by `GET /web` and by
/// `GET /web/session/{id}` (a session page is the same shell with the
/// regions filled for that session).
#[derive(Template)]
#[template(path = "page.html")]
pub struct PageTemplate {
    /// The stylesheet text, inlined into the page.
    pub style: &'static str,
    /// The viewed session's id (empty on the no-session landing page).
    pub session_id: String,
    /// The session title.
    pub title: String,
    /// The model-status wrapper fragment (label + picker), rendered
    /// inline — the titlebar's whole model display.
    pub status_html: String,
    /// The rendered-entry cursor the page's inline transcript already
    /// carries — the stream's starting `Last-Event-ID`, so catch-up
    /// never re-appends what the page render included.
    pub cursor: usize,
    /// The sessions-list fragment, rendered inline.
    pub sessions_html: String,
    /// The transcript fragment, rendered inline.
    pub transcript_html: String,
    /// The composer-controls fragment, rendered inline.
    pub controls_html: String,
    /// The menu fragment (hamburger + panel), rendered inline.
    pub menu_html: String,
    /// `/vendor/` — the script tag's URL prefix.
    pub vendor_prefix: &'static str,
    /// The client bundle's cache-busting URL inside `/vendor/`
    /// (`main.<hash>.js`) — the hash is the manifest row's sha256
    /// prefix, so an upgraded binary serves a URL no old page cached.
    pub app_js_versioned: String,
    /// The page-root id — the element a session switch replaces.
    pub page_root_id: &'static str,
    /// The attribute carrying the viewed session's id.
    pub page_session_attr: &'static str,
    /// The sessions-list container id.
    pub sessions_id: &'static str,
    /// The sessions filter input's id — in the page shell, outside the
    /// `#sessions` swap target.
    pub sessions_filter_input_id: &'static str,
    /// The model-picker modal's host id — the empty span sibling the
    /// modal fragment swaps into.
    pub modal_host_id: &'static str,
    /// The transcript scroll-wrap id.
    pub transcript_wrap_id: &'static str,
    /// The transcript entry container id.
    pub transcript_id: &'static str,
    /// The attribute carrying the inline transcript's cursor.
    pub cursor_attr: &'static str,
    /// The composer-controls container id.
    pub controls_id: &'static str,
    /// The titlebar title span's id — present on session pages (the
    /// inline rename editor's host), empty on the landing page whose
    /// "jinn" title is inert.
    pub session_title_id: &'static str,
    /// The menu region's host container id — the hamburger + panel
    /// render inside it in the titlebar.
    pub menu_id: &'static str,
    /// The drawer scrim's id — the static shell overlay behind the
    /// off-canvas sidebar when the drawer is open.
    pub drawer_scrim_id: &'static str,
    /// The menu scrim's id — the static shell overlay behind the
    /// slide-in menu panel (stacked above the drawer scrim).
    pub menu_scrim_id: &'static str,
}

impl PageTemplate {
    /// The no-session landing page: sessions list only, every other
    /// region empty, cursor zero.
    #[must_use]
    pub fn landing(sessions_html: String, menu_html: String) -> Self {
        Self {
            style: Box::leak(page_style().into_boxed_str()),
            session_id: String::new(),
            title: "jinn".to_owned(),
            cursor: 0,
            sessions_html,
            transcript_html: String::new(),
            controls_html: String::new(),
            status_html: String::new(),
            menu_html,
            vendor_prefix: contract::VENDOR_PREFIX,
            app_js_versioned: app_js_versioned_path(),
            page_root_id: contract::PAGE_ROOT_ID,
            page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
            sessions_id: contract::SESSIONS_ID,
            sessions_filter_input_id: contract::SESSIONS_FILTER_INPUT_ID,
            modal_host_id: contract::MODEL_PICKER_MODAL_HOST_ID,
            transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
            transcript_id: contract::TRANSCRIPT_ID,
            cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
            controls_id: contract::CONTROLS_ID,
            session_title_id: "",
            menu_id: contract::MENU_ID,
            drawer_scrim_id: contract::DRAWER_SCRIM_ID,
            menu_scrim_id: contract::MENU_SCRIM_ID,
        }
    }

    /// A session page: every region filled for one viewed session at
    /// the cursor its inline transcript already carries.
    #[must_use]
    pub fn session(
        session_id: String,
        title: String,
        cursor: usize,
        sessions_html: String,
        transcript_html: String,
        controls_html: String,
        status_html: String,
        menu_html: String,
    ) -> Self {
        Self {
            style: Box::leak(page_style().into_boxed_str()),
            session_id,
            title,
            cursor,
            sessions_html,
            transcript_html,
            controls_html,
            status_html,
            menu_html,
            vendor_prefix: contract::VENDOR_PREFIX,
            app_js_versioned: app_js_versioned_path(),
            page_root_id: contract::PAGE_ROOT_ID,
            page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
            sessions_id: contract::SESSIONS_ID,
            sessions_filter_input_id: contract::SESSIONS_FILTER_INPUT_ID,
            modal_host_id: contract::MODEL_PICKER_MODAL_HOST_ID,
            transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
            transcript_id: contract::TRANSCRIPT_ID,
            cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
            controls_id: contract::CONTROLS_ID,
            session_title_id: contract::SESSION_TITLE_ID,
            menu_id: contract::MENU_ID,
            drawer_scrim_id: contract::DRAWER_SCRIM_ID,
            menu_scrim_id: contract::MENU_SCRIM_ID,
        }
    }
}

/// One transcript entry of the transcript fragment.
#[derive(Template)]
#[template(path = "entry.html")]
pub struct EntryTemplate {
    /// The rendered entry.
    pub entry: crate::transcript::RenderedEntry,
    /// The entry's index — the cursor coordinate, rendered as the
    /// element id.
    pub index: usize,
}

/// The transcript fragment: the entries at `cursor..` (or all).
#[derive(Template)]
#[template(path = "transcript.html")]
pub struct TranscriptTemplate {
    /// The rendered entries.
    pub entries: Vec<crate::transcript::RenderedEntry>,
    /// The index the slice starts at (the first entry's cursor
    /// position).
    pub start: usize,
}

/// One row of the sessions fragment.
#[derive(Template)]
#[template(path = "session_row.html")]
pub struct SessionRowTemplate {
    /// The row.
    pub item: SessionListItem,
    /// The row's class hook.
    pub row_class: &'static str,
    /// The attribute carrying the row's session id.
    pub session_id_attr: &'static str,
}

impl SessionRowTemplate {
    /// Builds the row template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(item: SessionListItem) -> Self {
        Self {
            item,
            row_class: contract::SESSION_ROW_CLASS,
            session_id_attr: contract::SESSION_ID_DATA_ATTR,
        }
    }
}

/// The sessions fragment: the sidebar list. The include of
/// `session_row.html` shares this struct's context, so the row hooks
/// ride as fields here.
#[derive(Template)]
#[template(path = "sessions.html")]
pub struct SessionsTemplate {
    /// One row per session, active first.
    pub rows: Vec<SessionListItem>,
    /// The row's class hook, visible to the row include.
    pub row_class: &'static str,
    /// The attribute carrying each row's session id.
    pub session_id_attr: &'static str,
}

impl SessionsTemplate {
    /// Builds the fragment with the contract's row hooks filled.
    #[must_use]
    pub fn with_contract(rows: Vec<SessionListItem>) -> Self {
        Self {
            rows,
            row_class: contract::SESSION_ROW_CLASS,
            session_id_attr: contract::SESSION_ID_DATA_ATTR,
        }
    }
}

/// The composer-controls fragment: the send↔cancel button.
#[derive(Template)]
#[template(path = "controls.html")]
pub struct ControlsTemplate {
    /// The session the composer targets.
    pub session_id: String,
    /// Whether the session is busy (true renders the cancel button).
    pub busy: bool,
    /// The streaming spinner's id (rendered only while busy).
    pub spinner_id: &'static str,
    /// The form container's id.
    pub composer_id: &'static str,
    /// The textarea's id.
    pub input_id: &'static str,
    /// The attribute a button names its action with.
    pub action_attr: &'static str,
    /// The send action's contract name.
    pub send_action: &'static str,
    /// The cancel action's contract name.
    pub cancel_action: &'static str,
    /// The send button's label.
    pub send_label: &'static str,
    /// The cancel button's label.
    pub cancel_label: &'static str,
}

impl ControlsTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(session_id: String, busy: bool) -> Self {
        Self {
            session_id,
            busy,
            spinner_id: contract::BUSY_SPINNER_ID,
            composer_id: contract::COMPOSER_ID,
            input_id: contract::INPUT_ID,
            action_attr: contract::ACTION_DATA_ATTR,
            send_action: contract::ACTION_SEND,
            cancel_action: contract::ACTION_CANCEL,
            send_label: "Send",
            cancel_label: "Cancel",
        }
    }
}

/// The menu fragment: the titlebar's hamburger button and its item
/// panel, rendered as one static fragment (the model-picker button's
/// precedent — a region renders its page-fill through a fragment even
/// when the content is chrome). The panel ships `hidden`; the client
/// toggles visibility and, on the item, the drawer-open class on the
/// page root.
#[derive(Template)]
#[template(path = "menu.html")]
pub struct MenuTemplate {
    /// The menu region's host container id.
    pub menu_id: &'static str,
    /// The hamburger button's id.
    pub menu_button_id: &'static str,
    /// The item panel's id.
    pub menu_panel_id: &'static str,
    /// The one item's id ("Sessions").
    pub menu_item_sessions_id: &'static str,
    /// The attribute a button names its action with.
    pub action_attr: &'static str,
    /// The menu item's action name.
    pub menu_action: &'static str,
}

impl MenuTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract() -> Self {
        Self {
            menu_id: contract::MENU_ID,
            menu_button_id: contract::MENU_BUTTON_ID,
            menu_panel_id: contract::MENU_PANEL_ID,
            menu_item_sessions_id: contract::MENU_ITEM_SESSIONS_ID,
            action_attr: contract::ACTION_DATA_ATTR,
            menu_action: contract::ACTION_MENU_SESSIONS,
        }
    }
}

/// The model-picker's titlebar button fragment: the current model as a
/// clickable control. The label carries the viewed selection; the
/// client opens the modal on its click.
#[derive(Template)]
#[template(path = "model_button.html")]
pub struct ModelButtonTemplate {
    /// The session the picker targets.
    pub session_id: String,
    /// The current selection's label.
    pub current: String,
    /// The button's id.
    pub button_id: &'static str,
    /// The picker container's id.
    pub picker_id: &'static str,
}

impl ModelButtonTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(session_id: String, current: String) -> Self {
        Self {
            session_id,
            current,
            button_id: contract::MODEL_PICKER_BUTTON_ID,
            picker_id: contract::MODEL_PICKER_ID,
        }
    }
}

/// The titlebar's model-status wrapper: the static `.m` label span and
/// the picker container rendered **as one fragment**, so a switch
/// re-renders both in one swap (the `.m` label used to be static page
/// markup that stayed stale after a switch). The page shell renders it
/// inline at boot; the switch and refresh POSTs answer with it.
#[derive(Template)]
#[template(path = "model_status.html")]
pub struct ModelStatusTemplate {
    /// The current selection's label (the `.m` span's text).
    pub model: String,
    /// The pre-rendered button fragment (inlined with `|safe`).
    pub model_html: String,
    /// The wrapper's id.
    pub status_id: &'static str,
    /// The picker container's id (inside the wrapper).
    pub picker_id: &'static str,
}

impl ModelStatusTemplate {
    /// Builds the template from the current label and button HTML.
    #[must_use]
    pub fn with_contract(model: String, model_html: String) -> Self {
        Self {
            model,
            model_html,
            status_id: contract::MODEL_STATUS_ID,
            picker_id: contract::MODEL_PICKER_ID,
        }
    }
}

/// One row of the model-picker modal — the shared builder's
/// [`ProviderPickerEntry`][entry] data stamped into markup. Rendered
/// standalone; the modal embeds the pre-rendered rows.
///
/// [entry]: jinn_provider_selection_msg::ProviderPickerEntry
#[derive(Template)]
#[template(path = "model_row.html")]
pub struct ModelRowTemplate {
    /// The row's entry data (presentation-free).
    pub entry: jinn_provider_selection_msg::ProviderPickerEntry,
    /// The model field split into matched/plain runs from the fuzzy
    /// filter — matched runs render in a highlight span. One plain run
    /// of the full text when there is no filter.
    pub model_segments: Vec<jinn_provider_selection_msg::provider_rows::TextSegment>,
    /// The provider-name field, split like `model_segments`.
    pub provider_segments: Vec<jinn_provider_selection_msg::provider_rows::TextSegment>,
    /// The session the switch targets.
    pub session_id: String,
    /// Whether this row is the session's current model (bold + marked).
    pub is_active: bool,
    /// The attribute a row names its model with.
    pub model_attr: &'static str,
    /// The attribute a row names its marker with.
    pub marker_attr: &'static str,
    /// The attribute an unavailable row carries.
    pub unavailable_attr: &'static str,
    /// The attribute a row names its action with.
    pub action_attr: &'static str,
    /// The switch action's contract name.
    pub model_action: &'static str,
}

impl ModelRowTemplate {
    /// Builds the template from a filtered entry: the entry data plus
    /// its filter-highlight segments.
    #[must_use]
    pub fn from_filtered(
        filtered: jinn_provider_selection_msg::FilteredEntry,
        session_id: String,
        is_active: bool,
    ) -> Self {
        let segments = filtered.segments();
        Self {
            entry: filtered.entry,
            model_segments: segments.model,
            provider_segments: segments.provider,
            session_id,
            is_active,
            model_attr: contract::MODEL_ID_DATA_ATTR,
            marker_attr: contract::MODEL_MARKER_ATTR,
            unavailable_attr: contract::MODEL_UNAVAILABLE_ATTR,
            action_attr: contract::ACTION_DATA_ATTR,
            model_action: contract::ACTION_MODEL,
        }
    }
}

/// The model-picker modal: the overlay, the filter input, the row
/// container, and the refresh button. Rendered into the picker
/// container by the GET pull (with the filter applied server-side);
/// the client toggles its visibility. The rows arrive pre-rendered
/// (each [`ModelRowTemplate`] renders first — the same shape the
/// sessions list uses for its rows).
#[derive(Template)]
#[template(path = "model_modal.html")]
pub struct ModelModalTemplate {
    /// The session the picker targets.
    pub session_id: String,
    /// The pre-rendered row fragments, already sorted/filtered by the
    /// shared builder.
    pub rows: Vec<String>,
    /// The current filter text (echoed back into the input).
    pub filter: String,
    /// The modal overlay's id.
    pub modal_id: &'static str,
    /// The filter input's id.
    pub filter_id: &'static str,
    /// The row container's id.
    pub rows_id: &'static str,
    /// The refresh button's id.
    pub refresh_id: &'static str,
    /// The attribute a button names its action with.
    pub action_attr: &'static str,
    /// The refresh action's contract name.
    pub refresh_action: &'static str,
}

impl ModelModalTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(session_id: String, rows: Vec<String>, filter: String) -> Self {
        Self {
            session_id,
            rows,
            filter,
            modal_id: contract::MODEL_PICKER_MODAL_ID,
            filter_id: contract::MODEL_PICKER_FILTER_ID,
            rows_id: contract::MODEL_PICKER_ROWS_ID,
            refresh_id: contract::MODEL_PICKER_REFRESH_ID,
            action_attr: contract::ACTION_DATA_ATTR,
            refresh_action: contract::ACTION_REFRESH_MODELS,
        }
    }
}

/// Renders a template to its HTML string.
///
/// # Errors
///
/// Returns the render error; askama render failures are template
/// bugs, so the caller maps them to a 500 with the sentence.
pub fn render<T: Template>(template: &T) -> Result<String, askama::Error> {
    template.render()
}
