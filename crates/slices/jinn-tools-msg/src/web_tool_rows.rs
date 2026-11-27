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
use jinn_selection_widget::split_matched_runs;
use unicode_segmentation::UnicodeSegmentation;

/// One tool row ready for display, presentation-free.
///
/// `enabled` is derived from the session's live tool filter at row build,
/// the same predicate the prompt's gate applies — a row the filter withholds
/// renders off, so committing the rendered state reproduces the filter.
/// `description` is the definition description's **first sentence**: the
/// row summary stays scannable when a definition carries a multi-sentence
/// brief, and the toggle semantics never read the description — the name
/// is the identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebToolRow {
    /// The tool's name — the identity a toggle POST validates against the
    /// rendered rows.
    pub name: String,
    /// The description's first sentence, shown beside the name.
    pub description: String,
    /// Whether the session's live filter currently permits the tool.
    pub enabled: bool,
}

/// The first UAX#29 sentence of `description`, verbatim — the row
/// summary's scannable form. Splitting rides `unicode-segmentation`'s
/// sentence boundaries (never a manual split on `.`, which would cut
/// abbreviations and decimals in half). A description whose text yields
/// no sentence (empty, or no alphanumeric content) truncates to the
/// empty string rather than the whole text.
#[must_use]
fn first_sentence(description: &str) -> String {
    description
        .split_sentence_bounds()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned()
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
            description: first_sentence(&def.description),
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

impl WebToolRow {
    /// The row's search text: name then description — the TUI tool
    /// picker's own search closure (`"{name} {description}"`), so a query
    /// filters the `/web2` tool picker identically.
    #[must_use]
    pub fn search_text(&self) -> String {
        format!("{} {}", self.name, self.description)
    }

    /// Splits the row's two text fields into matched/plain runs from the
    /// filter's match indices.
    ///
    /// The indices are byte offsets into [`Self::search_text`] (name
    /// occupies `[0, name_len)`, the single-byte space separator sits at
    /// `name_len`, the description starts at `name_len + 1`); offsets on
    /// the separator belong to neither field. Runs cut on grapheme
    /// boundaries via the shared splitter.
    #[must_use]
    pub fn segments(&self, match_indices: &[usize]) -> WebToolRowSegments {
        let name_len = self.name.len();
        let desc_offset = name_len.saturating_add(1); // the space separator
        let name_hits: Vec<usize> = match_indices
            .iter()
            .copied()
            .filter(|&i| i < name_len)
            .collect();
        let desc_hits: Vec<usize> = match_indices
            .iter()
            .copied()
            .filter_map(|i| i.checked_sub(desc_offset))
            .collect();
        WebToolRowSegments {
            name: split_matched_runs(&self.name, &name_hits),
            description: split_matched_runs(&self.description, &desc_hits),
        }
    }
}

/// One row's two text fields, each split into matched/plain runs.
///
/// Presentation-free: the web stamps `<mark>` on matched runs, the TUI
/// stamps a highlight background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebToolRowSegments {
    /// The name field's runs, in display order.
    pub name: Vec<jinn_selection_widget::TextRun>,
    /// The description field's runs, in display order.
    pub description: Vec<jinn_selection_widget::TextRun>,
}

/// One fuzzy-filtered tool row: the row plus where its search text
/// matched.
#[derive(Debug, Clone)]
pub struct FilteredWebToolRow {
    /// The matched row, in filter display order.
    pub row: WebToolRow,
    /// Sorted, deduplicated byte offsets into the row's search text
    /// that the filter matched. Empty when the filter is empty.
    pub match_indices: Vec<usize>,
}

/// The shared fuzzy filter over tool rows — the same match core the TUI
/// picker routes through.
///
/// Every whitespace-separated term of `filter` must fuzzy-match the
/// row's search text (`jinn_selection_widget::fuzzy_filter`,
/// [`SkimMatcherV2`] with per-term AND logic); matches order by
/// cumulative score descending, ties by input order. An empty filter
/// returns the rows unchanged (the caller's sort order) with no ranges.
#[must_use]
pub fn filter_rows(rows: &[WebToolRow], filter: &str) -> Vec<FilteredWebToolRow> {
    if filter.is_empty() {
        return rows
            .iter()
            .map(|row| FilteredWebToolRow {
                row: row.clone(),
                match_indices: Vec::new(),
            })
            .collect();
    }
    let labels: Vec<String> = rows.iter().map(WebToolRow::search_text).collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    jinn_selection_widget::fuzzy_filter(&label_refs, filter)
        .into_iter()
        .filter_map(|matched| {
            rows.get(matched.index).map(|row| FilteredWebToolRow {
                row: row.clone(),
                match_indices: matched.match_indices,
            })
        })
        .collect()
}

#[cfg(test)]
mod web_tool_rows_tests;
