//! The askama page and fragment templates.
//!
//! Compile-time-checked: a template naming a field the model lacks
//! fails the build, which is the reason this stack was chosen. The
//! page shell owns the SSE wiring (`sse-connect` / `sse-swap`
//! attributes — no custom JavaScript); every dynamic region is a
//! fragment template the handlers render and the SSE pushes replace.
//!
//! Escaping: askama escapes by extension — `.html` templates escape
//! every interpolation — so a session title carrying markup cannot
//! break the page. Pre-rendered HTML (the markdown bodies) rides
//! through askama's `|safe` filter *only* on fields the transcript
//! module produced, and that module is where the escaping contract
//! lives.

use askama::Template;
use jinn_webui_msg::SessionListItem;

/// The stylesheet, inlined into the page shell. Hand-written and small
/// enough that a separate asset (and a build.rs authored-file row)
/// would be ceremony, not structure.
pub const STYLE_CSS: &str = r#"
:root { --bg: #14161a; --fg: #d7dae0; --muted: #8a8f98; --accent: #4c8bd8;
        --border: #2a2e35; --panel: #1b1e24; --err: #e5534b; --ok: #57ab5a; }
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg);
       font: 15px/1.55 system-ui, sans-serif; display: flex; height: 100vh; }
#sidebar { width: 280px; min-width: 220px; border-right: 1px solid var(--border);
           background: var(--panel); display: flex; flex-direction: column; }
#sidebar h2 { font-size: 12px; text-transform: uppercase; letter-spacing: .08em;
              color: var(--muted); padding: 12px 14px 4px; margin: 0; }
#sessions { overflow-y: auto; flex: 1; }
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
    /// The sessions-list fragment, rendered inline.
    pub sessions_html: String,
    /// The transcript fragment, rendered inline.
    pub transcript_html: String,
    /// The composer-controls fragment, rendered inline.
    pub controls_html: String,
    /// The model-picker fragment, rendered inline.
    pub model_html: String,
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
}

/// The sessions fragment: the sidebar list.
#[derive(Template)]
#[template(path = "sessions.html")]
pub struct SessionsTemplate {
    /// One row per session, active first.
    pub rows: Vec<SessionListItem>,
}

/// The composer-controls fragment: the send↔cancel button.
#[derive(Template)]
#[template(path = "controls.html")]
pub struct ControlsTemplate {
    /// The session the composer targets.
    pub session_id: String,
    /// Whether the session is busy (true renders the cancel button).
    pub busy: bool,
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
