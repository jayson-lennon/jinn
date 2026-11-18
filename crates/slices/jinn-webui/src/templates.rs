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

/// The stylesheet, inlined into the page shell. Hand-written and small
/// enough that a separate asset (and a build.rs authored-file row)
/// would be ceremony, not structure.
pub const STYLE_CSS: &str = r#"
:root { --bg: #14161a; --fg: #d7dae0; --muted: #8a8f98; --accent: #4c8bd8;
        --border: #2a2e35; --panel: #1b1e24; --err: #e5534b; --ok: #57ab5a; }
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg);
       font: 15px/1.55 system-ui, sans-serif; }
#page-root { display: flex; height: 100vh; }
#sidebar { width: 280px; min-width: 220px; border-right: 1px solid var(--border);
           background: var(--panel); display: flex; flex-direction: column;
           min-height: 0; }
#sidebar h2 { font-size: 12px; text-transform: uppercase; letter-spacing: .08em;
              color: var(--muted); padding: 12px 14px 4px; margin: 0; }
#sessions { overflow-y: auto; flex: 1; min-height: 0; }
.srow { display: block; padding: 8px 14px; color: var(--fg); text-decoration: none;
        border-bottom: 1px solid var(--border); cursor: pointer; }
.srow:hover { background: #23272f; }
.srow.active { border-left: 3px solid var(--accent); background: #20242c; }
.srow .stitle { display: block; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.srow .smeta { font-size: 11px; color: var(--muted); }
.srow.bot .smeta::after { content: " · bot"; color: #d4a72c; }
#main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
#titlebar { padding: 10px 18px; border-bottom: 1px solid var(--border);
            display: flex; align-items: baseline; gap: 12px; }
#titlebar .t { font-weight: 600; }
#titlebar .m { color: var(--muted); font-size: 12px; }
#transcript-wrap { flex: 1; overflow-y: auto; padding: 8px 0; }
.entry { padding: 6px 18px; }
.entry.user { border-left: 3px solid var(--accent); background: #181b21;
              margin: 8px 12px; border-radius: 6px; padding: 10px 14px; }
.entry .who { font-size: 11px; text-transform: uppercase; letter-spacing: .06em;
              color: var(--muted); margin-bottom: 2px; }
.entry.error { color: var(--err); }
.entry.system, .entry.compaction, .entry.annotation,
.entry.rule_interrupt, .entry.intersession { color: var(--muted); font-size: 13px; }
.entry pre { background: #101216; padding: 10px; border-radius: 6px;
             overflow-x: auto; white-space: pre-wrap; word-break: break-word; }
.entry code { background: #101216; padding: 1px 5px; border-radius: 4px; }
.entry table { border-collapse: collapse; }
.entry td, .entry th { border: 1px solid var(--border); padding: 4px 8px; }
details.disclosure { border: 1px solid var(--border); border-radius: 6px;
                     margin: 4px 0; background: var(--panel); }
details.disclosure > summary { cursor: pointer; padding: 6px 12px; color: var(--muted);
                               font-size: 13px; list-style: none; }
details.disclosure > summary::before { content: "▸ "; }
details.disclosure[open] > summary::before { content: "▾ "; }
details.disclosure .body { padding: 4px 12px 10px; white-space: pre-wrap;
                           word-break: break-word; font-size: 13px;
                           font-family: ui-monospace, monospace; }
.badge { font-size: 11px; padding: 1px 7px; border-radius: 9px; margin-left: 8px; }
.badge.success { background: #244; color: var(--ok); }
.badge.failure { background: #422; color: var(--err); }
.badge.pending { background: #334; color: var(--muted); }
#streaming .tok-thinking { color: var(--muted); font-style: italic; }
.busy-spinner { width: 18px; height: 18px; margin: 0 auto 8px; border: 2px solid var(--border);
                border-top-color: var(--accent); border-radius: 50%;
                animation: webui-spin .8s linear infinite; }
@keyframes webui-spin { to { transform: rotate(360deg); } }
#composer { border-top: 1px solid var(--border); padding: 12px 18px; display: flex; gap: 10px; }
#composer textarea { flex: 1; background: var(--panel); color: var(--fg);
                     border: 1px solid var(--border); border-radius: 6px;
                     padding: 8px 10px; font: inherit; resize: none; height: 64px; }
#composer button { background: var(--accent); color: #fff; border: 0; border-radius: 6px;
                   padding: 0 22px; font: inherit; cursor: pointer; }
#composer button.cancel { background: var(--err); }
.model-picker { margin-left: auto; }
.model-picker select { background: var(--panel); color: var(--fg);
                       border: 1px solid var(--border); border-radius: 6px;
                       padding: 6px 8px; font: inherit; max-width: 260px; }
"#;

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
    /// The client bundle's filename inside `/vendor/`.
    pub app_js: &'static str,
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
}

impl PageTemplate {
    /// The no-session landing page: sessions list only, every other
    /// region empty, cursor zero.
    #[must_use]
    pub fn landing(sessions_html: String) -> Self {
        Self {
            style: STYLE_CSS,
            session_id: String::new(),
            title: "jinn".to_owned(),
            model: String::new(),
            cursor: 0,
            sessions_html,
            transcript_html: String::new(),
            controls_html: String::new(),
            model_html: String::new(),
            vendor_prefix: contract::VENDOR_PREFIX,
            app_js: contract::VENDOR_APP_JS,
            page_root_id: contract::PAGE_ROOT_ID,
            page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
            sessions_id: contract::SESSIONS_ID,
            model_picker_id: contract::MODEL_PICKER_ID,
            transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
            transcript_id: contract::TRANSCRIPT_ID,
            cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
            controls_id: contract::CONTROLS_ID,
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
            style: STYLE_CSS,
            session_id,
            title,
            model,
            cursor,
            sessions_html,
            transcript_html,
            controls_html,
            model_html,
            vendor_prefix: contract::VENDOR_PREFIX,
            app_js: contract::VENDOR_APP_JS,
            page_root_id: contract::PAGE_ROOT_ID,
            page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
            sessions_id: contract::SESSIONS_ID,
            model_picker_id: contract::MODEL_PICKER_ID,
            transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
            transcript_id: contract::TRANSCRIPT_ID,
            cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
            controls_id: contract::CONTROLS_ID,
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
