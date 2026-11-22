#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::uninlined_format_args,
    reason = "test code"
)]
//! Tests for the shared tools-modal row vocabulary.

use jinn_bot_msg::BOT_TOOL_NAMES;
use jinn_core_types::FilterMode;
use jinn_core_types::NameFilter;
use jinn_core_types::ToolDefinition;

use crate::web_tool_rows::{
    WebToolRow, bot_sees_tool, committed_tool_filter, filter_rows, modal_rows,
};

/// Ordinary built-ins every session keeps, bot or not.
const ORDINARY_TOOL_NAMES: &[&str] = &["read", "bash", "grep"];

/// A minimal definition under `name`.
fn tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: format!("{name} description"),
        parameters: serde_json::json!({ "type": "object" }),
        prompt_snippet: None,
        prompt_guidelines: vec![],
        server_tool_type: None,
    }
}

/// One definition per name, in registry order.
fn defs(names: &[&str]) -> Vec<ToolDefinition> {
    names.iter().map(|name| tool(name)).collect()
}

/// A deny filter withholding `names`.
fn deny_filter(names: &[&str]) -> NameFilter {
    NameFilter::deny(names.iter().map(|s| (*s).to_owned()))
}

/// An allow filter permitting exactly `names`.
fn allow_filter(names: &[&str]) -> NameFilter {
    NameFilter {
        mode: FilterMode::Allow,
        names: names.iter().map(|s| (*s).to_owned()).collect(),
    }
}

// ---------------------------------------------------------------------------
// bot_sees_tool
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn a_non_bot_sees_none_of_the_bot_tools() {
    // Given a plain session and one definition per tool family.
    let definitions = defs(&["bash", BOT_TOOL_NAMES[0], BOT_TOOL_NAMES[1]]);

    // When asking whether the session sees each tool.
    let seen: Vec<bool> = definitions
        .iter()
        .map(|def| bot_sees_tool(false, false, def))
        .collect();

    // Then only the ordinary tool is seen.
    assert_eq!(seen, [true, false, false]);
}

#[rstest::rstest]
#[test]
fn an_attached_bot_sees_its_tools_and_the_ordinary_ones() {
    // Given a bot with a parent.
    let definitions = defs(&["bash", BOT_TOOL_NAMES[0], BOT_TOOL_NAMES[1]]);

    // When asking whether it sees each tool.
    let seen: Vec<bool> = definitions
        .iter()
        .map(|def| bot_sees_tool(true, true, def))
        .collect();

    // Then every tool is seen — the gate subtracts, it never substitutes.
    assert_eq!(seen, [true, true, true]);
}

#[rstest::rstest]
#[test]
fn a_standalone_bot_sees_conclude_but_not_notify_parent() {
    // Given a parentless bot.
    let definitions = defs(&["bash", BOT_TOOL_NAMES[0], BOT_TOOL_NAMES[1]]);

    // When asking whether it sees each tool.
    let seen: Vec<bool> = definitions
        .iter()
        .map(|def| bot_sees_tool(true, false, def))
        .collect();

    // Then `conclude` survives — recording a conclusion needs no parent —
    // and `notify_parent` is withheld: waking a parent that does not
    // exist is a call that can only fail.
    assert_eq!(seen, [true, true, false]);
}

// ---------------------------------------------------------------------------
// modal_rows
// ---------------------------------------------------------------------------

/// The names of `rows`, in display order.
fn names_of(rows: &[WebToolRow]) -> Vec<&str> {
    rows.iter().map(|row| row.name.as_str()).collect()
}

#[rstest::rstest]
#[test]
fn rows_sort_case_insensitively_by_name() {
    // Given definitions whose registry order is not display order and
    // mixes cases.
    let definitions = defs(&["Grep", "bash", "Apply_patch", "edit"]);

    // When building the modal rows.
    let rows = modal_rows(&definitions, "openrouter", false, false, None);

    // Then the rows sort by the lowercased key while each row keeps its
    // registry name's casing.
    assert_eq!(names_of(&rows), ["Apply_patch", "bash", "edit", "Grep"]);
}

#[rstest::rstest]
#[test]
fn rows_withhold_tools_the_provider_cannot_run() {
    // Given a definition the provider cannot run (web search off
    // non-OpenRouter) beside one it can.
    let mut definitions = defs(&["bash"]);
    definitions.push(ToolDefinition {
        server_tool_type: Some(jinn_core_types::tool_types::ServerToolType::OpenrouterWebSearch),
        ..tool("web_search")
    });

    // When building rows for a provider that lacks the server tool.
    let rows = modal_rows(&definitions, "ollama", false, false, None);

    // Then only the plain tool has a row.
    assert_eq!(names_of(&rows), ["bash"]);
}

#[rstest::rstest]
#[test]
fn rows_for_a_non_bot_never_include_bot_tools() {
    // Given the full registry including the bot family.
    let definitions = defs(
        ORDINARY_TOOL_NAMES
            .iter()
            .chain(BOT_TOOL_NAMES.iter())
            .copied()
            .collect::<Vec<_>>()
            .as_slice(),
    );

    // When building rows for a plain session.
    let rows = modal_rows(&definitions, "openrouter", false, false, None);

    // Then the bot tools have no rows and the ordinary ones do.
    let names = names_of(&rows);
    for name in BOT_TOOL_NAMES {
        assert!(
            !names.contains(name),
            "{name} must not be offered to a non-bot"
        );
    }
    for name in ORDINARY_TOOL_NAMES {
        assert!(names.contains(name), "{name} must remain");
    }
}

#[rstest::rstest]
#[test]
fn rows_for_a_standalone_bot_withhold_only_notify_parent() {
    // Given the full registry including the bot family.
    let definitions = defs(
        ORDINARY_TOOL_NAMES
            .iter()
            .chain(BOT_TOOL_NAMES.iter())
            .copied()
            .collect::<Vec<_>>()
            .as_slice(),
    );

    // When building rows for a parentless bot.
    let rows = modal_rows(&definitions, "openrouter", true, false, None);

    // Then `conclude` is offered and `notify_parent` is not.
    let names = names_of(&rows);
    assert!(names.contains(&"conclude"));
    assert!(!names.contains(&"notify_parent"));
}

#[rstest::rstest]
#[test]
fn rows_seed_the_enabled_mark_from_a_deny_filter() {
    // Given a session withholding one of two tools.
    let definitions = defs(&["bash", "read"]);
    let filter = deny_filter(&["read"]);

    // When building the modal rows.
    let rows = modal_rows(&definitions, "openrouter", false, false, Some(&filter));

    // Then the withheld tool's row renders off and the other on.
    let enabled_of = |name: &str| {
        rows.iter()
            .find(|row| row.name == name)
            .map(|row| row.enabled)
    };
    assert_eq!(enabled_of("read"), Some(false));
    assert_eq!(enabled_of("bash"), Some(true));
}

#[rstest::rstest]
#[test]
fn rows_seed_the_enabled_mark_from_an_allow_filter() {
    // Given an allow filter permitting only one of two tools.
    let definitions = defs(&["bash", "read"]);
    let filter = allow_filter(&["read"]);

    // When building the modal rows.
    let rows = modal_rows(&definitions, "openrouter", false, false, Some(&filter));

    // Then the unpermitted tool's row renders off — the live filter, not
    // its mode, decides the mark.
    let enabled_of = |name: &str| {
        rows.iter()
            .find(|row| row.name == name)
            .map(|row| row.enabled)
    };
    assert_eq!(enabled_of("read"), Some(true));
    assert_eq!(enabled_of("bash"), Some(false));
}

#[rstest::rstest]
#[test]
fn rows_render_everything_on_with_no_filter() {
    // Given two registered tools and no filter at all.
    let definitions = defs(&["bash", "read"]);

    // When building the modal rows.
    let rows = modal_rows(&definitions, "openrouter", false, false, None);

    // Then both render enabled — absence inherits, withholding nothing.
    assert!(rows.iter().all(|row| row.enabled));
}

#[rstest::rstest]
#[test]
fn rows_truncate_the_description_to_its_first_sentence() {
    // Given a definition whose description runs several sentences.
    let mut definitions = defs(&["bash"]);
    definitions[0].description =
        "Runs a shell command. Captures stdout and stderr. Times out after the configured limit."
            .to_owned();

    // When building the modal rows.
    let rows = modal_rows(&definitions, "openrouter", false, false, None);

    // Then the row carries only the first sentence — the summary stays
    // scannable in the modal.
    assert_eq!(rows[0].description, "Runs a shell command.");
}

#[rstest::rstest]
#[test]
fn rows_keep_a_single_sentence_description_whole() {
    // Given a definition whose description is one sentence with no
    // trailing period and no further sentences.
    let mut definitions = defs(&["bash"]);
    definitions[0].description = "Runs a shell command".to_owned();

    // When building the modal rows.
    let rows = modal_rows(&definitions, "openrouter", false, false, None);

    // Then the row carries the text verbatim — truncation never
    // damages a description that is already one sentence.
    assert_eq!(rows[0].description, "Runs a shell command");
}

#[rstest::rstest]
#[test]
fn rows_truncate_at_a_sentence_boundary_not_an_abbreviation() {
    // Given a description whose first sentence contains a decimal
    // number (a naive split on '.' would cut mid-sentence).
    let mut definitions = defs(&["bash"]);
    definitions[0].description =
        "Times out after 2.5 seconds by default. Override the limit per call.".to_owned();

    // When building the modal rows.
    let rows = modal_rows(&definitions, "openrouter", false, false, None);

    // Then the row carries through the real sentence end — the UAX#29
    // boundary, not the decimal point.
    assert_eq!(
        rows[0].description,
        "Times out after 2.5 seconds by default."
    );
}

#[rstest::rstest]
#[test]
fn rows_truncate_an_empty_description_to_empty() {
    // Given a definition with an empty description.
    let mut definitions = defs(&["bash"]);
    definitions[0].description = String::new();

    // When building the modal rows.
    let rows = modal_rows(&definitions, "openrouter", false, false, None);

    // Then the row's summary is empty — no panic, no fallback text.
    assert_eq!(rows[0].description, "");
}

// ---------------------------------------------------------------------------
// committed_tool_filter
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn commit_is_a_deny_filter_over_the_off_rows() {
    // Given rows where one tool renders off.
    let rows = modal_rows(&defs(&["bash", "read"]), "openrouter", false, false, None);
    let rows: Vec<WebToolRow> = rows
        .into_iter()
        .map(|row| {
            if row.name == "read" {
                WebToolRow {
                    enabled: false,
                    ..row
                }
            } else {
                row
            }
        })
        .collect();

    // When committing.
    let filter = committed_tool_filter(&rows);

    // Then the commit denies exactly the off row — a deny filter, never a
    // delta and never an allow-mode write.
    assert_eq!(filter, deny_filter(&["read"]));
}

#[rstest::rstest]
#[test]
fn committing_an_allow_mode_session_demotes_it_to_its_withheld_names() {
    // Given an allow-mode session's rendered rows: the unpermitted tool
    // renders off.
    let rows = modal_rows(
        &defs(&["bash", "read"]),
        "openrouter",
        false,
        false,
        Some(&allow_filter(&["read"])),
    );

    // When committing those rows untouched.
    let filter = committed_tool_filter(&rows);

    // Then the commit is a deny filter over exactly the name the allow
    // filter withheld — the TUI confirm's demotion, reproduced.
    assert_eq!(filter, deny_filter(&["bash"]));
}

#[rstest::rstest]
#[test]
fn committing_all_on_rows_yields_an_empty_deny_filter() {
    // Given rows where everything renders on.
    let rows = modal_rows(&defs(&["bash", "read"]), "openrouter", false, false, None);

    // When committing.
    let filter = committed_tool_filter(&rows);

    // Then the deny list is empty, which withholds nothing — the same
    // state the TUI confirm reaches from an untouched picker.
    assert!(filter.names.is_empty());
    assert_eq!(filter.mode, FilterMode::Deny);
}

// ---------------------------------------------------------------------------
// filter_rows — the web modal's shared fuzzy filter
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn filter_rows_with_empty_filter_passes_every_row_through_in_order() {
    // Given rows in their sorted order.
    let rows = modal_rows(&defs(&["bash", "grep", "read"]), "openrouter", false, false, None);

    // When filtering with an empty query.
    let filtered = filter_rows(&rows, "");

    // Then every row passes through in input order with no match
    // indices — nothing is highlighted on an empty filter.
    assert_eq!(filtered.len(), 3);
    assert!(filtered.iter().all(|row| row.match_indices.is_empty()));
    assert_eq!(filtered[0].row.name, "bash");
    assert_eq!(filtered[2].row.name, "read");
}

#[rstest::rstest]
#[test]
fn filter_rows_narrows_to_matching_rows_ordered_by_score() {
    // Given rows whose names share a query prefix to different degrees.
    let rows = modal_rows(
        &defs(&["read", "read_image", "grep"]),
        "openrouter",
        false,
        false,
        None,
    );

    // When filtering for "read".
    let filtered = filter_rows(&rows, "read");

    // Then only the two rows whose search text matches remain, ordered
    // by the shared core's score (exact name first), with match
    // indices present.
    let names: Vec<&str> = filtered.iter().map(|row| row.row.name.as_str()).collect();
    assert_eq!(names, vec!["read", "read_image"]);
    assert!(filtered.iter().all(|row| !row.match_indices.is_empty()));
}

#[rstest::rstest]
#[test]
fn filter_rows_matches_the_description_field_too() {
    // Given a row whose distinguishing text lives in its description.
    let rows = {
        let mut definitions = defs(&["alpha"]);
        definitions.push(ToolDefinition {
            name: "beta".to_owned(),
            description: "read a file from the workspace".to_owned(),
            parameters: serde_json::json!({ "type": "object" }),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: None,
        });
        modal_rows(&definitions, "openrouter", false, false, None)
    };

    // When filtering for the description-only word "workspace".
    let filtered = filter_rows(&rows, "workspace");

    // Then exactly the beta row remains — the search text spans both
    // fields, the TUI's `"{name} {description}"` closure.
    let names: Vec<&str> = filtered.iter().map(|row| row.row.name.as_str()).collect();
    assert_eq!(names, vec!["beta"]);
}

#[rstest::rstest]
#[test]
fn segments_split_the_match_indices_across_both_fields() {
    // Given a filtered row whose match spans the name and the
    // description.
    let rows = modal_rows(&defs(&["read"]), "openrouter", false, false, None);
    let filtered = filter_rows(&rows, "readd");
    let row = &filtered[0];

    // When splitting into per-field runs.
    let segments = row.row.segments(&row.match_indices);

    // Then the name's matched run covers the whole name, and the
    // description's runs reconstruct its text exactly with at least one
    // matched run — the indices remapped into description-local
    // offsets, wherever the match core placed them.
    let name_text: String = segments.name.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(name_text, "read");
    assert!(segments.name[0].matched);
    let desc_rebuilt: String = segments
        .description
        .iter()
        .map(|run| run.text.as_str())
        .collect();
    assert_eq!(desc_rebuilt, "read description");
    assert!(segments.description.iter().any(|run| run.matched));
}

#[rstest::rstest]
#[test]
fn segments_keep_the_separator_unhighlighted() {
    // Given a row and its search text — the separator offset (the
    // space at `name_len`) must belong to neither field.
    let rows = modal_rows(&defs(&["ab"]), "openrouter", false, false, None);
    let row = &rows[0];
    let name_len = row.name.len();

    // When splitting with only the separator offset marked.
    let segments = row.segments(&[name_len]);

    // Then neither field carries a matched run — the separator's
    // offset maps into neither the name's range nor the description's.
    assert!(segments.name.iter().all(|run| !run.matched));
    assert!(segments.description.iter().all(|run| !run.matched));
    // And both fields still reconstruct whole.
    let name_rebuilt: String = segments.name.iter().map(|run| run.text.as_str()).collect();
    let desc_rebuilt: String = segments
        .description
        .iter()
        .map(|run| run.text.as_str())
        .collect();
    assert_eq!(name_rebuilt, "ab");
    assert_eq!(desc_rebuilt, "ab description");
}
