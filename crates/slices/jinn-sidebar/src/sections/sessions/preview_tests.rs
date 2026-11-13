//! Tests for the session preview popup renderer.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use crate::sections::sessions::preview::{
    DEFAULT_TOOL_ENTRY_MAX_LINES, render_session_preview, render_session_preview_loading,
    session_preview_popup_rect,
};
use jinn_chat_log_view::chat_log::RenderContext;
use jinn_chat_log_view::kernel_element::render_preview as render_preview_lines;
use jinn_chat_log_view_msg::{PREVIEW_ENTRY_COUNT, PREVIEW_MAX_LINES};
use jinn_core_types::model_selection::ModelSelection;
use jinn_kernel::protocol::ChatEntry;
use jinn_session_state::ChatSessionState;
use jinn_testutil::{buffer_row, setup_term, volume_root};
use jinn_theme::default_theme;
use jinn_tools_msg::{PhaseInput, TaskStatus};
use marked_path::{Absolute, MarkedPathBuf};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

fn make_session_with_entries(n: usize) -> ChatSessionState {
    let mut session = ChatSessionState::new();
    for i in 0..n {
        session.push_entry(ChatEntry::user(format!("message {i}")));
    }
    session
}

fn make_session_with_title(title: &str) -> ChatSessionState {
    let mut session = ChatSessionState::new();
    session.set_title(title.to_owned());
    session
}

/// A path that lies outside the user's home directory on every platform.
///
/// The platform temp directory is *under* the user profile on Windows
/// (`%LOCALAPPDATA%\Temp` hangs off `%USERPROFILE%`), so a temp root is not
/// outside home there — it is on Unix. A path rooted at the temp volume's
/// root is outside home everywhere. The directory need not exist: these tests
/// render a path string, they do not touch the filesystem.
fn path_outside_home(name: &str) -> MarkedPathBuf<Absolute> {
    volume_root()
        .join_all([name])
        .expect("root joins a bare name")
}

/// A session whose trailing entries render to more lines than the budget allows.
fn session_with_many_lines() -> ChatSessionState {
    let mut session = ChatSessionState::new();
    for i in 0..5 {
        let text = (0..10)
            .map(|j| format!("line {i}-{j}"))
            .collect::<Vec<_>>()
            .join("\n");
        session.push_entry(ChatEntry::assistant(text));
    }
    session
}

/// A session with exactly one short entry — a preview much shorter than the
/// content area, so there is room above it.
fn session_with_one_short_entry() -> ChatSessionState {
    make_session_with_entries(1)
}

/// The lines the worker would publish for `session`, at the popup's width.
fn worker_lines_for(session: &ChatSessionState, popup_area: Rect) -> Vec<Line<'static>> {
    let ctx = RenderContext {
        content_width: popup_area.width.saturating_sub(2),
        is_selected: false,
        is_expanded: false,
        tool_entry_max_lines: DEFAULT_TOOL_ENTRY_MAX_LINES,
        theme: default_theme(),
        paired_status: None,
        is_streaming: false,
        is_waiting_on_subagent: false,
    };
    render_preview_lines(
        session.history(),
        &ctx,
        PREVIEW_ENTRY_COUNT,
        PREVIEW_MAX_LINES,
    )
}

/// Renders `session`'s preview into a frame whose cursor sits at `cursor_y`.
///
/// The lines come from the worker's own pipeline at the width the popup
/// derives, so what is asserted is what production would draw.
fn render_preview_at_cursor(
    session: &ChatSessionState,
    term_width: u16,
    term_height: u16,
    cursor_y: u16,
) -> (Buffer, Rect) {
    let theme = default_theme();
    let frame_area = Rect::new(0, 0, term_width, term_height);
    let popup_area = popup_rect_or_panic(frame_area, cursor_y);
    let lines = worker_lines_for(session, popup_area);

    let (mut terminal, _) = setup_term(term_width, term_height);
    terminal
        .draw(|frame| {
            render_session_preview(
                frame,
                popup_area,
                session,
                &theme,
                &lines,
                None,
                no_working_time(),
            );
        })
        .unwrap();

    let buffer = terminal.backend().buffer().clone();
    (buffer, popup_area)
}

/// The row directly above the footer — the content area's last row.
fn last_content_row(popup_area: Rect) -> u16 {
    popup_area.y + popup_area.height - 2 - 4
}

/// The working time a render test passes: zero, so the info line shows the
/// never-worked clock and assertions are not chasing a live measurement.
fn no_working_time() -> jiff::SignedDuration {
    jiff::SignedDuration::ZERO
}

fn render_preview(
    session: &ChatSessionState,
    term_width: u16,
    term_height: u16,
) -> (ratatui::buffer::Buffer, Rect) {
    render_preview_with_cache(session, term_width, term_height, None)
}

/// A model cache naming one model under `provider` with a context length, so
/// an info-row test can produce the `NN.N%/<max>` shape rather than degrading
/// to `???`.
fn model_cache_with_context_length(
    provider: &str,
    model_id: &str,
    context_length: u32,
) -> jinn_provider_config::ModelCache {
    let mut entries = std::collections::HashMap::new();
    entries.insert(
        provider.to_owned(),
        vec![jinn_provider_config::ModelInfo {
            id: model_id.to_owned(),
            context_length: Some(context_length),
            input_modalities: jinn_provider_config::InputModalities::text(),
        }],
    );
    jinn_provider_config::ModelCache {
        entries,
        last_updated_at: None,
    }
}

fn render_preview_with_cache(
    session: &ChatSessionState,
    term_width: u16,
    term_height: u16,
    model_cache: Option<&jinn_provider_config::ModelCache>,
) -> (ratatui::buffer::Buffer, Rect) {
    let theme = default_theme();
    let frame_area = Rect::new(0, 0, term_width, term_height);
    // Simulate cursor at row 30 (sessions section starts at row 30, cursor on first item).
    let cursor_y = 30u16;
    let popup_area = popup_rect_or_panic(frame_area, cursor_y);

    // The lines a worker would have published for this session, rendered the
    // same way the chat log renders them.
    let lines = {
        let ctx = RenderContext {
            content_width: popup_area.width.saturating_sub(2),
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: DEFAULT_TOOL_ENTRY_MAX_LINES,
            theme: theme.clone(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
        };
        render_preview_lines(
            session.history(),
            &ctx,
            PREVIEW_ENTRY_COUNT,
            PREVIEW_MAX_LINES,
        )
    };

    let (mut terminal, _) = setup_term(term_width, term_height);
    terminal
        .draw(|frame| {
            render_session_preview(
                frame,
                popup_area,
                session,
                &theme,
                &lines,
                model_cache,
                no_working_time(),
            );
        })
        .unwrap();

    let buffer = terminal.backend().buffer().clone();
    (buffer, popup_area)
}

#[rstest::rstest]
fn empty_history_shows_title_and_keybinds_only() {
    // Given a session with no entries and a title.
    let session = make_session_with_title("Empty Session");

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the popup title appears in the top border.
    let top_row = buffer_row(&buffer, popup_area.y, popup_area.x + popup_area.width);
    assert!(
        top_row.contains("Empty Session"),
        "popup title should contain 'Empty Session', got: {top_row}"
    );

    // And the keybinds lines appear in the footer area.
    // Keybinds line 2 (c continue · r rename) is the footer's second row: four
    // rows above the popup's bottom border.
    let keybinds_y = popup_area.y + popup_area.height - 4;
    let keybinds_row = buffer_row(&buffer, keybinds_y, popup_area.x + popup_area.width);
    assert!(
        keybinds_row.contains('c') || keybinds_row.contains('r'),
        "keybinds line 2 should contain c or r, got: {keybinds_row}"
    );
}

#[rstest::rstest]
fn a_short_history_fills_the_content_area() {
    // Given a session with 8 one-line entries — fewer messages than the content
    // area has rows, which is the case that used to leave a gap at the top.
    let session = make_session_with_entries(8);

    // When rendering the preview with enough vertical space.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the content area is full: the newest entry is on the last row and
    // there is no blank band above it. Bounding the window by message count
    // showed only what fit in five messages and left the rest of the box empty.
    let content_start_y = popup_area.y + 1;
    let content_end_y = popup_area.y + popup_area.height - 2;
    let mut all_text = String::new();
    for y in content_start_y..=content_end_y {
        all_text.push_str(&buffer_row(&buffer, y, popup_area.x + popup_area.width));
    }
    assert!(
        all_text.contains("message 7"),
        "should contain the last entry 'message 7', got text: {all_text}"
    );
    assert!(
        !all_text.contains("message 0"),
        "should NOT reach back past the preview's budget to 'message 0', got text: {all_text}"
    );

    // And no row of the content area is an empty band. The popup is drawn
    // bottom-anchored over a fixed height, so a preview that renders fewer lines
    // than the area has rows leaves the surplus as blank rows above the text —
    // the gap this is about. Every row here is a message or its padding, so
    // there is nothing left over.
    let blank_rows = (content_start_y..=content_end_y)
        .filter(|y| {
            buffer_row(&buffer, *y, popup_area.x + popup_area.width)
                .trim()
                .is_empty()
        })
        .count();
    assert_eq!(
        blank_rows, 0,
        "the content area should be filled, but {blank_rows} of its rows are blank"
    );
}

#[rstest::rstest]
fn the_workers_lines_fit_the_popups_content_area() {
    // Given a session with entries that produce many more lines than the popup
    // has rows for.
    let session = session_with_many_lines();

    // When rendering the preview.
    let (_buffer, popup_area) = render_preview(&session, 80, 40);
    let content_rows = content_rows_of(popup_area);

    // Then the lines the worker produced fit the rows the popup offers. The
    // box's own height is asserted separately — it is fixed, so it proves
    // nothing about how much text the worker returned.
    let lines = {
        let ctx = RenderContext {
            content_width: popup_area.width.saturating_sub(2),
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: DEFAULT_TOOL_ENTRY_MAX_LINES,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
        };
        render_preview_lines(
            session.history(),
            &ctx,
            PREVIEW_ENTRY_COUNT,
            PREVIEW_MAX_LINES,
        )
    };
    assert_eq!(
        lines.len() as u16,
        content_rows,
        "the worker's line budget should match the popup's content rows"
    );
}

#[rstest::rstest]
fn keybinds_line_one_shows_close_archive_insert() {
    // Given a session with one entry.
    let session = make_session_with_entries(1);

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then keybinds line 1 (5 rows above bottom border) contains x, a, i.
    let line1_y = popup_area.y + popup_area.height - 5;
    let line1_row = buffer_row(&buffer, line1_y, popup_area.x + popup_area.width);

    assert!(
        line1_row.contains('x'),
        "line 1 should contain 'x' keybind, got: {line1_row}"
    );
    assert!(
        line1_row.contains('a'),
        "line 1 should contain 'a' keybind, got: {line1_row}"
    );
    assert!(
        line1_row.contains('i'),
        "line 1 should contain 'i' keybind, got: {line1_row}"
    );
}

#[rstest::rstest]
fn keybinds_line_two_shows_continue_rename() {
    // Given a session with one entry.
    let session = make_session_with_entries(1);

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then keybinds line 2 (4 rows above bottom border) contains c, r.
    let line2_y = popup_area.y + popup_area.height - 4;
    let line2_row = buffer_row(&buffer, line2_y, popup_area.x + popup_area.width);

    assert!(
        line2_row.contains('c'),
        "line 2 should contain 'c' keybind, got: {line2_row}"
    );
    assert!(
        line2_row.contains('r'),
        "line 2 should contain 'r' keybind, got: {line2_row}"
    );
}

#[rstest::rstest]
fn popup_title_shows_session_title() {
    // Given a session with a custom title.
    let session = make_session_with_title("My Custom Session");

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the top border row contains the session title.
    let top_row = buffer_row(&buffer, popup_area.y, popup_area.x + popup_area.width);
    assert!(
        top_row.contains("My Custom Session"),
        "popup top border should contain 'My Custom Session', got: {top_row}"
    );
}

// ---------------------------------------------------------------------------
// Shared status line: cwd row + stats/model row, previewed session's own data
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn model_shows_provider_and_model_on_the_info_row() {
    // Given a session with a specific model set.
    let mut session = ChatSessionState::new();
    session.set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the info row (2 rows above the bottom border) shows the model in
    // the status bar's format.
    let info_y = popup_area.y + popup_area.height - 2;
    let info_row = buffer_row(&buffer, info_y, popup_area.x + popup_area.width);
    assert!(
        info_row.contains("(ollama)/llama3"),
        "info line should contain '(ollama)/llama3', got: {info_row}"
    );
}

#[rstest::rstest]
fn model_shows_no_model_selected_when_unset() {
    // Given a default session (no provider selected).
    let session = ChatSessionState::new();

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the info row shows "no model selected".
    let info_y = popup_area.y + popup_area.height - 2;
    let info_row = buffer_row(&buffer, info_y, popup_area.x + popup_area.width);
    assert!(
        info_row.contains("no model selected"),
        "info line should show 'no model selected', got: {info_row}"
    );
}

#[rstest::rstest]
fn cwd_shows_on_its_own_row_above_the_info_row() {
    // Given a session with a cwd and a model.
    let cwd = path_outside_home("test-project");
    let mut session = ChatSessionState::new();
    session.set_cwd(cwd.clone());
    session.set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the cwd sits on the row directly above the info row, and the info
    // row carries the model — two rows, not one combined line.
    let cwd_label = cwd.to_string();
    let cwd_y = popup_area.y + popup_area.height - 3;
    let cwd_row = buffer_row(&buffer, cwd_y, popup_area.x + popup_area.width);
    assert!(
        cwd_row.contains(cwd_label.as_str()),
        "cwd row should contain the cwd, got: {cwd_row}"
    );
    let info_y = popup_area.y + popup_area.height - 2;
    let info_row = buffer_row(&buffer, info_y, popup_area.x + popup_area.width);
    assert!(
        info_row.contains("(ollama)/llama3"),
        "info row should contain the model, got: {info_row}"
    );
    assert!(
        !cwd_row.contains("(ollama)"),
        "the cwd row must not carry the model: {cwd_row}"
    );
}

#[rstest::rstest]
fn cwd_shortens_a_home_relative_path_with_a_tilde() {
    // Given a session whose cwd is under the user's home directory.
    let mut session = ChatSessionState::new();
    let home = dirs::home_dir().expect("home dir exists");
    let home = MarkedPathBuf::<Absolute>::new(home).expect("home dir is absolute");
    session.set_cwd(
        home.join_all(["projects", "my-app"])
            .expect("home joins bare names"),
    );

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the cwd row shows the `~/…` form, the same collapse the status
    // bar's cwd line applies. The collapse keeps the platform's native
    // separators — the contract `shorten_path` itself is pinned to — so
    // the expected label is built component-wise.
    let cwd_y = popup_area.y + popup_area.height - 3;
    let cwd_row = buffer_row(&buffer, cwd_y, popup_area.x + popup_area.width);
    let expected = std::path::Path::new("~")
        .join("projects")
        .join("my-app")
        .display()
        .to_string();
    assert!(
        cwd_row.contains(&expected),
        "cwd row should show the ~-shortened path, got: {cwd_row}"
    );
}

#[rstest::rstest]
fn info_row_shows_the_previewed_sessions_token_stats() {
    // Given a session whose ledger reports sent/received counts.
    let mut session = ChatSessionState::new();
    session.set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    session.push_token_record(jinn_token_count_msg::TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1500,
        tokens_received: 750,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the info row opens with the status bar's indicator arrows and the
    // session's own counts — not the active session's (the active session
    // here is a different, empty session).
    let info_y = popup_area.y + popup_area.height - 2;
    let info_row = buffer_row(&buffer, info_y, popup_area.x + popup_area.width);
    assert!(
        info_row.contains("\u{2191}1.5k \u{2193}750"),
        "info line should show the previewed session's counts, got: {info_row}"
    );
}

#[rstest::rstest]
fn info_row_shows_nonzero_context_percent_for_an_unactivated_session() {
    // Given a session that was never activated this run — its in-memory state
    // carries no cached context size, but its ledger records a dispatch whose
    // whole-context estimate was 10k tokens — and a model cache naming that
    // model's 200k context length.
    let mut session = ChatSessionState::new();
    session.set_model(ModelSelection::Single(
        "openrouter/anthropic/claude-sonnet-4".to_owned(),
    ));
    session.push_token_record(jinn_token_count_msg::TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 10_000,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });
    let model_cache =
        model_cache_with_context_length("openrouter", "anthropic/claude-sonnet-4", 200_000);

    // When rendering the preview. (Wide frame: the popup is 60% of it, and the
    // full info row — counts, context percentage, cost, turns, model — needs
    // the room to render unclipped.)
    let (buffer, popup_area) = render_preview_with_cache(&session, 160, 40, Some(&model_cache));

    // Then the info row shows a non-zero percentage in the status bar's
    // NN.N%/<max> format — 10k/200k = 5.0% — instead of the 0.0% a session
    // without a cached assembled size used to read.
    let info_y = popup_area.y + popup_area.height - 2;
    let info_row = buffer_row(&buffer, info_y, popup_area.x + popup_area.width);
    assert!(
        info_row.contains("5.0%/200k"),
        "info line should show a non-zero context percentage for an unactivated \
         session, got: {info_row}"
    );
}

#[rstest::rstest]
fn info_row_shows_the_previewed_sessions_turn_count() {
    // Given a session with a user-and-assistant exchange.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.push_entry(ChatEntry::assistant("hi there"));

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the info row carries the previewed session's turn counter.
    let info_y = popup_area.y + popup_area.height - 2;
    let info_row = buffer_row(&buffer, info_y, popup_area.x + popup_area.width);
    assert!(
        info_row.contains("\u{21BB}2"),
        "info line should show the previewed session's turn count, got: {info_row}"
    );
}

#[rstest::rstest]
fn preview_renders_no_aggregate_anywhere() {
    // Given a state whose active session is empty but whose previewed session
    // carries a ledger — if the preview leaked aggregate (or active-session)
    // numbers, they would show here.
    let mut session = ChatSessionState::new();
    session.set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    session.push_entry(ChatEntry::user("hello"));
    session.push_entry(ChatEntry::assistant("hi there"));

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then no row of the popup carries a tree glyph or a session-count glyph:
    // both are aggregates over the session tree, and the preview shows one
    // session's own numbers only.
    for y in popup_area.y..popup_area.y + popup_area.height {
        let row = buffer_row(&buffer, y, popup_area.x + popup_area.width);
        assert!(
            !row.contains('\u{1F333}'),
            "no tree aggregate glyph may appear in the preview, row {y}: {row}"
        );
        assert!(
            !row.contains('\u{29C9}'),
            "no session-count glyph may appear in the preview, row {y}: {row}"
        );
    }
}

#[rstest::rstest]
fn preview_never_shows_a_transient_hint_in_place_of_the_model() {
    // Given a previewed session with a model — the popup holds no hint cell,
    // so nothing can replace the model on the info row.
    let mut session = ChatSessionState::new();
    session.set_model(ModelSelection::Single("ollama/llama3".to_owned()));

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the info row ends in the model.
    let info_y = popup_area.y + popup_area.height - 2;
    let info_row = buffer_row(&buffer, info_y, popup_area.x + popup_area.width);
    assert!(
        info_row.contains("(ollama)/llama3"),
        "the shared info line always ends in the model, got: {info_row}"
    );
}

// ---------------------------------------------------------------------------
// Completion badge tests
// ---------------------------------------------------------------------------

/// Builds a session whose task list has `total` tasks, `completed` of which
/// are marked [`TaskStatus::Completed`].
fn session_with_tasks(completed: usize, total: usize) -> ChatSessionState {
    let mut session = ChatSessionState::new();
    let done = std::iter::repeat("done".to_owned())
        .map(|name| (name, TaskStatus::Completed))
        .take(completed);
    let todo = std::iter::repeat("todo".to_owned())
        .map(|name| (name, TaskStatus::Pending))
        .take(total.saturating_sub(completed));
    let tasks = done.chain(todo).collect();
    session.task_list_mut().set_from_inputs(&[PhaseInput {
        description: "Build".to_owned(),
        tasks,
    }]);
    session
}

#[rstest::rstest]
fn badge_renders_counts_and_percentage_in_top_border() {
    // Given a session with 3 tasks, 1 completed (1*100/3 = 33%).
    let session = session_with_tasks(1, 3);

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the top border row contains the badge "1/3 · 33%".
    let top_row = buffer_row(&buffer, popup_area.y, popup_area.x + popup_area.width);
    assert!(
        top_row.contains("1/3"),
        "top border should contain '1/3' badge, got: {top_row}"
    );
    assert!(
        top_row.contains("33%"),
        "top border should contain '33%' (truncated), got: {top_row}"
    );
    assert!(
        top_row.contains('\u{00B7}'),
        "top border should contain '·' separator, got: {top_row}"
    );
}

#[rstest::rstest]
fn badge_hidden_when_task_list_empty() {
    // Given a session with no tasks.
    let mut session = ChatSessionState::new();
    session.set_title("No Tasks".to_owned());

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then the top border row has no '%' badge but still shows the title.
    let top_row = buffer_row(&buffer, popup_area.y, popup_area.x + popup_area.width);
    assert!(
        !top_row.contains('%'),
        "top border should not contain a percentage badge when empty, got: {top_row}"
    );
    assert!(
        top_row.contains("No Tasks"),
        "top border should still show the session title, got: {top_row}"
    );
}

#[rstest::rstest]
fn badge_uses_streaming_color() {
    // Given a session with tasks.
    let streaming = default_theme().streaming;
    let session = session_with_tasks(1, 3);

    // When rendering the preview.
    let (buffer, popup_area) = render_preview(&session, 80, 40);

    // Then at least one cell in the top border row uses the streaming color.
    let top_border_y = popup_area.y;
    let has_streaming = (popup_area.x..popup_area.x + popup_area.width)
        .filter_map(|x| buffer.cell((x, top_border_y)))
        .any(|cell| cell.fg == streaming);
    assert!(
        has_streaming,
        "some top border cell should use the streaming color {streaming:?}"
    );
}

// ---------------------------------------------------------------------------
// Placement: content is anchored to the bottom of the content area
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn a_short_preview_sits_on_the_last_content_row() {
    // Given a session with one short entry, in a frame with a full-height popup.
    let session = session_with_one_short_entry();

    // When rendering the preview.
    let (buffer, popup_area) = render_preview_at_cursor(&session, 100, 40, 35);

    // Then the entry is on the content area's last row, directly above the
    // keybinds bar, with the unused rows above it rather than below.
    let last_row = buffer_row(
        &buffer,
        last_content_row(popup_area),
        popup_area.x + popup_area.width,
    );
    assert!(
        last_row.contains("message 0"),
        "the newest line should sit on the last content row, got: {last_row}"
    );
}

#[rstest::rstest]
fn an_overflowing_preview_drops_its_oldest_lines() {
    // Given a session whose preview overflows the content area.
    let session = session_with_many_lines();

    // When rendering the preview.
    let (buffer, popup_area) = render_preview_at_cursor(&session, 100, 40, 35);
    let content_rows = content_rows_of(popup_area);
    let visible: String = (0..content_rows)
        .map(|i| {
            buffer_row(
                &buffer,
                popup_area.y + 1 + i,
                popup_area.x + popup_area.width,
            )
        })
        .collect();

    // Then the newest entry is the last visible content row, and the oldest of
    // the overflowing text is gone.
    let last_row = buffer_row(
        &buffer,
        last_content_row(popup_area),
        popup_area.x + popup_area.width,
    );
    assert!(
        last_row.contains("line 4-9"),
        "the newest line should be the last visible row, got: {last_row}"
    );
    assert!(
        !visible.contains("line 0-0"),
        "overflow should be dropped from the front, not the back"
    );
}

#[rstest::rstest]
fn the_newest_line_survives_a_clamped_popup() {
    // Given a frame too short above the cursor for the full popup, with a
    // preview that overflows even the clamped content area.
    let session = session_with_many_lines();

    // When rendering the preview.
    let (buffer, popup_area) = render_preview_at_cursor(&session, 100, 40, 22);
    let content_rows = content_rows_of(popup_area);

    // Then the newest entry is still the last visible content row, directly
    // above the footer. This is the case that top-anchoring gets wrong.
    assert!(
        content_rows < u16::try_from(PREVIEW_MAX_LINES).expect("budget fits a u16"),
        "this cursor row was chosen so the popup is capped below the full height"
    );
    let last_row = buffer_row(
        &buffer,
        last_content_row(popup_area),
        popup_area.x + popup_area.width,
    );
    assert!(
        last_row.contains("line 4-9"),
        "the newest line should stay on the last content row in a capped popup, \
         got: {last_row}"
    );
}

#[rstest::rstest]
fn an_empty_history_gets_a_full_height_popup() {
    // Given a session with no entries at all.
    let session = make_session_with_title("empty");

    // When rendering the preview.
    let (_buffer, popup_area) = render_preview(&session, 100, 40);

    // Then the box is the same height as one full of text, not collapsed onto
    // its chrome — the surface does not appear and disappear with the content.
    assert_eq!(
        popup_area.height, 26,
        "an empty history should still get the full-height popup"
    );
}

// ---------------------------------------------------------------------------
// Geometry: the popup is a fixed-height surface anchored to the cursor
// ---------------------------------------------------------------------------

/// The content rows inside a popup rect: its inner area less the four footer
/// rows. Mirrors production so a test can name the rows content is drawn on.
fn content_rows_of(popup_rect: Rect) -> u16 {
    popup_rect.height.saturating_sub(2).saturating_sub(4)
}

/// The rect production computes, for tests that are not about the no-room case.
///
/// Every caller here picks a cursor row with room for a box, so the unwrap is a
/// guard against a test silently measuring nothing rather than a measure of the
/// floor. The floor itself is asserted directly, on
/// [`session_preview_popup_rect`].
fn popup_rect_or_panic(frame_area: Rect, cursor_y: u16) -> Rect {
    session_preview_popup_rect(frame_area, cursor_y)
        .unwrap_or_else(|| panic!("no popup rect at cursor_y={cursor_y} with frame {frame_area:?}"))
}

#[rstest::rstest]
#[case::normal(30)]
#[case::near_top(10)]
#[case::mid_screen(20)]
fn popup_bottom_edge_sits_two_rows_above_cursor(#[case] cursor_y: u16) {
    // Given a frame area and a cursor position.
    let frame_area = Rect::new(0, 0, 80, 40);

    // When computing the popup rect.
    let popup_rect = popup_rect_or_panic(frame_area, cursor_y);

    // Then the popup bottom edge + 2 = cursor_y, leaving a one-row gap.
    assert_eq!(
        popup_rect.y + popup_rect.height + 2,
        cursor_y,
        "popup bottom edge + 2 should equal cursor_y ({cursor_y}), \
         got popup_y={}, popup_height={}",
        popup_rect.y,
        popup_rect.height
    );
}

#[rstest::rstest]
fn popup_follows_cursor_not_section_top() {
    // Given two cursor positions, each with room for the full popup.
    let frame_area = Rect::new(0, 0, 80, 60);

    // When computing popup rects for each cursor.
    let rect_at_30 = popup_rect_or_panic(frame_area, 30);
    let rect_at_35 = popup_rect_or_panic(frame_area, 35);

    // Then each popup is anchored to its own cursor, not to a fixed section top.
    // The height is constant, so the top row is what moves with the cursor.
    assert_eq!(
        rect_at_30.height, rect_at_35.height,
        "the popup height is fixed"
    );
    assert_eq!(
        rect_at_30.y + rect_at_30.height + 2,
        30,
        "popup at cursor_y=30 should sit two rows above its cursor"
    );
    assert_eq!(
        rect_at_35.y + rect_at_35.height + 2,
        35,
        "popup at cursor_y=35 should sit two rows above its cursor"
    );
    assert_eq!(
        rect_at_35.y,
        rect_at_30.y + 5,
        "moving the cursor down five rows should move the popup down five rows"
    );
}

#[rstest::rstest]
fn popup_height_is_the_budget_plus_chrome() {
    // Given a frame with ample room above the cursor.
    let frame_area = Rect::new(0, 0, 80, 60);

    // When computing the popup rect.
    let popup_rect = popup_rect_or_panic(frame_area, 50);

    // Then the content area is the preview's own line budget — the same constant
    // the layout worker renders to, not a second number kept alongside it — and
    // the box adds exactly the two borders and four footer rows around it.
    assert_eq!(
        content_rows_of(popup_rect),
        u16::try_from(PREVIEW_MAX_LINES).expect("the budget fits a u16"),
        "the popup's content area must be the preview line budget"
    );
    assert_eq!(
        popup_rect.height, 26,
        "20 content rows + 4 footer rows + 2 borders"
    );
}

#[rstest::rstest]
fn popup_height_does_not_depend_on_how_much_text_it_holds() {
    // Given a frame with room for the full popup.
    let frame_area = Rect::new(0, 0, 80, 60);
    let cursor_y = 50;

    // When computing rects for every amount of content the worker can return.
    let heights: Vec<u16> = [0, 1, 5, 12, PREVIEW_MAX_LINES]
        .iter()
        .map(|_| popup_rect_or_panic(frame_area, cursor_y).height)
        .collect();

    // Then they are all the same, so the box does not resize as a reply grows
    // or a session is selected.
    assert!(
        heights.windows(2).all(|w| w[0] == w[1]),
        "the popup resized with its content: {heights:?}"
    );
}

#[rstest::rstest]
fn popup_height_capped_when_cursor_near_top() {
    // Given a cursor very near the top of the terminal.
    let frame_area = Rect::new(0, 0, 80, 40);
    let cursor_y = 9u16;

    // When computing the popup rect.
    let popup_rect = popup_rect_or_panic(frame_area, cursor_y);

    // Then the popup height is capped (max_height = 9 - 0 - 2 = 7, the floor).
    assert_eq!(
        popup_rect.height, 7,
        "popup height should be capped when cursor is near top, \
         got: {}",
        popup_rect.height
    );

    // And the popup does not extend past the gap boundary toward the cursor.
    assert!(
        popup_rect.y + popup_rect.height < cursor_y,
        "popup should not encroach on the gap above cursor"
    );
}

#[rstest::rstest]
fn popup_height_capped_below_the_full_height() {
    // Given a cursor with more room than the cap but less than the full popup:
    // 20 rows above the cursor, so the cap (20 - 2) bites below the full 26.
    let frame_area = Rect::new(0, 0, 80, 40);
    let cursor_y = 22u16;
    let full_height = popup_rect_or_panic(frame_area, 40).height;

    // When computing the popup rect.
    let popup_rect = popup_rect_or_panic(frame_area, cursor_y);

    // Then the cap genuinely binds — the box is shorter than the full height,
    // which a test at the floor could not tell apart from the cap.
    assert_eq!(
        popup_rect.height, 20,
        "the cap should yield cursor_y - gap rows, got: {}",
        popup_rect.height
    );
    assert!(
        popup_rect.height < full_height,
        "this cursor row was chosen so the cap binds below the full height"
    );
}

#[rstest::rstest]
fn no_popup_is_anchored_to_a_cursor_row_it_would_cover() {
    // Given the shortest terminal the app lays out — below this it renders the
    // "too small" notice and no popup path at all — and a taller one.
    let smallest = Rect::new(0, 0, 80, 15);
    let taller = Rect::new(0, 0, 80, 40);

    // When computing the popup rect for every row the cursor could land on.
    for frame_area in [smallest, taller] {
        for cursor_y in frame_area.y..=frame_area.y.saturating_add(frame_area.height) {
            let Some(popup_rect) = session_preview_popup_rect(frame_area, cursor_y) else {
                continue;
            };

            // Then no row of the box lands on the cursor's row, so the row the
            // popup describes is still the row the user can see.
            assert!(
                popup_rect.y > cursor_y
                    || popup_rect.y.saturating_add(popup_rect.height) <= cursor_y,
                "the popup at cursor_y={cursor_y} is {} rows at y={}, covering the cursor: {popup_rect:?}",
                popup_rect.height,
                popup_rect.y
            );
        }
    }
}

#[rstest::rstest]
fn no_popup_is_drawn_under_the_minimum_height_of_room() {
    // Given a frame whose top rows leave less room than the shortest box needs.
    let frame_area = Rect::new(0, 0, 80, 40);

    // When computing the rect for a cursor within that space, and again with
    // exactly enough of it.
    let with_no_room = session_preview_popup_rect(frame_area, 7);
    let below_the_gap = session_preview_popup_rect(frame_area, 8);
    let with_enough_room = session_preview_popup_rect(frame_area, 9);

    // Then the first two draw nothing at any height, and the third draws a box
    // at the floor: the boundary is the room itself, not a tiebreak in the
    // height arithmetic.
    assert_eq!(
        with_no_room, None,
        "a cursor 7 rows down leaves no room for a box"
    );
    assert_eq!(
        below_the_gap, None,
        "a cursor 8 rows down leaves only the gap, and no box fits in it"
    );
    assert_eq!(
        with_enough_room.map(|rect| rect.height),
        Some(7),
        "the shortest drawable box is the floor, and it starts where the room does"
    );
}

/// The popup's content area when the render has not come back.
///
/// A frame that renders nothing while it waits is indistinguishable from a
/// frame that is stuck, so the loading state has to be visible.
mod loading_state {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;
    use jinn_slices::spinner_glyph;

    /// The popup drawn by the loading renderer, at the geometry production uses.
    ///
    /// Takes the same rect production computes, so a test cannot pass against a
    /// geometry the render pass never draws with.
    fn draw_loading(
        session: &ChatSessionState,
        term_width: u16,
        term_height: u16,
    ) -> (Buffer, Rect) {
        let theme = default_theme();
        let frame_area = Rect::new(0, 0, term_width, term_height);
        let popup_area = popup_rect_or_panic(frame_area, 30);

        let (mut terminal, _) = setup_term(term_width, term_height);
        terminal
            .draw(|frame| {
                render_session_preview_loading(
                    frame,
                    popup_area,
                    session,
                    &theme,
                    None,
                    no_working_time(),
                );
            })
            .expect("draw");
        (terminal.backend().buffer().clone(), popup_area)
    }

    /// The buffer as one string, for a whole-popup assertion.
    fn buffer_to_string(buffer: &ratatui::buffer::Buffer) -> String {
        let area = buffer.area();
        (area.y..area.y + area.height)
            .map(|y| buffer_row(buffer, y, area.x + area.width))
            .collect()
    }

    /// Every glyph `spinner_glyph` can return, so the assertion is not tied to
    /// which frame the test happens to catch.
    fn any_spinner_glyph() -> Vec<String> {
        (0..8)
            .map(|step| {
                spinner_glyph(std::time::Duration::from_millis(
                    u64::try_from(step).unwrap_or(0)
                        * u64::try_from(jinn_slices::SPINNER_INTERVAL.as_millis()).unwrap_or(1),
                ))
                .to_owned()
            })
            .collect()
    }

    #[rstest::rstest]
    fn the_loading_popup_reserves_a_content_row() {
        // Given a frame with room above the cursor for the full popup.
        let frame_area = Rect::new(0, 0, 100, 40);

        // When the loading popup's rect is computed the way production does.
        let popup_area = popup_rect_or_panic(frame_area, 30);

        // Then rows remain for content once the borders and footer are taken.
        // The height no longer depends on a line count, so the loading popup is
        // the same box the ready popup draws and cannot lose its content area.
        assert!(
            content_rows_of(popup_area) > 0,
            "the loading popup must leave room for the spinner, got {} content rows in a {} row popup",
            content_rows_of(popup_area),
            popup_area.height
        );
    }

    #[rstest::rstest]
    fn the_loading_and_ready_popups_are_the_same_size() {
        // Given a frame with room for the full popup.
        let frame_area = Rect::new(0, 0, 100, 40);

        // When the rect is computed — which no longer takes a line count, so
        // there is nothing for the loading state to size itself differently to.
        let loading = popup_rect_or_panic(frame_area, 30);
        let ready = popup_rect_or_panic(frame_area, 30);

        // Then the box does not resize when the render comes back.
        assert_eq!(
            loading, ready,
            "the popup must not change shape between loading and ready"
        );
    }

    #[rstest::rstest]
    fn the_loading_indicator_sits_on_the_last_content_row() {
        // Given a session whose preview has not been rendered.
        let session = make_session_with_title("busy");
        let (buffer, popup_area) = draw_loading(&session, 100, 40);
        let expected_row = last_content_row(popup_area);

        // When reading every content row, looking for the spinner.
        let row_with_spinner = (0..content_rows_of(popup_area))
            .map(|i| popup_area.y + 1 + i)
            .find(|y| {
                let row = buffer_row(&buffer, *y, popup_area.x + popup_area.width);
                any_spinner_glyph().iter().any(|g| row.contains(g))
            });

        // Then the spinner is on the content area's last row, where content will
        // arrive, rather than floating at the top of an empty box.
        assert_eq!(
            row_with_spinner,
            Some(expected_row),
            "the loading indicator should be on the last content row (row {expected_row})"
        );
    }

    #[rstest::rstest]
    fn the_loading_indicator_is_centred_in_the_content_area() {
        // Given a session whose preview has not been rendered.
        let session = make_session_with_title("busy");
        let (buffer, popup_area) = draw_loading(&session, 100, 40);
        let row_y = last_content_row(popup_area);
        let content_x = popup_area.x + 1;
        let content_width = popup_area.width - 2;

        // When the loading line's ink is measured on that row. The popup is
        // right-aligned, so only the content area's own columns are read —
        // measuring from column zero would count the screen's left margin.
        let row: String = (content_x..content_x + content_width)
            .filter_map(|x| buffer.cell((x, row_y)).map(|c| c.symbol().to_owned()))
            .collect();
        let indent = row.chars().take_while(|c| *c == ' ').count();
        let ink = row.trim_end().len() - indent;
        let left_slack = indent;
        let right_slack = content_width as usize - indent - ink;

        // Then the line is centred — the same slack either side, to within the
        // one column an odd remainder cannot split.
        assert!(
            left_slack.abs_diff(right_slack) <= 1,
            "the loading line is not centred: {left_slack} left, {right_slack} right, \
             row: {row:?}"
        );
    }

    #[rstest::rstest]
    fn the_loading_state_shows_a_spinner() {
        // Given a session whose preview has not been rendered.
        let session = make_session_with_title("busy");

        // When drawing the loading popup.
        let (buffer, _popup_area) = draw_loading(&session, 100, 40);

        // Then the content area carries a spinner glyph.
        let screen = buffer_to_string(&buffer);
        assert!(
            any_spinner_glyph().iter().any(|g| screen.contains(g)),
            "expected a spinner glyph on screen"
        );
    }

    #[rstest::rstest]
    fn the_loading_state_does_not_show_entry_text() {
        // Given a session whose entries carry distinctive text.
        let mut session = make_session_with_title("busy");
        session.push_entry(ChatEntry::user("SECRETENTRYTEXT"));

        // When drawing the loading popup.
        let (buffer, _popup_area) = draw_loading(&session, 100, 40);

        // Then none of it is drawn, because the render has not come back.
        let screen = buffer_to_string(&buffer);
        assert!(
            !screen.contains("SECRETENTRYTEXT"),
            "a loading popup must not draw text it has not been given"
        );
    }

    #[rstest::rstest]
    fn the_loading_state_shows_the_session_title() {
        // Given a titled session whose preview has not been rendered.
        let session = make_session_with_title("busy");

        // When drawing the loading popup.
        let (buffer, _popup_area) = draw_loading(&session, 100, 40);

        // Then the chrome is already drawn, so only the content waits.
        assert!(buffer_to_string(&buffer).contains("busy"));
    }

    #[rstest::rstest]
    fn the_loading_state_shows_the_keybinds() {
        // Given a session whose preview has not been rendered.
        let session = make_session_with_title("busy");

        // When drawing the loading popup.
        let (buffer, _popup_area) = draw_loading(&session, 100, 40);

        // Then the footer is present, matching the ready state.
        assert!(buffer_to_string(&buffer).contains("archive"));
    }

    #[rstest::rstest]
    fn the_loading_state_shows_the_previewed_sessions_cwd() {
        // Given a session with a distinctive cwd whose preview has not rendered.
        let cwd = path_outside_home("loadtest-cwd");
        let mut session = make_session_with_title("busy");
        session.set_cwd(cwd.clone());

        // When drawing the loading popup.
        let (buffer, popup_area) = draw_loading(&session, 100, 40);

        // Then the loading footer already carries the shared status line — the
        // cwd row shows the session's own directory — because loading and ready
        // share one inner renderer, so the footer never flickers.
        let cwd_label = cwd.to_string();
        let cwd_y = popup_area.y + popup_area.height - 3;
        let cwd_row = buffer_row(&buffer, cwd_y, popup_area.x + popup_area.width);
        assert!(
            cwd_row.contains(cwd_label.as_str()),
            "the loading footer should already show the previewed session's cwd, got: {cwd_row}"
        );
    }

    #[rstest::rstest]
    fn the_loading_state_shows_no_aggregate() {
        // Given a session whose preview has not been rendered.
        let session = make_session_with_title("busy");

        // When drawing the loading popup.
        let (buffer, popup_area) = draw_loading(&session, 100, 40);

        // Then the loading footer carries no aggregate glyphs either — the
        // loading and ready states share one footer, and that footer shows
        // one session's own numbers only.
        for y in popup_area.y..popup_area.y + popup_area.height {
            let row = buffer_row(&buffer, y, popup_area.x + popup_area.width);
            assert!(
                !row.contains('\u{1F333}') && !row.contains('\u{29C9}'),
                "no aggregate glyph may appear in the loading popup, row {y}: {row}"
            );
        }
    }

    #[rstest::rstest]
    fn the_loading_label_uses_the_streaming_color() {
        // Given a session whose preview has not been rendered.
        let session = make_session_with_title("busy");
        let theme = default_theme();
        let frame_area = Rect::new(0, 0, 100, 40);
        let popup_area = popup_rect_or_panic(frame_area, 30);

        // When the loading renderer draws it.
        let (mut terminal, _) = setup_term(100, 40);
        terminal
            .draw(|frame| {
                render_session_preview_loading(
                    frame,
                    popup_area,
                    &session,
                    &theme,
                    None,
                    no_working_time(),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();

        // Then the label carries the theme's streaming color, the same one the
        // chat log's loading indicator uses, so "working" reads the same in both
        // places. Muted grey read as "nothing here" rather than "wait".
        // Scanned from the label's own first cell: the columns to its left are
        // the centring indent and the leftmost cell of the row is the border,
        // neither of which says anything about the label's color.
        let label_row = last_content_row(popup_area);
        let label_start = (popup_area.x + 1..popup_area.x + popup_area.width)
            .find(|x| {
                buffer
                    .cell((*x, label_row))
                    .is_some_and(|c| c.symbol() != " ")
            })
            .expect("the label row carries the label");
        let fg = (label_start..popup_area.x + popup_area.width)
            .filter_map(|x| buffer.cell((x, label_row)))
            .map(|cell| cell.fg)
            .find(|fg| *fg != theme.border_unfocused)
            .expect("the label row carries text");
        assert_eq!(
            fg, theme.streaming,
            "the loading label must use the streaming color, not muted grey",
        );
    }

    #[rstest::rstest]
    fn a_session_with_no_entries_draws_no_content() {
        // Given a session with an empty history, rendered and complete.
        let session = make_session_with_title("empty");
        let lines: Vec<ratatui::text::Line<'static>> = Vec::new();

        // When the ready renderer draws it.
        let theme = default_theme();
        let frame_area = Rect::new(0, 0, 100, 40);
        let popup_area = popup_rect_or_panic(frame_area, 30);
        let (mut terminal, _) = setup_term(100, 40);
        terminal
            .draw(|frame| {
                render_session_preview(
                    frame,
                    popup_area,
                    &session,
                    &theme,
                    &lines,
                    None,
                    no_working_time(),
                );
            })
            .expect("draw");

        // Then the popup is chrome-only — no spinner, because it is not waiting.
        // Scanned over the content area only: the shared status line in the
        // footer legitimately carries spinner-shaped glyphs (the `/` in a
        // model name or a context ratio), which say nothing about loading.
        let content = (0..content_rows_of(popup_area))
            .map(|i| buffer_row(terminal.backend().buffer(), popup_area.y + 1 + i, 100))
            .collect::<String>();
        assert!(
            !any_spinner_glyph().iter().any(|g| content.contains(g)),
            "an empty preview is complete, not loading: {content}"
        );
    }
}

/// The popup's border, read from the cell at the top-left corner.
///
/// The corner is the one border cell whose position is fixed by the rect, so
/// it says what the border was drawn in without depending on where any title or
/// badge happens to start.
fn border_fg(session: &ChatSessionState) -> ratatui::style::Color {
    let (buffer, popup_area) = render_preview(session, 100, 40);
    buffer
        .cell((popup_area.x, popup_area.y))
        .expect("the popup has a top-left corner")
        .fg
}

/// A session of `origin`, with one entry so the popup has content to show.
fn session_of_origin(origin: jinn_session_msg::SessionOrigin) -> ChatSessionState {
    let mut session = make_session_with_entries(1);
    session.set_origin(origin);
    session
}

#[rstest::rstest]
fn an_bot_sessions_preview_border_is_tinted_for_bots() {
    // Given a bot's session.
    let session = session_of_origin(jinn_session_msg::SessionOrigin::Bot);

    // When its preview is rendered.
    let color = border_fg(&session);

    // Then the border carries the bot color the session list uses.
    assert_eq!(color, default_theme().bot_fg);
}

#[rstest::rstest]
fn a_subagent_sessions_preview_border_is_tinted_for_subagents() {
    // Given a subagent's session.
    let session = session_of_origin(jinn_session_msg::SessionOrigin::Subagent);

    // When its preview is rendered.
    let color = border_fg(&session);

    // Then the border carries the subagent color the session list uses.
    assert_eq!(color, default_theme().subagent_fg);
}

#[rstest::rstest]
fn a_user_sessions_preview_border_is_the_ordinary_unfocused_border() {
    // Given a plain user session.
    let session = session_of_origin(jinn_session_msg::SessionOrigin::User);

    // When its preview is rendered.
    let color = border_fg(&session);

    // Then nothing distinguishes it — the border is exactly what it always was.
    assert_eq!(color, default_theme().border_unfocused);
}

#[rstest::rstest]
fn a_fork_of_an_bot_sessions_preview_border_is_the_ordinary_border() {
    // Given a session forked from a bot's conversation.
    let session = session_of_origin(jinn_session_msg::SessionOrigin::Fork);

    // When its preview is rendered.
    let color = border_fg(&session);

    // Then the fork is an ordinary conversation and says so, whatever it was
    // branched from.
    assert_eq!(color, default_theme().border_unfocused);
}

#[rstest::rstest]
fn an_bot_sessions_preview_border_tint_leaves_the_title_alone() {
    // Given a bot's session with a title.
    let mut session = session_of_origin(jinn_session_msg::SessionOrigin::Bot);
    session.set_title("nightly review".to_owned());

    // When its preview is rendered.
    let (buffer, popup_area) = render_preview(&session, 100, 40);
    let top = buffer_row(&buffer, popup_area.y, popup_area.width);

    // Then the title is still in the popup's own title color — only the border
    // follows the session's kind.
    let cell = buffer
        .cell((popup_area.x + 1, popup_area.y))
        .expect("the top border holds the title");
    assert!(
        top.contains("nightly review"),
        "the title must still show: {top}"
    );
    assert_eq!(cell.fg, default_theme().popup_title);
}
