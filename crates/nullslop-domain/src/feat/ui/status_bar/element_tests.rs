#![allow(clippy::expect_used, clippy::indexing_slicing)]

use nullslop_testutil::{buffer_row, setup_term};
use ratatui::style::Color;

use crate::common::app_state::{AppState, StatusNotification};
use crate::common::ui_element::UiElement;
use crate::feat::ui::status_bar::element::StatusBarElement;

#[rstest::rstest]
fn name_returns_status_bar() {
    let element = StatusBarElement;
    assert_eq!(element.name(), "status-bar");
}

#[rstest::rstest]
fn render_shows_no_model_selected_when_unset() {
    let mut element = StatusBarElement;
    let state = AppState::default();
    let (mut terminal, area) = setup_term(50, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);
    assert!(row.starts_with("(Passthrough)"));
    assert!(row.contains("no model selected"));
}

#[rstest::rstest]
fn render_shows_provider_and_model() {
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    let (mut terminal, area) = setup_term(50, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);
    assert!(row.starts_with("(Passthrough)"));
    assert!(row.contains("(ollama)/llama3"));
}

#[rstest::rstest]
fn render_right_aligns_text() {
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    let (mut terminal, area) = setup_term(50, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    // Row 1 is the info line.
    let first = buffer.cell((0, 1)).expect("first cell");
    assert_eq!(first.symbol(), "(");
    let last = buffer.cell((49, 1)).expect("last cell");
    assert_eq!(last.symbol(), "3");
}

#[rstest::rstest]
fn render_uses_gray_color() {
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    let (mut terminal, area) = setup_term(40, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    // Find a non-space cell on the info line (row 1).
    let text_cell = (0..40)
        .filter_map(|x| buffer.cell((x, 1)))
        .find(|c| c.symbol() != " ")
        .expect("should have text cell on info line");
    assert_eq!(text_cell.fg, Color::DarkGray);
}

#[rstest::rstest]
fn render_shows_provider_with_slash_in_model() {
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("openrouter/anthropic/claude-sonnet-4".to_owned());
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    assert!(row.starts_with("(Passthrough)"));
    assert!(row.contains("(openrouter)/anthropic/claude-sonnet-4"));
}

#[rstest::rstest]
fn render_shows_non_default_strategy() {
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state
        .active_session_mut()
        .switch_strategy(crate::protocol::PromptStrategyId::sliding_window());
    let (mut terminal, area) = setup_term(50, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 50);
    assert!(row.starts_with("(Sliding Window)"));
    assert!(row.contains("(ollama)/llama3"));
}

#[rstest::rstest]
fn render_shows_pinned_count_when_entries_pinned() {
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    let idx = state
        .active_session_mut()
        .push_entry(crate::protocol::ChatEntry::user("hello"));
    let entry_id = state.active_session().history()[idx].id.clone();
    state
        .active_session_mut()
        .pin_entry(&entry_id, crate::protocol::PinPosition::Relative);
    let (mut terminal, area) = setup_term(60, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 60);
    assert!(row.contains("\u{1f4cc}"));
    assert!(row.contains('1'));
}

#[rstest::rstest]
fn render_hides_pinned_count_when_no_entries_pinned() {
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    let (mut terminal, area) = setup_term(60, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 60);
    assert!(row.starts_with("(Passthrough) "));
    assert!(!row.contains("\u{1f4cc}"));
}

#[rstest::rstest]
fn render_shows_notification_when_active() {
    // Given a state with a notification and a model.
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state
        .frontend
        .set_status_notification("Copied to clipboard");
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then the notification text appears in the right portion.
    assert!(row.contains("Copied to clipboard"));
    // And the model is still shown.
    assert!(row.contains("(ollama)/llama3"));
}

#[rstest::rstest]
fn render_notification_uses_green_color() {
    // Given a state with an active notification.
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state.frontend.set_status_notification("Copied!");
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();

    // Find a cell in the notification text ("Copied!") that has green fg.
    // The notification is on row 1, right-aligned.
    let green_cell = (0..80)
        .filter_map(|x| buffer.cell((x, 1)))
        .find(|c| c.symbol() == "C" && c.fg == Color::Green);
    assert!(green_cell.is_some(), "notification text should be green");
}

#[rstest::rstest]
fn render_no_notification_shows_model_only() {
    // Given a state with no notification.
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then only the model is shown on the right.
    assert!(row.contains("(ollama)/llama3"));
    assert!(!row.contains("Copied"));
}

#[rstest::rstest]
fn render_expired_notification_not_shown() {
    // Given a state with an expired notification.
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state.frontend.status_notification = Some(StatusNotification {
        message: "old msg".to_owned(),
        created_at: std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(5))
            .unwrap(),
    });
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then the notification is not shown.
    assert!(!row.contains("old msg"));
    // And the model is still shown normally.
    assert!(row.contains("(ollama)/llama3"));
}

// --- Token display tests ---

#[rstest::rstest]
fn render_shows_token_counts_with_zero_values() {
    // Given a state with no token records.
    let mut element = StatusBarElement;
    let state = AppState::default();
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then the status bar shows zero token counts.
    assert!(row.contains("\u{2191}0 \u{2193}0"));
    // And cost is always shown as $0.0000.
    assert!(row.contains("$0.0000"));
}

#[rstest::rstest]
fn render_shows_token_counts_with_values() {
    // Given a session with token records.
    use crate::feat::session::token_stats::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state.active_session_mut().push_token_record(TokenRecord {
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1500,
        tokens_received: 750,
        cost: None,
    });
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then the status bar shows token counts.
    assert!(row.contains("1.5k"));
    assert!(row.contains("750"));
    // And cost is shown as $0.0000 when no cost data.
    assert!(row.contains("$0.0000"));
}

#[rstest::rstest]
fn render_shows_zero_percent_max_when_context_size_but_no_limit() {
    // Given a session with a cached context size but no model cache.
    use crate::feat::session::token_stats::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state.active_session_mut().push_token_record(TokenRecord {
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 5000,
        tokens_received: 0,
        cost: None,
    });
    state.active_session_mut().set_context_size(5000);
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then the status bar shows the context size with ??? (no context_length available).
    assert!(row.contains("5.0k/???"), "expected 5.0k/???, got: {row}");
}

#[rstest::rstest]
fn render_shows_zero_percent_max_when_no_context_size() {
    // Given a session with no cached context size.
    let mut element = StatusBarElement;
    let state = AppState::default();
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then the context display shows 0/??? as fallback.
    assert!(
        row.contains("0/???"),
        "expected 0/??? fallback, got: {row}"
    );
    // And token counts are still shown.
    assert!(row.contains("0 0") || row.contains("\u{2191}0 \u{2193}0"));
}

// --- Turn counter tests ---

#[rstest::rstest]
fn render_shows_zero_turns_when_no_history() {
    // Given a state with no chat entries.
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then the status bar shows "Turns: 0".
    assert!(row.contains("Turns: 0"));
}

#[rstest::rstest]
fn render_shows_turn_count_with_history() {
    // Given a state with user and assistant entries.
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state
        .active_session_mut()
        .push_entry(crate::protocol::ChatEntry::user("hello"));
    state
        .active_session_mut()
        .push_entry(crate::protocol::ChatEntry::assistant("hi there"));
    state
        .active_session_mut()
        .push_entry(crate::protocol::ChatEntry::user("how are you?"));
    state
        .active_session_mut()
        .push_entry(crate::protocol::ChatEntry::assistant("doing well"));
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then the status bar shows "Turns: 4".
    assert!(row.contains("Turns: 4"));
}

#[rstest::rstest]
fn render_turn_count_skips_tool_loop_intermediates() {
    // Given a state with a tool-loop conversation.
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state
        .active_session_mut()
        .push_entry(crate::protocol::ChatEntry::user("fix the bug"));
    state
        .active_session_mut()
        .push_entry(crate::protocol::ChatEntry::assistant("let me check"));
    state
        .active_session_mut()
        .push_entry(crate::protocol::ChatEntry::tool_call(
            "id-1",
            "bash",
            r#"{"command":"ls"}"#,
        ));
    state
        .active_session_mut()
        .push_entry(crate::protocol::ChatEntry::assistant("fixed it"));
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then only the user and final assistant count as turns.
    assert!(row.contains("Turns: 2"));
}

// --- CWD display tests ---

#[rstest::rstest]
fn render_shows_cwd_on_first_line() {
    // Given default state (cwd is "." which resolves to the current dir).
    let mut element = StatusBarElement;
    let state = AppState::default();
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    // The first row should have content (cwd).
    let row0 = buffer_row(&buffer, 0, 80);
    assert!(!row0.trim().is_empty(), "cwd line should not be empty");
}

#[rstest::rstest]
fn render_shows_absolute_path_for_non_home_cwd() {
    // Given a session with a non-home CWD.
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state
        .active_session_mut()
        .set_cwd(std::path::PathBuf::from("/tmp/test-project"));
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    // The cwd line (row 0) should show the full absolute path.
    let row0 = buffer_row(&buffer, 0, 80);
    assert!(
        row0.contains("/tmp/test-project"),
        "expected /tmp/test-project in cwd line, got: {row0}"
    );
}

#[rstest::rstest]
fn render_shows_tilde_for_home_cwd() {
    // Given a session whose CWD is the user's home directory.
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    let home = dirs::home_dir().expect("home dir exists");
    state.active_session_mut().set_cwd(home);
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    // The cwd line should show just "~" (home dir).
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
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    let home = dirs::home_dir().expect("home dir exists");
    state
        .active_session_mut()
        .set_cwd(home.join("projects/my-app"));
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    // The cwd line should show "~/projects/my-app".
    let row0 = buffer_row(&buffer, 0, 80);
    assert!(
        row0.trim_start().starts_with("~/projects/my-app"),
        "expected ~/projects/my-app in cwd line, got: {row0}"
    );
}

// --- Context limit display tests ---

#[rstest::rstest]
fn render_shows_context_limit_with_usage_and_percentage() {
    // Given a session with a cached context size and a model cache with context_length.
    use crate::feat::session::token_stats::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("openrouter/anthropic/claude-sonnet-4".to_owned());
    state.active_session_mut().push_token_record(TokenRecord {
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 5000,
        tokens_received: 0,
        cost: None,
    });
    state.active_session_mut().set_context_size(5000);

    // And a model cache with context_length for the active model.
    let mut cache_entries = std::collections::HashMap::new();
    cache_entries.insert(
        "openrouter".to_owned(),
        vec![crate::feat::provider_infra::ModelInfo {
            id: "anthropic/claude-sonnet-4".to_owned(),
            context_length: Some(200_000),
        }],
    );
    state.provider.model_cache = Some(crate::feat::provider_infra::ModelCache {
        entries: cache_entries,
        last_updated_at: None,
    });

    let (mut terminal, area) = setup_term(100, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 100);
    // Then the status bar shows the percentage and formatted max.
    assert!(row.contains("2.5%/200k"), "expected 2.5%/200k, got: {row}");
}

#[rstest::rstest]
fn render_falls_back_when_no_context_limit_in_cache() {
    // Given a session with a cached context size but no context_length in the model cache.
    use crate::feat::session::token_stats::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state.active_session_mut().push_token_record(TokenRecord {
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 5000,
        tokens_received: 0,
        cost: None,
    });
    state.active_session_mut().set_context_size(5000);

    // Model cache exists but has no context_length.
    let mut cache_entries = std::collections::HashMap::new();
    cache_entries.insert(
        "ollama".to_owned(),
        vec![crate::feat::provider_infra::ModelInfo {
            id: "llama3".to_owned(),
            context_length: None,
        }],
    );
    state.provider.model_cache = Some(crate::feat::provider_infra::ModelCache {
        entries: cache_entries,
        last_updated_at: None,
    });

    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then the status bar falls back to the context size with ??? (no context_length).
    assert!(
        row.contains("5.0k/???"),
        "expected 5.0k/??? fallback, got: {row}"
    );
}

#[rstest::rstest]
fn render_falls_back_when_no_model_cache() {
    // Given a session with a cached context size but no model cache at all.
    use crate::feat::session::token_stats::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state.active_session_mut().push_token_record(TokenRecord {
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 5000,
        tokens_received: 0,
        cost: None,
    });
    state.active_session_mut().set_context_size(5000);
    // No model cache.
    assert!(state.provider.model_cache.is_none());

    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then the status bar falls back to the context size with ??? (no model cache).
    assert!(
        row.contains("5.0k/???"),
        "expected 5.0k/??? fallback, got: {row}"
    );
}

// --- Token budget display tests ---

#[rstest::rstest]
fn render_shows_token_budget_when_token_budget_strategy_active() {
    // Given a session with the token_budget strategy.
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state
        .active_session_mut()
        .switch_strategy(crate::protocol::PromptStrategyId::token_budget());
    state.active_session_mut().profile_mut().token_budget = 200_000;
    let (mut terminal, area) = setup_term(100, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 100);
    // Then the status bar shows "(Token Budget: 200k)".
    assert!(
        row.contains("(Token Budget: 200k)"),
        "expected budget display, got: {row}"
    );
}

#[rstest::rstest]
fn render_hides_token_budget_for_passthrough_strategy() {
    // Given a session with the passthrough strategy (default).
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    let (mut terminal, area) = setup_term(100, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 100);
    // Then the status bar does NOT show "Token Budget:".
    assert!(
        !row.contains("Token Budget:"),
        "expected no budget display for passthrough strategy, got: {row}"
    );
}

// --- Cost display tests ---

#[rstest::rstest]
fn render_always_shows_cost_even_when_zero() {
    // Given a state with no token records and no history.
    let mut element = StatusBarElement;
    let state = AppState::default();
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then cost is always shown as $0.0000.
    assert!(
        row.contains("$0.0000"),
        "expected $0.0000 in status bar, got: {row}"
    );
}

#[rstest::rstest]
fn render_shows_cost_with_non_zero_value() {
    // Given a session with a token record that has cost data.
    use crate::feat::session::token_stats::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state.active_session_mut().push_token_record(TokenRecord {
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1500,
        tokens_received: 750,
        cost: Some(0.0023),
    });
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then the status bar shows the cost value.
    assert!(
        row.contains("$0.0023"),
        "expected $0.0023 in status bar, got: {row}"
    );
}

#[rstest::rstest]
fn render_shows_cost_before_turns_indicator() {
    // Given a state with history entries producing turns and a token record with cost.
    use crate::feat::session::token_stats::TokenRecord;
    let mut element = StatusBarElement;
    let mut state = AppState::default();
    state
        .active_session_mut()
        .set_model("ollama/llama3".to_owned());
    state
        .active_session_mut()
        .push_entry(crate::protocol::ChatEntry::user("hello"));
    state
        .active_session_mut()
        .push_entry(crate::protocol::ChatEntry::assistant("hi there"));
    state.active_session_mut().push_token_record(TokenRecord {
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1000,
        tokens_received: 500,
        cost: Some(0.0015),
    });
    let (mut terminal, area) = setup_term(80, 2);
    terminal
        .draw(|frame| {
            element.render(frame, area, &state);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = buffer_row(&buffer, 1, 80);
    // Then cost appears before Turns in the rendered row.
    let cost_pos = row.find("$0.0015").expect("cost should be present");
    let turns_pos = row.find("Turns:").expect("Turns should be present");
    assert!(
        cost_pos < turns_pos,
        "cost should appear before Turns, got: {row}"
    );
}
