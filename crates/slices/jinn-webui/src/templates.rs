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
#page-root { display: flex; height: 100vh; }
#main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
#titlebar { padding: 10px 18px; border-bottom: 1px solid var(--border);
            display: flex; align-items: baseline; gap: 12px; }
#titlebar .t { font-weight: 600; }
#titlebar .m { color: var(--muted); font-size: 12px; }
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
    /// The model label.
    pub model: String,
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
    /// The model-picker fragment, rendered inline.
    pub model_html: String,
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
    /// The model-picker container id.
    pub model_picker_id: &'static str,
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
}

impl PageTemplate {
    /// The no-session landing page: sessions list only, every other
    /// region empty, cursor zero.
    #[must_use]
    pub fn landing(sessions_html: String) -> Self {
        Self {
            style: Box::leak(page_style().into_boxed_str()),
            session_id: String::new(),
            title: "jinn".to_owned(),
            model: String::new(),
            cursor: 0,
            sessions_html,
            transcript_html: String::new(),
            controls_html: String::new(),
            model_html: String::new(),
            vendor_prefix: contract::VENDOR_PREFIX,
            app_js_versioned: app_js_versioned_path(),
            page_root_id: contract::PAGE_ROOT_ID,
            page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
            sessions_id: contract::SESSIONS_ID,
            model_picker_id: contract::MODEL_PICKER_ID,
            transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
            transcript_id: contract::TRANSCRIPT_ID,
            cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
            controls_id: contract::CONTROLS_ID,
            session_title_id: "",
        }
    }

    /// A session page: every region filled for one viewed session at
    /// the cursor its inline transcript already carries.
    #[must_use]
    #[expect(clippy::too_many_arguments, reason = "one field per rendered region")]
    pub fn session(
        session_id: String,
        title: String,
        model: String,
        cursor: usize,
        sessions_html: String,
        transcript_html: String,
        controls_html: String,
        model_html: String,
    ) -> Self {
        Self {
            style: Box::leak(page_style().into_boxed_str()),
            session_id,
            title,
            model,
            cursor,
            sessions_html,
            transcript_html,
            controls_html,
            model_html,
            vendor_prefix: contract::VENDOR_PREFIX,
            app_js_versioned: app_js_versioned_path(),
            page_root_id: contract::PAGE_ROOT_ID,
            page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
            sessions_id: contract::SESSIONS_ID,
            model_picker_id: contract::MODEL_PICKER_ID,
            transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
            transcript_id: contract::TRANSCRIPT_ID,
            cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
            controls_id: contract::CONTROLS_ID,
            session_title_id: contract::SESSION_TITLE_ID,
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

/// The model-picker fragment.
#[derive(Template)]
#[template(path = "model.html")]
pub struct ModelTemplate {
    /// The session the picker targets.
    pub session_id: String,
    /// The current selection's label.
    pub current: String,
    /// The cache's model ids, sorted.
    pub models: Vec<String>,
    /// The attribute the select names its action with.
    pub action_attr: &'static str,
    /// The model action's contract name.
    pub model_action: &'static str,
}

impl ModelTemplate {
    /// Builds the template with the contract's hooks filled.
    #[must_use]
    pub fn with_contract(session_id: String, current: String, models: Vec<String>) -> Self {
        Self {
            session_id,
            current,
            models,
            action_attr: contract::ACTION_DATA_ATTR,
            model_action: contract::ACTION_MODEL,
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
