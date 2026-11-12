#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jinn_testutil::{buffer_row, volume_root};

use crate::element::StatusBarElement;
use jinn_core_types::WorkingInterval;
use jinn_core_types::model_selection::{AlloyStrategy, ModelSelection};
use jinn_kernel::AppState;
use jinn_kernel::common::render_test::{render_element, render_element_with_slices, seed_cell};
use jinn_kernel::common::ui_element::UiElement;
use jinn_token_count_msg::TokenRecord;
use jinn_work_time_msg::{WorkingTimeState, work_time_slot};

#[rstest::rstest]
fn name_returns_status_bar() {
    // Given the status bar element.
    let element = StatusBarElement;

    // When reading its name.
    let element_name = element.name();

    // Then the name identifies the status bar.
    assert_eq!(element_name, "status-bar");
}

#[rstest::rstest]
fn render_shows_no_model_selected_when_unset() {
    // Given a state with no model selected.
    let mut element = StatusBarElement;
    let state = AppState::default();

    // When rendering.
    let terminal = render_element(&mut element, &state, 50, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);

    // Then the info line opens with the indicator arrow.
    assert!(row.starts_with("\u{2191}"));
    // And names the missing model.
    assert!(row.contains("no model selected"));
}

#[rstest::rstest]
fn render_shows_status_hint_instead_of_model_when_set() {
    // Given a status-bar cell carrying a hint.
    let mut element = StatusBarElement;
    let state = AppState::default();
    let slices = jinn_slices::Slices::new();
    {
        let cell = seed_cell(
            &slices,
            &jinn_status_bar_msg::status_bar_slot(),
            jinn_status_bar_msg::StatusBarState {
                hint: Some("that session has no live terminal".to_owned()),
            },
        );
        cell.update(|s| s.hint = Some("that session has no live terminal".to_owned()));
    }

    // When rendering.
    let terminal = render_element_with_slices(&mut element, &state, &slices, 60, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 60);

    // Then the hint replaces the model display.
    assert!(row.contains("no live terminal"), "row: {row}");
    // And the model is not shown while a hint is up.
    assert!(!row.contains("no model selected"), "row: {row}");
}

#[rstest::rstest]
fn render_short_hint_leaves_no_model_residue() {
    // Given a status-bar cell carrying a hint SHORTER than the model string,
    // on a state whose model is set to a long provider/model path.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Single(
        "openrouter/anthropic/claude-sonnet-4.5".to_owned(),
    ));
    let slices = jinn_slices::Slices::new();
    {
        let cell = seed_cell(
            &slices,
            &jinn_status_bar_msg::status_bar_slot(),
            jinn_status_bar_msg::StatusBarState::default(),
        );
        cell.update(|s| s.hint = Some("yanked 3 lines".to_owned()));
    }

    // When rendering.
    let terminal = render_element_with_slices(&mut element, &state, &slices, 60, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 60);

    // Then the hint replaces the model entirely — no fragment of the longer
    // model string survives to the left of the hint. Hint and model are one
    // either/or widget; the right-aligned hint overwrites only its own
    // columns, so an overlay instead of a replacement would leave
    // "(openrouter)yanked 3 lines" on the row.
    assert!(row.contains("yanked 3 lines"), "row: {row}");
    assert!(
        !row.contains("(openrouter)") && !row.contains("claude"),
        "a short hint must leave no model residue, got: {row}"
    );
}

#[rstest::rstest]
fn render_defaults_to_model_when_cell_absent() {
    // Given a state whose status-bar cell was never registered (slice not
    // activated) and no model selected.
    let mut element = StatusBarElement;
    let state = AppState::default();

    // When rendering.
    let terminal = render_element(&mut element, &state, 60, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 60);

    // Then the model display shows (the no-hint default).
    assert!(row.contains("no model selected"), "row: {row}");
}

#[rstest::rstest]
fn render_shows_provider_and_model() {
    // Given a session with a single model selected.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // When rendering.
    let terminal = render_element(&mut element, &state, 50, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);

    // Then the info line opens with the indicator arrow.
    assert!(row.starts_with("\u{2191}"));
    // And shows the provider in parentheses before the model.
    assert!(row.contains("(ollama)/llama3"));
}

#[rstest::rstest]
fn render_single_model_ignores_stale_ledger_model_used() {
    // Given a Single selection of ollama/llama3, but a token ledger whose last
    // record claims a different (stale) model was dispatched.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: Some("openrouter/gpt-4".to_owned()),
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1000,
        tokens_received: 500,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });

    // When rendering.
    let terminal = render_element(&mut element, &state, 50, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);

    // Then the selected model is shown, not the stale dispatched model.
    assert!(
        row.contains("(ollama)/llama3"),
        "should show selected model, got: {row}"
    );
    assert!(
        !row.contains("gpt-4"),
        "should not show stale dispatched model, got: {row}"
    );
}

#[rstest::rstest]
fn render_right_aligns_text() {
    // Given a session with a single model selected, in a 50-column terminal.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // When rendering.
    let terminal = render_element(&mut element, &state, 50, 2);
    let buffer = terminal.backend().buffer().clone();

    // Then the info line (row 1) is right-aligned, starting with the indicator
    // arrow and ending with the scrollbar handle.
    let first = buffer.cell((0, 1)).expect("first cell");
    assert_eq!(first.symbol(), "\u{2191}");
    let last = buffer.cell((49, 1)).expect("last cell");
    assert_eq!(last.symbol(), ">");
}

#[rstest::rstest]
fn render_shows_provider_with_slash_in_model() {
    // Given a session whose model id itself contains a slash.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Single(
        "openrouter/anthropic/claude-sonnet-4".to_owned(),
    ));

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the info line opens with the indicator arrow.
    assert!(row.starts_with("\u{2191}"));
    // And only the FIRST slash splits provider from model, so the rest of the
    // model path survives intact.
    assert!(row.contains("(openrouter)/anthropic/claude-sonnet-4"));
}

#[rstest::rstest]
fn render_shows_token_counts_with_zero_values() {
    // Given a state with no token records.
    let mut element = StatusBarElement;
    let state = AppState::default();

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the status bar shows zero token counts.
    assert!(row.contains("\u{2191}0 \u{2193}0"));
    // And cost is always shown as $0.00000.
    assert!(row.contains("$0.00000"));
}

#[rstest::rstest]
fn render_shows_token_counts_with_values() {
    // Given a session with token records.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1500,
        tokens_received: 750,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the status bar shows token counts.
    assert!(row.contains("1.5k"));
    assert!(row.contains("750"));
    // And cost is shown as $0.00000 when no cost data.
    assert!(row.contains("$0.00000"));
}

#[rstest::rstest]
fn render_shows_cache_percent_when_cached_tokens_present() {
    // Given a session with a measured turn reporting cache hits.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("openrouter/auto".to_owned()));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1000,
        tokens_received: 50,
        cost: None,
        prompt_tokens: Some(1000),
        cached_tokens: Some(400),
    });

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the cache glyph + 40% appears leftmost.
    assert!(row.contains("\u{2B22} 40%"));
}

/// Build an `AppState` whose active session has one measured turn reporting
/// the given provider prompt and cached token counts.
fn state_with_measured_cache(prompt_tokens: u32, cached_tokens: u32) -> AppState {
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("openrouter/auto".to_owned()));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: prompt_tokens,
        tokens_received: 50,
        cost: None,
        prompt_tokens: Some(prompt_tokens),
        cached_tokens: Some(cached_tokens),
    });
    state
}

/// Render the status bar and return the cell carrying the cache glyph.
///
/// Tests using this helper run with a single session, so the tree aggregate
/// is hidden and the info line holds the only glyph in the buffer.
fn info_line_cache_cell(terminal: &ratatui::backend::TestBackend) -> ratatui::buffer::Cell {
    terminal
        .buffer()
        .content()
        .iter()
        .find(|cell| cell.symbol().starts_with('\u{2B22}'))
        .cloned()
        .expect("cache glyph should be rendered")
}

#[rstest::rstest]
#[case(400, "\u{2B22} 40%")]
// 894/1000 = 89.4% raw — rounds DOWN to a displayed 89%, staying in the error
// band one tick below the warning boundary.
#[case(894, "\u{2B22} 89%")]
fn render_info_line_cache_segment_is_error_below_90_percent(
    #[case] cached_tokens: u32,
    #[case] expected_text: &str,
) {
    // Given a measured turn displaying below 90%.
    let mut element = StatusBarElement;
    let state = state_with_measured_cache(1000, cached_tokens);

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);

    // Then the cache segment reads its percentage and carries the error color.
    let row = buffer_row(terminal.backend().buffer(), 1, 80);
    assert!(row.contains(expected_text), "got: {row}");
    let cell = info_line_cache_cell(terminal.backend());
    assert_eq!(
        cell.style().fg,
        Some(state.frontend.theme.error_text),
        "cache segment below 90% should use error_text"
    );
}

#[rstest::rstest]
#[case(960, "\u{2B22} 96%")]
// 946/1000 = 94.6% raw — must DISPLAY as 95% and therefore band as success
// (bands apply to the rounded display value, not the raw ratio).
#[case(946, "\u{2B22} 95%")]
fn render_info_line_cache_segment_is_success_at_or_above_95_percent(
    #[case] cached_tokens: u32,
    #[case] expected_text: &str,
) {
    // Given a measured turn reporting cache hits at or above 95% after rounding.
    let mut element = StatusBarElement;
    let state = state_with_measured_cache(1000, cached_tokens);

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);

    // Then the segment shows the ROUNDED percentage and carries the success color.
    let row = buffer_row(terminal.backend().buffer(), 1, 80);
    assert!(row.contains(expected_text), "got: {row}");
    let cell = info_line_cache_cell(terminal.backend());
    assert_eq!(
        cell.style().fg,
        Some(state.frontend.theme.success),
        "cache segment displaying 95%+ should use success"
    );
}

#[rstest::rstest]
#[case(900, "\u{2B22} 90%")]
// 895/1000 = 89.5% raw — f64 round is half-away-from-zero, so it DISPLAYS as
// 90% and therefore bands as warning.
#[case(895, "\u{2B22} 90%")]
fn render_info_line_cache_segment_is_warning_between_90_and_94_percent(
    #[case] cached_tokens: u32,
    #[case] expected_text: &str,
) {
    // Given a measured turn displaying between 90% and 94% inclusive.
    let mut element = StatusBarElement;
    let state = state_with_measured_cache(1000, cached_tokens);

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);

    // Then the cache segment reads its percentage and carries the warning color.
    let row = buffer_row(terminal.backend().buffer(), 1, 80);
    assert!(row.contains(expected_text), "got: {row}");
    let cell = info_line_cache_cell(terminal.backend());
    assert_eq!(
        cell.style().fg,
        Some(state.frontend.theme.warning),
        "cache segment displaying 90-94% should use warning"
    );
}

#[rstest::rstest]
fn render_hides_cache_glyph_when_no_cached_tokens() {
    // Given a session with no cache hits (cached_tokens = None).
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("openrouter/auto".to_owned()));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1000,
        tokens_received: 50,
        cost: None,
        prompt_tokens: Some(1000),
        cached_tokens: None,
    });

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then no cache glyph appears.
    assert!(!row.contains('\u{2B22}'));
}

#[rstest::rstest]
fn render_cache_percent_uses_measured_turns_only() {
    // Given a session with one measured turn (prompt=1000, cached=400) and one
    // cancelled turn (estimate=50, no usage). 400/1000 = 40%, not affected by
    // the cancelled turn's estimate.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("openrouter/auto".to_owned()));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1000,
        tokens_received: 50,
        cost: None,
        prompt_tokens: Some(1000),
        cached_tokens: Some(400),
    });
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 50,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the percentage is 40% (measured only), not 38% (1050 denominator).
    assert!(row.contains("\u{2B22} 40%"));
}

#[rstest::rstest]
fn render_shows_zero_percent_max_when_context_size_but_no_limit() {
    // Given a session with a cached context size but no model cache.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 5000,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });
    state.active_session_mut().set_context_sizes(5000, 0);

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the status bar shows usage with unknown limit (no context_length available).
    assert!(row.contains("5.0k/???"), "expected 5.0k/???, got: {row}");
}

#[rstest::rstest]
fn render_shows_zero_percent_max_when_no_context_size() {
    // Given a session with no cached context size.
    let mut element = StatusBarElement;
    let state = AppState::default();

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the context display shows 0/??? as fallback (no context_size, no model cache).
    assert!(row.contains("0/???"), "expected 0/??? fallback, got: {row}");
    // And token counts are still shown.
    assert!(row.contains("0 0") || row.contains("\u{2191}0 \u{2193}0"));
}

#[rstest::rstest]
fn render_shows_zero_turns_when_no_history() {
    // Given a state with no chat entries.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the status bar shows "Turns: 0".
    assert!(row.contains("\u{21BB}0"));
}

#[rstest::rstest]
fn render_shows_turn_count_with_history() {
    // Given a state with user and assistant entries.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state
        .active_session_mut()
        .push_entry(jinn_kernel::protocol::ChatEntry::user("hello"));
    state
        .active_session_mut()
        .push_entry(jinn_kernel::protocol::ChatEntry::assistant("hi there"));
    state
        .active_session_mut()
        .push_entry(jinn_kernel::protocol::ChatEntry::user("how are you?"));
    state
        .active_session_mut()
        .push_entry(jinn_kernel::protocol::ChatEntry::assistant("doing well"));

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the status bar shows "Turns: 4".
    assert!(row.contains("\u{21BB}4"));
}

#[rstest::rstest]
fn render_turn_count_skips_tool_loop_intermediates() {
    // Given a state with a tool-loop conversation.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state
        .active_session_mut()
        .push_entry(jinn_kernel::protocol::ChatEntry::user("fix the bug"));
    state
        .active_session_mut()
        .push_entry(jinn_kernel::protocol::ChatEntry::assistant("let me check"));
    state
        .active_session_mut()
        .push_entry(jinn_kernel::protocol::ChatEntry::tool_call(
            "id-1",
            "bash",
            r#"{"command":"ls"}"#,
        ));
    state
        .active_session_mut()
        .push_entry(jinn_kernel::protocol::ChatEntry::assistant("fixed it"));

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then only the user and final assistant count as turns.
    assert!(row.contains("\u{21BB}2"));
}

#[rstest::rstest]
fn render_shows_cwd_on_first_line() {
    // Given default state (cwd is "." which resolves to the current dir).
    let mut element = StatusBarElement;
    let state = AppState::default();

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();

    // Then the first row carries the cwd and is not empty.
    let row0 = buffer_row(&buffer, 0, 80);
    assert!(!row0.trim().is_empty(), "cwd line should not be empty");
}

#[rstest::rstest]
fn render_shows_absolute_path_for_non_home_cwd() {
    // Given a session with a non-home CWD. The path is rooted at the temp
    // volume's root so it lies outside the user's home directory on every
    // platform — the platform temp directory is under the user profile on
    // Windows, so a temp dir itself would be tilde-compressed there.
    let cwd = volume_root()
        .join_all(["test-project"])
        .expect("root joins a bare name");
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state.active_session_mut().set_cwd(cwd.clone());

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();

    // Then the cwd line (row 0) shows the full absolute path, with no tilde.
    let cwd_label = cwd.to_string();
    let row0 = buffer_row(&buffer, 0, 80);
    assert!(
        row0.contains(cwd_label.as_str()),
        "expected the cwd in row 0, got: {row0}"
    );
}

#[rstest::rstest]
fn render_shows_tilde_for_home_cwd() {
    // Given a session whose CWD is the user's home directory.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    let home = marked_path::MarkedPathBuf::<marked_path::Absolute>::new(
        dirs::home_dir().expect("home dir exists"),
    )
    .expect("home is absolute");
    state.active_session_mut().set_cwd(home);

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();

    // Then the cwd line collapses to just "~".
    let row0 = buffer_row(&buffer, 0, 80);
    assert!(
        row0.trim_start().starts_with('~'),
        "expected ~ in cwd line, got: {row0}"
    );
}

#[rstest::rstest]
fn render_shows_tilde_substitution_for_path_under_home() {
    // Given a session whose CWD is under the user's home directory.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    let home = marked_path::MarkedPathBuf::<marked_path::Absolute>::new(
        dirs::home_dir().expect("home dir exists"),
    )
    .expect("home is absolute");
    state.active_session_mut().set_cwd(
        home.join("projects/my-app")
            .expect("typed home joins a bare name"),
    );

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();

    // Then only the home-directory prefix is replaced by "~", with the
    // platform's native separators after it (PathBuf-built expectation).
    let expected = std::path::Path::new("~")
        .join("projects")
        .join("my-app")
        .display()
        .to_string();
    let row0 = buffer_row(&buffer, 0, 80);
    assert!(
        row0.trim_start().starts_with(&expected),
        "expected {expected} in cwd line, got: {row0}"
    );
}

/// Attach a model cache to the state's provider cell, holding exactly one model
/// under `provider` whose id is `model_id` and whose context length is
/// `context_length`.
fn attach_model_cache(
    state: &mut AppState,
    provider: &str,
    model_id: &str,
    context_length: Option<u32>,
) {
    let mut entries = std::collections::HashMap::new();
    entries.insert(
        provider.to_owned(),
        vec![jinn_provider_config::ModelInfo {
            id: model_id.to_owned(),
            context_length,
            input_modalities: jinn_provider_config::InputModalities::text(),
        }],
    );
    state
        .provider_state()
        .expect("provider cell attached")
        .update(|cell| {
            cell.model_cache = Some(jinn_provider_config::ModelCache {
                entries,
                last_updated_at: None,
            });
        });
}

#[rstest::rstest]
fn render_shows_context_limit_with_usage_and_percentage() {
    // Given a session with a cached context size and a model cache with context_length.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Single(
        "openrouter/anthropic/claude-sonnet-4".to_owned(),
    ));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 5000,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });
    state.active_session_mut().set_context_sizes(5000, 0);
    attach_model_cache(
        &mut state,
        "openrouter",
        "anthropic/claude-sonnet-4",
        Some(200_000),
    );

    // When rendering.
    let terminal = render_element(&mut element, &state, 100, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 100);

    // Then the status bar shows the percentage and formatted max.
    assert!(row.contains("2.5%/200k"), "expected 2.5%/200k, got: {row}");
}

#[rstest::rstest]
fn render_falls_back_to_last_ledger_tokens_when_no_cached_context_size() {
    // Given a session with a ledger but no cached context size — a background
    // session the context-size actor has never recalculated.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Single(
        "openrouter/anthropic/claude-sonnet-4".to_owned(),
    ));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 10_000,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });
    // No set_context_sizes — the fallback must supply the used side.
    attach_model_cache(
        &mut state,
        "openrouter",
        "anthropic/claude-sonnet-4",
        Some(200_000),
    );

    // When rendering.
    let terminal = render_element(&mut element, &state, 100, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 100);

    // Then the percentage derives from the last record's tokens_sent, not a
    // ledger sum: 10_000/200_000 = 5.0%, where summing two turns of 10k would
    // claim double.
    assert!(row.contains("5.0%/200k"), "expected 5.0%/200k, got: {row}");
}

#[rstest::rstest]
fn render_uses_the_last_ledger_record_not_a_sum() {
    // Given a session whose ledger grows per turn — each record carries that
    // dispatch's whole context, so record two (20k) is the current figure and
    // 10k + 20k would be wrong.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Single(
        "openrouter/anthropic/claude-sonnet-4".to_owned(),
    ));
    for tokens_sent in [10_000, 20_000] {
        state.active_session_mut().push_token_record(TokenRecord {
            model_used: None,
            timestamp: jiff::Timestamp::now(),
            tokens_sent,
            tokens_received: 0,
            cost: None,
            prompt_tokens: None,
            cached_tokens: None,
        });
    }
    attach_model_cache(
        &mut state,
        "openrouter",
        "anthropic/claude-sonnet-4",
        Some(200_000),
    );

    // When rendering.
    let terminal = render_element(&mut element, &state, 100, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 100);

    // Then the display is the last record only: 20_000/200_000 = 10.0% — not
    // 15% (half the 30k sum) and not 30k.
    assert!(
        row.contains("10.0%/200k"),
        "expected 10.0%/200k, got: {row}"
    );
    assert!(
        row.contains("\u{2191}30.0k"),
        "the ↑ token total stays the ledger sum (30k), got: {row}"
    );
}

#[rstest::rstest]
fn render_prefers_cached_context_size_over_the_ledger_fallback() {
    // Given a session with both a cached context size and a ledger whose last
    // estimate differs — the actor's value reflects pins, overrides, and
    // pruning, so it must win.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Single(
        "openrouter/anthropic/claude-sonnet-4".to_owned(),
    ));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 10_000,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });
    state.active_session_mut().set_context_sizes(6_000, 0);
    attach_model_cache(
        &mut state,
        "openrouter",
        "anthropic/claude-sonnet-4",
        Some(200_000),
    );

    // When rendering.
    let terminal = render_element(&mut element, &state, 100, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 100);

    // Then the cached size is shown: 6_000/200_000 = 3.0%, not the ledger's 5%.
    assert!(row.contains("3.0%/200k"), "expected 3.0%/200k, got: {row}");
}

#[rstest::rstest]
fn render_shows_zero_usage_when_ledger_empty_and_no_cached_size() {
    // Given a session that has never dispatched — no ledger records and no
    // cached context size.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Single(
        "openrouter/anthropic/claude-sonnet-4".to_owned(),
    ));
    attach_model_cache(&mut state, "openrouter", "anthropic/claude-sonnet-4", None);

    // When rendering.
    let terminal = render_element(&mut element, &state, 100, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 100);

    // Then the display is 0/??? : an unactivated session that has never
    // dispatched is genuinely zero, and with no cached size there is no max
    // to show against it.
    assert!(row.contains("0/???"), "expected 0/???, got: {row}");
}

#[rstest::rstest]
fn render_falls_back_when_no_context_limit_in_cache() {
    // Given a session with a cached context size but no context_length in the model cache.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 5000,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });
    state.active_session_mut().set_context_sizes(5000, 0);
    attach_model_cache(&mut state, "ollama", "llama3", None);

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the status bar shows usage with unknown limit (model found but no context_length).
    assert!(
        row.contains("5.0k/???"),
        "expected 5.0k/??? fallback, got: {row}"
    );
}

#[rstest::rstest]
fn render_falls_back_when_no_model_cache() {
    // Given a session with a cached context size but no model cache at all.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 5000,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });
    state.active_session_mut().set_context_sizes(5000, 0);
    // No model cache.
    assert!(
        state
            .provider_state()
            .is_none_or(|cell| cell.read().model_cache.is_none())
    );

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the status bar shows usage with unknown limit (no model cache at all).
    assert!(
        row.contains("5.0k/???"),
        "expected 5.0k/??? fallback, got: {row}"
    );
}

#[rstest::rstest]
fn render_shows_zero_percent_with_max_when_no_messages_sent() {
    // Given a model with a known context length but no messages sent (no context_size).
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Single(
        "openrouter/anthropic/claude-sonnet-4".to_owned(),
    ));

    attach_model_cache(
        &mut state,
        "openrouter",
        "anthropic/claude-sonnet-4",
        Some(200_000),
    );

    // When rendering.
    let terminal = render_element(&mut element, &state, 100, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 100);

    // Then the status bar shows 0.0% with the real max.
    assert!(row.contains("0.0%/200k"), "expected 0.0%/200k, got: {row}");
}

#[rstest::rstest]
fn render_shows_used_over_unknown_when_no_context_length() {
    // Given a session with context_size but no context_length in the model cache.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state.active_session_mut().set_context_sizes(15_000, 0);

    // Model cache exists but has no context_length.
    attach_model_cache(&mut state, "ollama", "llama3", None);

    // When rendering.
    let terminal = render_element(&mut element, &state, 100, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 100);

    // Then the status bar shows the formatted usage with unknown limit.
    assert!(row.contains("15.0k/???"), "expected 15.0k/???, got: {row}");
}

#[rstest::rstest]
fn render_always_shows_cost_even_when_zero() {
    // Given a state with no token records and no history.
    let mut element = StatusBarElement;
    let state = AppState::default();

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then cost is always shown as $0.00000.
    assert!(
        row.contains("$0.00000"),
        "expected $0.00000 in status bar, got: {row}"
    );
}

#[rstest::rstest]
fn render_shows_cost_with_non_zero_value() {
    // Given a session with a token record that has cost data.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1500,
        tokens_received: 750,
        cost: Some(0.00230),
        prompt_tokens: None,
        cached_tokens: None,
    });

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then the status bar shows the cost value.
    assert!(
        row.contains("$0.00230"),
        "expected $0.00230 in status bar, got: {row}"
    );
}

/// Build a state with an active (parent) session and one child session whose
/// ledgers report cache hits. The parent reports 1000 prompt / 960 cached
/// tokens; the child 200 prompt / 192 cached — 1152/1200 = 96% when aggregated.
fn state_with_parent_and_child_cache() -> AppState {
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1200,
        tokens_received: 600,
        cost: Some(0.01),
        prompt_tokens: Some(1000),
        cached_tokens: Some(960),
    });
    let child_id = jinn_core_types::SessionId::new();
    let active_id = state.session.active_session_id().clone();
    let child = state.session_mut_or_create(&child_id);
    child.push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 500,
        tokens_received: 250,
        cost: Some(0.005),
        prompt_tokens: Some(200),
        cached_tokens: Some(192),
    });
    child.set_parent_session(active_id);
    state
}

#[rstest::rstest]
fn render_tree_cache_segment_is_success_when_at_or_above_95_percent() {
    // Given a parent and child session whose aggregated ledgers report 96% cache hits.
    let mut element = StatusBarElement;
    let state = state_with_parent_and_child_cache();

    // When rendering.
    let terminal = render_element(&mut element, &state, 120, 2);
    let buffer = terminal.backend().buffer().clone();
    let row0 = buffer_row(&buffer, 0, 120);

    // Then the tree aggregate shows the cache segment with a 96% figure...
    assert!(
        row0.contains("\u{2B22} 96%"),
        "tree aggregate should show \u{2B22} 96%, got: {row0}"
    );
    // And the cell carrying the cache glyph is styled with the success color.
    let expected_fg = state.frontend.theme.success;
    let cache_cell = (0..120)
        .filter_map(|x| buffer.cell((x, 0)))
        .find(|cell| cell.symbol().starts_with('\u{2B22}'))
        .expect("cache glyph should be rendered on the tree line");
    assert_eq!(
        cache_cell.style().fg,
        Some(expected_fg),
        "cache segment should carry theme success color"
    );
}

#[rstest::rstest]
fn render_tree_cache_segment_keeps_muted_neighbors() {
    // Given a parent and child session showing a tree aggregate at 96% cache hits.
    let mut element = StatusBarElement;
    let state = state_with_parent_and_child_cache();

    // When rendering.
    let terminal = render_element(&mut element, &state, 120, 2);
    let buffer = terminal.backend().buffer().clone();

    // Then the token arrow right after the colored cache segment keeps the
    // muted color — only the cache segment itself is banded.
    let expected_muted = state.frontend.theme.muted_text;
    let after_cache = (0..120)
        .filter_map(|x| buffer.cell((x, 0)).map(|cell| (x, cell)))
        .skip_while(|(_, cell)| !cell.symbol().starts_with('\u{2B22}'))
        .find(|(_, cell)| cell.symbol().starts_with('\u{2191}'))
        .map(|(_, cell)| cell)
        .expect("up-arrow should follow the cache segment on the tree line");
    assert_eq!(
        after_cache.style().fg,
        Some(expected_muted),
        "non-cache segments should keep muted color"
    );
}

#[rstest::rstest]
fn render_shows_cost_before_turns_indicator() {
    // Given a state with history entries producing turns and a token record with cost.
    use jinn_token_count_msg::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state
        .active_session_mut()
        .push_entry(jinn_kernel::protocol::ChatEntry::user("hello"));
    state
        .active_session_mut()
        .push_entry(jinn_kernel::protocol::ChatEntry::assistant("hi there"));
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1000,
        tokens_received: 500,
        cost: Some(0.00150),
        prompt_tokens: None,
        cached_tokens: None,
    });

    // When rendering.
    let terminal = render_element(&mut element, &state, 80, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);

    // Then cost appears before Turns in the rendered row.
    let cost_pos = row.find("$0.00150").expect("cost should be present");
    let turns_pos = row
        .find("\u{21BB}")
        .expect("turns symbol should be present");
    assert!(
        cost_pos < turns_pos,
        "cost should appear before turns symbol, got: {row}"
    );
}

#[rstest::rstest]
fn render_hides_tree_aggregate_for_single_session() {
    // Given a single session (no tree).
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // When rendering.
    let terminal = render_element(&mut element, &state, 120, 2);
    let buffer = terminal.backend().buffer().clone();
    let row0 = buffer_row(&buffer, 0, 120);

    // Then line 1 should NOT contain the tree prefix.
    assert!(
        !row0.contains('\u{1F333}'),
        "single session should not show tree aggregate, got: {row0}"
    );
}

#[rstest::rstest]
fn render_shows_tree_aggregate_when_parent_has_child() {
    // Given a parent session with a child session, neither reporting cache hits.
    use jinn_token_count_msg::TokenRecord;

    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // The active (parent) session has a ledger entry...
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1000,
        tokens_received: 500,
        cost: Some(0.01),
        prompt_tokens: None,
        cached_tokens: None,
    });
    // ...and so does a child session.
    let child_id = jinn_core_types::SessionId::new();
    let active_id = state.session.active_session_id().clone();
    let child = state.session_mut_or_create(&child_id);
    child.push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 500,
        tokens_received: 250,
        cost: Some(0.005),
        prompt_tokens: None,
        cached_tokens: None,
    });
    child.set_parent_session(active_id);

    // When rendering.
    let terminal = render_element(&mut element, &state, 120, 2);
    let buffer = terminal.backend().buffer().clone();
    let row0 = buffer_row(&buffer, 0, 120);

    // Then line 1 right should show tree aggregate with \u{29C9}2.
    assert!(
        row0.contains("\u{29C9}2"),
        "tree aggregate should show session count \u{29C9}2, got: {row0}"
    );
    // And the tree prefix \u{1F333} should be present.
    assert!(
        row0.contains('\u{1F333}'),
        "tree prefix should be present, got: {row0}"
    );
}

#[rstest::rstest]
fn render_shows_tree_aggregate_from_child_viewpoint() {
    // Given a parent with a child, viewing from the child.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();

    // Create parent session first.
    let parent_id = jinn_core_types::SessionId::new();
    {
        let parent = state.session_mut_or_create(&parent_id);
        parent.push_entry(jinn_kernel::protocol::ChatEntry::user("parent msg"));
    }

    // Create child session.
    let child_id = jinn_core_types::SessionId::new();
    {
        let child = state.session_mut_or_create(&child_id);
        child.push_entry(jinn_kernel::protocol::ChatEntry::user("child msg"));
        child.set_parent_session(parent_id.clone());
    }

    // Switch to child as active.
    state.session.set_active(child_id);
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // When rendering.
    let terminal = render_element(&mut element, &state, 120, 2);
    let buffer = terminal.backend().buffer().clone();
    let row0 = buffer_row(&buffer, 0, 120);

    // Then tree aggregate still shows \u{29C9}2 (both sessions in tree).
    assert!(
        row0.contains("\u{29C9}2"),
        "tree aggregate from child should show \u{29C9}2, got: {row0}"
    );
}

#[rstest::rstest]
fn render_single_model_shows_provider_and_model_without_alloy_prefix() {
    // Given a single model selection.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // When rendering.
    let terminal = render_element(&mut element, &state, 50, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);

    // Then the model shows (provider)/model without alloy prefix.
    assert!(
        row.contains("(ollama)/llama3"),
        "should contain (ollama)/llama3, got: {row}"
    );
    assert!(
        !row.contains("[alloy"),
        "single model should not have alloy prefix, got: {row}"
    );
}

#[rstest::rstest]
fn render_alloy_with_token_records_shows_prefix_and_last_dispatched_model() {
    // Given an alloy with 3 models and a token record showing model-2 as last dispatched.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Alloy {
        models: vec![
            "provider-a/model-1".to_owned(),
            "provider-b/model-2".to_owned(),
            "provider-c/model-3".to_owned(),
        ],
        strategy: AlloyStrategy::RoundRobin { index: 0 },
    });
    state.active_session_mut().push_token_record(TokenRecord {
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 100,
        tokens_received: 50,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
        model_used: Some("provider-b/model-2".to_owned()),
    });

    // When rendering.
    let terminal = render_element(&mut element, &state, 60, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 60);

    // Then the model shows [alloy 3] prefix with the last-dispatched model.
    assert!(
        row.contains("[alloy 3] (provider-b)/model-2"),
        "should contain [alloy 3] (provider-b)/model-2, got: {row}"
    );
}

#[rstest::rstest]
fn render_alloy_with_no_token_records_falls_back_to_first_model() {
    // Given an alloy with no LLM calls yet.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Alloy {
        models: vec![
            "provider-a/model-1".to_owned(),
            "provider-b/model-2".to_owned(),
        ],
        strategy: AlloyStrategy::RoundRobin { index: 0 },
    });

    // When rendering.
    let terminal = render_element(&mut element, &state, 50, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);

    // Then the model shows [alloy 2] prefix with the first model as fallback.
    assert!(
        row.contains("[alloy 2] (provider-a)/model-1"),
        "should contain [alloy 2] (provider-a)/model-1, got: {row}"
    );
}

#[rstest::rstest]
fn render_alloy_with_one_model_shows_alloy_1() {
    // Given a 1-model alloy.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Alloy {
        models: vec!["ollama/llama3".to_owned()],
        strategy: AlloyStrategy::RoundRobin { index: 0 },
    });

    // When rendering.
    let terminal = render_element(&mut element, &state, 50, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);

    // Then the model shows [alloy 1] prefix.
    assert!(
        row.contains("[alloy 1] (ollama)/llama3"),
        "should contain [alloy 1] (ollama)/llama3, got: {row}"
    );
}

#[rstest::rstest]
fn render_appends_resolved_reasoning_effort_after_model() {
    // Given a session with a model and a global reasoning default of High.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state.active_session_mut().profile_mut().reasoning_effort =
        Some(jinn_kernel::ReasoningEffort::High);

    // When rendering.
    let terminal = render_element(&mut element, &state, 50, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);

    // Then the effort bracket precedes the modality indicator.
    assert!(
        row.contains("(ollama)/llama3 [high] <t>"),
        "should contain [high] then <t>, got: {row}"
    );
}

#[rstest::rstest]
fn render_session_override_beats_global_reasoning_effort() {
    // Given a global default of High but a session override of Low.
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    // A stale global value must not affect rendering — only the session's own effort matters.
    state.frontend.app_state.reasoning_effort = Some(jinn_kernel::ReasoningEffort::High);
    state.active_session_mut().profile_mut().reasoning_effort =
        Some(jinn_kernel::ReasoningEffort::Low);

    // When rendering.
    let terminal = render_element(&mut element, &state, 50, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);

    // Then the override wins: shows [low], not [high].
    assert!(
        row.contains("(ollama)/llama3 [low] <t>"),
        "should show session override [low], got: {row}"
    );
    assert!(
        !row.contains("[high]"),
        "should not show global [high] when override is set, got: {row}"
    );
}

#[rstest::rstest]
fn render_omits_reasoning_effort_bracket_when_unresolved() {
    // Given a model but no resolved reasoning effort (no override, no global).
    let mut element = StatusBarElement;
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // When rendering.
    let terminal = render_element(&mut element, &state, 50, 2);
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);

    // Then no reasoning bracket appears at all (no [none] noise).
    assert!(
        !row.contains('['),
        "should contain no bracket when effort unresolved, got: {row}"
    );
}

/// Build a model cache with one model under `ollama` carrying the given modalities.
fn cache_with_modalities(
    modalities: jinn_provider_config::InputModalities,
) -> jinn_provider_config::ModelCache {
    use std::collections::HashMap;
    let mut entries = HashMap::new();
    entries.insert(
        "ollama".to_owned(),
        vec![jinn_provider_config::ModelInfo {
            id: "llama3".to_owned(),
            context_length: None,
            input_modalities: modalities,
        }],
    );
    jinn_provider_config::ModelCache {
        entries,
        last_updated_at: None,
    }
}

fn render_model_row(state: &AppState) -> String {
    let mut element = StatusBarElement;
    let terminal = render_element(&mut element, state, 80, 2);
    buffer_row(terminal.backend().buffer(), 1, 80)
}

#[rstest::rstest]
fn status_bar_shows_modality_indicator_for_image_model() {
    // Given a selected image-capable model in the cache.
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    let mut m = jinn_provider_config::InputModalities::text();
    m.insert(jinn_provider_config::Modality::Image);
    state
        .provider_state()
        .expect("provider cell attached")
        .update(|cell| cell.model_cache = Some(cache_with_modalities(m)));

    // When rendering.
    let row = render_model_row(&state);

    // Then the indicator shows both text and image.
    assert!(
        row.contains("(ollama)/llama3 <ti>"),
        "should show <ti> for image model, got: {row}"
    );
}

#[rstest::rstest]
fn status_bar_shows_text_only_indicator_for_text_model() {
    // Given a selected text-only model in the cache.
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state
        .provider_state()
        .expect("provider cell attached")
        .update(|cell| {
            cell.model_cache = Some(cache_with_modalities(
                jinn_provider_config::InputModalities::text(),
            ));
        });

    // When rendering.
    let row = render_model_row(&state);

    // Then the indicator shows text only.
    assert!(
        row.contains("(ollama)/llama3 <t>"),
        "should show <t> for text-only model, got: {row}"
    );
}

#[rstest::rstest]
fn status_bar_shows_text_only_indicator_when_model_not_in_cache() {
    // Given a selected model that is NOT recorded in the cache.
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // When rendering.
    let row = render_model_row(&state);

    // Then the indicator coerces to text-only (conservative default).
    assert!(
        row.contains("(ollama)/llama3 <t>"),
        "should show <t> for not-in-cache model, got: {row}"
    );
    assert!(
        !row.contains("<ti>"),
        "should not advertise image for an unconfirmed model, got: {row}"
    );
}

#[rstest::rstest]
fn status_bar_shows_indicator_without_reasoning_effort_bracket() {
    // Given an image model with no resolved reasoning effort.
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    let mut m = jinn_provider_config::InputModalities::text();
    m.insert(jinn_provider_config::Modality::Image);
    state
        .provider_state()
        .expect("provider cell attached")
        .update(|cell| cell.model_cache = Some(cache_with_modalities(m)));

    // When rendering.
    let row = render_model_row(&state);

    // Then the <ti> indicator appears with no effort bracket after it.
    assert!(
        row.contains("(ollama)/llama3 <ti>"),
        "should show <ti> indicator, got: {row}"
    );
    assert!(
        !row.contains('['),
        "should show no effort bracket, got: {row}"
    );
}

#[rstest::rstest]
fn status_bar_shows_effort_bracket_then_modality_indicator() {
    // Given an image-capable model with a resolved reasoning effort.
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    state.active_session_mut().profile_mut().reasoning_effort =
        Some(jinn_kernel::ReasoningEffort::High);
    let mut m = jinn_provider_config::InputModalities::text();
    m.insert(jinn_provider_config::Modality::Image);
    state
        .provider_state()
        .expect("provider cell attached")
        .update(|cell| cell.model_cache = Some(cache_with_modalities(m)));

    // When rendering.
    let row = render_model_row(&state);

    // Then the effort bracket precedes the modality indicator.
    assert!(
        row.contains("(ollama)/llama3 [high] <ti>"),
        "should show [high] then <ti>, got: {row}"
    );
}

#[rstest::rstest]
fn status_bar_omits_indicator_when_no_model_selected() {
    // Given no model selected (default state).
    let state = AppState::default();

    // When rendering.
    let row = render_model_row(&state);

    // Then no modality indicator appears.
    assert!(
        row.contains("no model selected"),
        "should show no model selected, got: {row}"
    );
    assert!(
        !row.contains("<t>") && !row.contains("<ti>"),
        "should show no indicator when no model selected, got: {row}"
    );
}

#[rstest::rstest]
fn status_bar_alloy_indicator_reflects_last_dispatched_member() {
    use jinn_token_count_msg::TokenRecord;
    // Given an alloy where the last-dispatched member is image-capable.
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().set_model(ModelSelection::Alloy {
        models: vec!["ollama/llama3".to_owned(), "ollama/gpt-4o".to_owned()],
        strategy: AlloyStrategy::RoundRobin { index: 0 },
    });
    // Last ledger record points at the image-capable member.
    state.active_session_mut().push_token_record(TokenRecord {
        model_used: Some("ollama/gpt-4o".to_owned()),
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 0,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });
    // Cache records gpt-4o as image-capable.
    {
        use std::collections::HashMap;
        let mut m = jinn_provider_config::InputModalities::text();
        m.insert(jinn_provider_config::Modality::Image);
        let mut entries = HashMap::new();
        entries.insert(
            "ollama".to_owned(),
            vec![
                jinn_provider_config::ModelInfo {
                    id: "llama3".to_owned(),
                    context_length: None,
                    input_modalities: jinn_provider_config::InputModalities::text(),
                },
                jinn_provider_config::ModelInfo {
                    id: "gpt-4o".to_owned(),
                    context_length: None,
                    input_modalities: m,
                },
            ],
        );
        state
            .provider_state()
            .expect("provider cell attached")
            .update(|cell| {
                cell.model_cache = Some(jinn_provider_config::ModelCache {
                    entries,
                    last_updated_at: None,
                });
            });
    }

    // When rendering.
    let row = render_model_row(&state);

    // Then the indicator reflects the last-dispatched member (gpt-4o, image).
    assert!(
        row.contains("(ollama)/gpt-4o <ti>"),
        "should show last-dispatched member modalities, got: {row}"
    );
}

// ── Working-time clocks ──────────────────────────────────────────────
//
// Both clocks are read from the work-time cell at paint time, so a running
// turn ticks with no extra plumbing. A test that renders with a bare
// `Slices` sees no clock at all; registering the cell is what makes one.

/// A `Slices` with the work-time cell registered, seeded for `session_id`.
///
/// Registers rather than constructing the actor: the bar only reads the cell,
/// and the monitor's own write path is tested where the monitor lives.
fn slices_with_working_time(
    session_id: &jinn_core_types::SessionId,
    intervals: Vec<WorkingInterval>,
) -> jinn_slices::Slices {
    let slices = jinn_slices::Slices::new();
    let cell = seed_cell(&slices, &work_time_slot(), WorkingTimeState::default());
    cell.update(|s| s.restore(session_id.clone(), intervals));
    slices
}

fn at(offset_secs: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_second(1_000_000_000 + offset_secs).expect("valid offset")
}

/// Renders the bar and returns row `row` as a string.
fn render_rows(
    state: &AppState,
    slices: &jinn_slices::Slices,
    width: u16,
    height: u16,
) -> Vec<String> {
    let mut element = StatusBarElement;
    let terminal = render_element_with_slices(&mut element, state, slices, width, height);
    let buffer = terminal.backend().buffer().clone();
    (0..height).map(|r| buffer_row(&buffer, r, width)).collect()
}

#[rstest::rstest]
fn info_line_shows_working_time_beside_the_turn_counter() {
    // Given a session that has worked for nine seconds.
    let state = AppState::default_with_scope_focus();
    let id = state.session.active_session_id().clone();
    let slices = slices_with_working_time(&id, vec![WorkingInterval::closed(at(0), at(9))]);

    // When rendering.
    let rows = render_rows(&state, &slices, 90, 2);

    // Then the info line shows the working time in HH:MM:SS form.
    assert!(rows[1].contains("00:00:09"), "row was: {}", rows[1]);
}

#[rstest::rstest]
fn info_line_pads_working_time_to_two_digits() {
    // Given a session that has worked for one hour, two minutes, three seconds.
    let state = AppState::default_with_scope_focus();
    let id = state.session.active_session_id().clone();
    let slices = slices_with_working_time(&id, vec![WorkingInterval::closed(at(0), at(3723))]);

    // When rendering.
    let rows = render_rows(&state, &slices, 90, 2);

    // Then the clock is zero-padded and never abbreviated.
    assert!(rows[1].contains("01:02:03"), "row was: {}", rows[1]);
}

#[rstest::rstest]
fn info_line_working_time_sits_after_the_turn_counter() {
    // Given a session that has worked for five seconds.
    let state = AppState::default_with_scope_focus();
    let id = state.session.active_session_id().clone();
    let slices = slices_with_working_time(&id, vec![WorkingInterval::closed(at(0), at(5))]);

    // When rendering.
    let rows = render_rows(&state, &slices, 90, 2);

    // Then the clock follows the turn counter rather than preceding it.
    let info = &rows[1];
    let turn = info.find('\u{21BB}').expect("turn counter rendered");
    let clock = info.find("00:00:05").expect("working time rendered");
    assert!(turn < clock, "row was: {info}");
}

#[rstest::rstest]
fn info_line_working_time_is_zero_before_any_work() {
    // Given a session that has never worked.
    let state = AppState::default_with_scope_focus();
    let id = state.session.active_session_id().clone();
    let slices = slices_with_working_time(&id, Vec::new());

    // When rendering.
    let rows = render_rows(&state, &slices, 90, 2);

    // Then the clock reads zero, holding the column's width so the bar does
    // not reflow the moment a turn starts.
    assert!(rows[1].contains("00:00:00"), "row was: {}", rows[1]);
}

#[rstest::rstest]
fn tree_block_shows_working_time_beside_the_turn_counter() {
    // Given a tree of a parent and a child that worked the same ten seconds.
    let mut state = AppState::default_with_scope_focus();
    let parent_id = state.session.active_session_id().clone();
    let child = jinn_session_state::ChatSessionState::new_child(&parent_id, true);
    let child_id = child.session_id().clone();
    state.session.insert(child);

    let shared = WorkingInterval::closed(at(0), at(10));
    let slices = jinn_slices::Slices::new();
    let cell = seed_cell(&slices, &work_time_slot(), WorkingTimeState::default());
    cell.update(|s| {
        s.restore(parent_id.clone(), vec![shared.clone()]);
        s.restore(child_id, vec![shared]);
    });

    // When rendering.
    let rows = render_rows(&state, &slices, 120, 2);

    // Then the tree block shows the union — ten seconds, not twenty.
    assert!(rows[0].contains("00:00:10"), "row was: {}", rows[0]);
}

#[rstest::rstest]
fn tree_block_shows_the_union_when_a_member_is_frozen() {
    // Given a live parent that worked 0..20 (intervals in the work-time cell)
    // and an archived child that worked 10..30 (intervals on its frozen node,
    // stamped through the same freeze path production uses).
    let mut state = AppState::default_with_scope_focus();
    let parent_id = state.session.active_session_id().clone();
    let child = jinn_session_state::ChatSessionState::new_child(&parent_id, true);
    let frozen_child = jinn_session_state::snapshot_frozen_node(
        &child,
        vec![WorkingInterval::closed(at(10), at(30))],
    );
    state.session.insert_frozen_node(frozen_child);

    let slices = jinn_slices::Slices::new();
    let cell = seed_cell(&slices, &work_time_slot(), WorkingTimeState::default());
    cell.update(|s| s.restore(parent_id, vec![WorkingInterval::closed(at(0), at(20))]));

    // When rendering.
    let rows = render_rows(&state, &slices, 120, 2);

    // Then the tree block shows the union of the cell's live intervals and
    // the node's frozen ones — 0..30, thirty seconds, not the 40-second sum.
    assert!(rows[0].contains("00:00:30"), "row was: {}", rows[0]);
}

#[rstest::rstest]
fn a_single_session_tree_renders_no_tree_working_time() {
    // Given a lone session that has worked, so the tree block is suppressed.
    let state = AppState::default_with_scope_focus();
    let id = state.session.active_session_id().clone();
    let slices = slices_with_working_time(&id, vec![WorkingInterval::closed(at(0), at(30))]);

    // When rendering.
    let rows = render_rows(&state, &slices, 120, 2);

    // Then no tree glyph block carries a second, unprefixed clock.
    assert!(!rows[0].contains('\u{1F333}'), "row was: {}", rows[0]);
    assert_eq!(
        rows[1].matches("00:00:30").count(),
        1,
        "exactly one clock, on the info line: {}",
        rows[1]
    );
}
