//! The tools modal's shared row vocabulary — the wire-safe seed a
//! non-TUI surface renders.
//!
//! The TUI tool picker's [`crate::ToolEntry`] is render-bound: it carries a
//! theme and ratatui machinery. This module is the presentation-free
//! counterpart, the [`crate::persona_rows`] precedent applied to tools: the
//! row type, the gating (provider + bot), the sort, and the enablement
//! derivation are defined once here, so a second surface cannot re-derive
//! them differently from the prompt's own rules.
//!
//! The bot gate is hoisted here too — it was already duplicated between the
//! TUI picker and the prompt assembler, and one definition cannot drift
//! across three consumers.

use jinn_bot_msg::is_bot_tool_definition;
use jinn_core_types::NameFilter;
use jinn_core_types::ToolDefinition;

/// One tool row ready for display, presentation-free.
///
/// `enabled` is derived from the session's live tool filter at row build,
/// the same predicate the prompt's gate applies — a row the filter withholds
/// renders off, so committing the rendered state reproduces the filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebToolRow {
    /// The tool's name — the identity a toggle POST validates against the
    /// rendered rows.
    pub name: String,
    /// Human-readable description shown beside the name.
    pub description: String,
    /// Whether the session's live filter currently permits the tool.
    pub enabled: bool,
}

/// Whether a session with the given bot facts is offered tool definition
/// `def`.
///
/// The single production definition of the gate, shared by the prompt
/// assembler, the TUI tool picker, and the web tools modal: a non-bot sees
/// none of the bot-only tools; a bot keeps every ordinary tool and, of its
/// own family, sees `conclude` always — recording a conclusion needs no
/// parent — and `notify_parent` only when it has one to notify. The gate is
/// a subtraction, never a substitution.
#[must_use]
pub fn bot_sees_tool(is_bot: bool, bot_has_parent: bool, def: &ToolDefinition) -> bool {
    if !is_bot {
        return !is_bot_tool_definition(def);
    }
    if !is_bot_tool_definition(def) {
        return true;
    }
    bot_has_parent || def.name == "conclude"
}

/// Builds the rows a tools modal renders for one session.
///
/// `definitions` are the session's registry tools (global merged with
/// per-session overrides, provider opt-outs already withheld — what
/// `ToolRegistry::tools_for_session` returns). Two more gates apply, both
/// "may this session use this tool": the provider gate for server tools,
/// and [`bot_sees_tool`] for the tools that only exist inside a bot. A row
/// the session cannot use is not something to toggle — offering it invites
/// a toggle that changes nothing.
///
/// Rows sort case-insensitively by name so the list is in a stable order
/// regardless of registry hash order, and `enabled` reads the session's
/// live filter — absent filter permits everything.
#[must_use]
pub fn modal_rows(
    definitions: &[ToolDefinition],
    provider_name: &str,
    is_bot: bool,
    bot_has_parent: bool,
    tool_filter: Option<&NameFilter>,
) -> Vec<WebToolRow> {
    let mut rows: Vec<WebToolRow> = definitions
        .iter()
        .filter(|def| def.available_for_provider(provider_name))
        .filter(|def| bot_sees_tool(is_bot, bot_has_parent, def))
        .map(|def| WebToolRow {
            name: def.name.clone(),
            description: def.description.clone(),
            enabled: tool_filter.is_none_or(|filter| filter.permits(&def.name)),
        })
        .collect();
    rows.sort_by_key(|row| row.name.to_lowercase());
    rows
}

/// The filter committing `rows` writes: a deny filter over every row now
/// rendering off.
///
/// This is the TUI confirm's semantic applied to a row set: the picker is a
/// blocklist editor by construction, so a commit is deny-over-off-rows —
/// never a delta, never an allow-mode write. Rows carry their open-time
/// enablement, so committing a session that held an allow filter narrows it
/// to that mode's withheld names (plus or minus the row the user flipped),
/// exactly as the TUI confirm demotes one.
#[must_use]
pub fn committed_tool_filter(rows: &[WebToolRow]) -> NameFilter {
    NameFilter::deny(
        rows.iter()
            .filter(|row| !row.enabled)
            .map(|row| row.name.clone()),
    )
}

#[cfg(test)]
mod web_tool_rows_tests;
