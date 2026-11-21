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

use crate::web_tool_rows::{WebToolRow, bot_sees_tool, committed_tool_filter, modal_rows};

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
