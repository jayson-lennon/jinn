//! Session lifecycle picker and arg input popup rendering.

use crate::common::render_ctx::RenderCtx;
use crate::feat::session_lifecycle::command_template::{CommandTemplate, split_preserving_quotes};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// Horizontal padding fraction for the arg input popup (10% each side).
const ARG_POPUP_H_PAD_FRAC: f32 = 0.10;
/// Minimum popup width in cells.
const ARG_POPUP_MIN_WIDTH: u16 = 40;

/// Returns the color for a parameter segment at the given index.
///
/// Currently returns `theme.accent_action` for all params. To add
/// per-argument gradient or rainbow colors in the future, change only
/// this function to return different colors based on `param_index`.
fn param_color(theme: &crate::feat::theme::Theme, _param_index: usize) -> Color {
    theme.accent_action
}

/// Computes a dynamic popup rectangle sized to fit the arg input content.
///
/// `content_rows` is the number of inner rows needed (command lines + blank + input).
/// Adds 2 for the border and clamps to terminal dimensions.
fn compute_arg_input_popup_rect(area: Rect, content_rows: u16) -> Rect {
    let popup_width = ((f32::from(area.width) * (1.0 - 2.0 * ARG_POPUP_H_PAD_FRAC)).ceil() as u16)
        .max(ARG_POPUP_MIN_WIDTH)
        .min(area.width);

    // border(2) + content rows.
    let popup_height = (content_rows + 2).min(area.height);

    // Integer division is intentional - we're computing cell positions for centering.
    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_x = area.width.saturating_sub(popup_width) / 2;
    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_y = area.height.saturating_sub(popup_height) / 3; // bias toward top third

    Rect::new(popup_x, popup_y, popup_width, popup_height)
}

/// Computes the popup rectangle for the arg input overlay.
///
/// This is used by the TUI layer to register the popup's selectable rect.
/// The popup height is dynamic based on the command's `&&` split and number of params.
#[must_use]
pub fn arg_input_popup_rect(area: Rect, ctx: &RenderCtx) -> Rect {
    let state = ctx.state;
    let arg_state = &state.frontend.arg_input;

    let content_rows = state
        .frontend
        .preferences
        .session_lifecycles
        .iter()
        .find(|l| l.name == arg_state.lifecycle_name)
        .and_then(|l| l.setup.as_ref())
        .and_then(|cmd| match cmd {
            crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(s) => Some(s.as_str()),
            crate::feat::session_lifecycle::builtin::LifecycleCommand::Builtin(_) => None,
        })
        .map_or(1, |cmd| {
            let template = CommandTemplate::parse(cmd);
            let display_args: Vec<String> = split_preserving_quotes(&arg_state.text.input);
            let lines = template.display_line_segments(&display_args);
            (lines.len() as u16) + 2 // command lines + separator + input
        });

    compute_arg_input_popup_rect(area, content_rows)
}

/// Renders the arg input popup for a lifecycle with positional parameters.
///
/// Shows a centered popup with:
/// - Title: "New Session (set script args)"
/// - Multi-line command display split on `&&`, with parameter substitution
///   and syntax highlighting for placeholders/substituted values
/// - Input line at bottom showing current text with cursor
pub fn render_arg_input(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let arg_state = &state.frontend.arg_input;

    // Parse the command template to extract parameter info.
    let template = state
        .frontend
        .preferences
        .session_lifecycles
        .iter()
        .find(|l| l.name == arg_state.lifecycle_name)
        .and_then(|l| l.setup.as_ref())
        .and_then(|cmd| match cmd {
            crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(s) => Some(s.as_str()),
            crate::feat::session_lifecycle::builtin::LifecycleCommand::Builtin(_) => None,
        })
        .map(CommandTemplate::parse);

    let theme = &state.frontend.theme;

    // Split the user's input into tokens for display (preserving quotes).
    let display_args: Vec<String> = split_preserving_quotes(&arg_state.text.input);

    // Compute content height: command lines + blank + input line.
    let content_rows = match &template {
        Some(t) => {
            let lines = t.display_line_segments(&display_args);
            (lines.len() as u16) + 2 // command lines + blank + input
        }
        None => 1, // minimal popup
    };

    // Compute dynamic popup rect.
    let popup_area = compute_arg_input_popup_rect(area, content_rows);

    let Some(template) = template else {
        // No template found - render minimal popup anyway.
        let block = Block::default()
            .title(" New Session (set script args) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border_unfocused));
        frame.render_widget(Clear, popup_area);
        frame.render_widget(block, popup_area);
        return;
    };

    let title = Line::from(Span::styled(
        " New Session (set script args) ",
        Style::default().fg(theme.popup_title),
    ));

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_unfocused));

    // Clear background before rendering the popup borders.
    frame.render_widget(Clear, popup_area);
    frame.render_widget(block, popup_area);

    // Inner area for content (1 padding on each side).
    let inner = Rect {
        x: popup_area.x + 1,
        y: popup_area.y + 1,
        width: popup_area.width.saturating_sub(2),
        height: popup_area.height.saturating_sub(2),
    };

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Get the structured display lines.
    let display_lines = template.display_line_segments(&display_args);

    // Render lines one by one, tracking y position.
    let mut y_offset = inner.y;
    let max_y = inner.y + inner.height;

    // Render each command line with styled segments.
    for line_segments in &display_lines {
        if y_offset >= max_y {
            break;
        }
        let spans: Vec<Span<'_>> = line_segments
            .iter()
            .map(|seg| match seg.param_index {
                Some(idx) => Span::styled(
                    seg.text.clone(),
                    Style::default().fg(param_color(theme, idx)),
                ),
                None => Span::raw(seg.text.clone()),
            })
            .collect();
        let para = Paragraph::new(Line::from(spans));
        frame.render_widget(para, Rect::new(inner.x, y_offset, inner.width, 1));
        y_offset += 1;
    }

    // Separator line between command and input.
    if y_offset < max_y {
        let separator = "─".repeat(inner.width as usize);
        let separator_line = Line::from(Span::styled(
            separator,
            Style::default().fg(theme.border_unfocused),
        ));
        frame.render_widget(
            Paragraph::new(separator_line),
            Rect::new(inner.x, y_offset, inner.width, 1),
        );
        y_offset += 1;
    }

    // Last line: the input line with cursor.
    if y_offset < max_y {
        let input_line = Line::from(Span::raw(format!("> {}", arg_state.text.input)));
        let input_para = Paragraph::new(input_line);
        frame.render_widget(input_para, Rect::new(inner.x, y_offset, inner.width, 1));

        // Compute cursor x position: "> " (2) + grapheme count up to cursor_pos.
        let prefix_len = 2u16; // "> "
        let grapheme_count = arg_state.text.graphemes_before_cursor();
        let cursor_x = (prefix_len + grapheme_count as u16).min(inner.width.saturating_sub(1));
        frame.set_cursor_position((inner.x.saturating_add(cursor_x), y_offset));
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::AppState;
    use crate::common::app_state::ArgInputState;
    use crate::feat::preferences_actor::user_preferences::SessionLifecycle;
    use jinn_testutil::setup_term;

    fn make_state_with_args(
        lifecycle_name: &str,
        setup_cmd: Option<&str>,
        input: &str,
        cursor_pos: usize,
    ) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input = ArgInputState {
            lifecycle_name: lifecycle_name.to_owned(),
            template_display: String::new(),
            text: {
                let mut li = crate::common::line_input::LineInput::new();
                li.input = input.to_owned();
                li.cursor_pos = cursor_pos;
                li
            },
        };
        if let Some(cmd) = setup_cmd {
            state
                .frontend
                .preferences
                .session_lifecycles
                .push(SessionLifecycle {
                    name: lifecycle_name.to_owned(),
                    description: None,
                    setup: Some(
                        crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(
                            cmd.to_owned(),
                        ),
                    ),
                    teardown: None,
                });
        }
        state
    }

    #[rstest::rstest]
    fn arg_input_popup_shows_title_in_border_no_template() {
        // Given a state with an unknown lifecycle (no template found).
        let state = make_state_with_args("unknown", None, "", 0);
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering the arg input popup.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_arg_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the popup title appears in the top border.
        let buffer = terminal.backend().buffer().clone();
        let popup_area = compute_arg_input_popup_rect(area, 1);
        let title_line_y = popup_area.y;

        let title_text = " New Session (set script args) ";
        let mut found_title = false;
        for x in popup_area.x..(popup_area.x + popup_area.width).min(buffer.area().width) {
            if let Some(cell) = buffer.cell((x, title_line_y)) {
                let cell_text: &str = cell.symbol();
                if matches!(cell_text, "┌" | "─" | "┐") {
                    continue;
                }
                if title_text.contains(cell_text) {
                    found_title = true;
                    break;
                }
            }
        }
        assert!(found_title, "title should appear in the top border");
    }

    #[rstest::rstest]
    fn arg_input_popup_shows_command_on_first_line() {
        // Given a state with a simple command (no &&).
        let state = make_state_with_args("test", Some("script.sh $1 $2"), "", 0);
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_arg_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the command display appears on the first inner line.
        let buffer = terminal.backend().buffer().clone();
        let input_args: Vec<String> = split_preserving_quotes("");
        let tmpl = CommandTemplate::parse("script.sh $1 $2");
        let lines = tmpl.display_line_segments(&input_args);
        let content_rows = (lines.len() as u16) + 2;
        let popup_area = compute_arg_input_popup_rect(area, content_rows);
        let inner_x = popup_area.x + 1;
        let template_y = popup_area.y + 1;

        if template_y < buffer.area().height {
            // First line should contain "script.sh <1> <2>".
            let row_text: String = (inner_x..inner_x + 30)
                .filter_map(|x| buffer.cell((x, template_y)).map(|c| c.symbol().to_owned()))
                .collect();
            assert!(
                row_text.contains("script.sh"),
                "command should appear on first line, got: {row_text}"
            );
        }
    }

    #[rstest::rstest]
    fn arg_input_popup_shows_input_line() {
        // Given a state with a lifecycle and some typed input.
        let state = make_state_with_args("test", Some("script.sh $1"), "hello world", 5);
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_arg_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the input line ("> hello world") appears at the bottom.
        let buffer = terminal.backend().buffer().clone();
        let input_args: Vec<String> = split_preserving_quotes("hello world");
        let tmpl = CommandTemplate::parse("script.sh $1");
        let lines = tmpl.display_line_segments(&input_args);
        let content_rows = (lines.len() as u16) + 2;
        let popup_area = compute_arg_input_popup_rect(area, content_rows);
        let inner_x = popup_area.x + 1;

        // Input line is 2 rows from the bottom of the popup.
        let input_y = popup_area.y + popup_area.height - 2;

        if input_y < buffer.area().height {
            let expected = "> hello world";
            for (i, expected_ch) in expected.chars().enumerate() {
                let x = inner_x + i as u16;
                if x >= buffer.area().width {
                    break;
                }
                if let Some(cell) = buffer.cell((x, input_y)) {
                    assert_eq!(
                        cell.symbol(),
                        expected_ch.to_string(),
                        "input line mismatch at offset {i}"
                    );
                }
            }
        }
    }

    #[rstest::rstest]
    fn arg_input_popup_handles_no_template() {
        // Given a state with an unknown lifecycle name (no matching template).
        let state = make_state_with_args("nonexistent", None, "foo", 3);
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering - should not panic.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_arg_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the minimal popup still shows the title.
        let buffer = terminal.backend().buffer().clone();
        let popup_area = compute_arg_input_popup_rect(area, 1);
        let title_line_y = popup_area.y;

        let mut found_title = false;
        for x in popup_area.x..(popup_area.x + popup_area.width).min(buffer.area().width) {
            if let Some(cell) = buffer.cell((x, title_line_y)) {
                let cell_text: &str = cell.symbol();
                if matches!(cell_text, "┌" | "─" | "┐") {
                    continue;
                }
                if " New Session (set script args) ".contains(cell_text) {
                    found_title = true;
                    break;
                }
            }
        }
        assert!(found_title, "minimal popup should show title");
    }

    #[rstest::rstest]
    fn arg_input_popup_background_cleared() {
        // Given a state with text in the chat area.
        let state = AppState::default_with_scope_focus();
        let (mut terminal, area) = setup_term(80, 24);

        // First draw something in the background so we can detect if Clear works.
        terminal
            .draw(|frame| {
                // Fill the terminal with characters *before* the popup area.
                let fill = Paragraph::new(Line::from(Span::raw("XXXXX")));
                frame.render_widget(fill, Rect::new(0, 0, 80, 24));
            })
            .unwrap();

        // Then draw the arg input popup on top.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_arg_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the background character cells inside the popup area should be empty
        // (cleared by Clear widget), not "X".
        let buffer = terminal.backend().buffer().clone();
        let popup_area = compute_arg_input_popup_rect(area, 1);

        // Check a few interior cells (not border) for cleared content.
        let inner_center = (
            popup_area.x + popup_area.width / 2,
            popup_area.y + popup_area.height / 2,
        );
        if let Some(cell) = buffer.cell(inner_center) {
            assert_ne!(
                cell.symbol(),
                "X",
                "background should be cleared inside popup"
            );
        }
    }

    #[rstest::rstest]
    fn arg_input_popup_multiline_command_splits_on_and_and() {
        // Given a state with a command containing &&.
        let state = make_state_with_args("test", Some("echo hello && echo world"), "", 0);
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_arg_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the popup has multiple command lines.
        let buffer = terminal.backend().buffer().clone();
        let input_args: Vec<String> = split_preserving_quotes("");
        let tmpl = CommandTemplate::parse("echo hello && echo world");
        let lines = tmpl.display_line_segments(&input_args);
        let content_rows = (lines.len() as u16) + 2;
        let popup_area = compute_arg_input_popup_rect(area, content_rows);
        let inner_x = popup_area.x + 1;

        // First line: "echo hello \"
        let line1_y = popup_area.y + 1;
        if line1_y < buffer.area().height {
            let row_text: String = (inner_x..inner_x + 20)
                .filter_map(|x| buffer.cell((x, line1_y)).map(|c| c.symbol().to_owned()))
                .collect();
            assert!(
                row_text.contains("echo hello"),
                "first line should contain 'echo hello', got: {row_text}"
            );
        }

        // Second line: "  && echo world"
        let line2_y = popup_area.y + 2;
        if line2_y < buffer.area().height {
            let row_text: String = (inner_x..inner_x + 20)
                .filter_map(|x| buffer.cell((x, line2_y)).map(|c| c.symbol().to_owned()))
                .collect();
            assert!(
                row_text.contains("echo world"),
                "second line should contain 'echo world', got: {row_text}"
            );
        }
    }

    #[rstest::rstest]
    fn arg_input_popup_param_segments_use_accent_action_color() {
        // Given a state with named params and typed input.
        let state = make_state_with_args("test", Some("mkdir <branch>"), "my-feature", 10);
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_arg_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the substituted value "my-feature" has accent_action color.
        let buffer = terminal.backend().buffer().clone();
        let theme = crate::feat::theme::default_theme();
        let expected_color = theme.accent_action;

        let input_args: Vec<String> = split_preserving_quotes("my-feature");
        let tmpl = CommandTemplate::parse("mkdir <branch>");
        let lines = tmpl.display_line_segments(&input_args);
        let content_rows = (lines.len() as u16) + 2;
        let popup_area = compute_arg_input_popup_rect(area, content_rows);
        let inner_x = popup_area.x + 1;
        let line1_y = popup_area.y + 1;

        // Find "my-feature" in the rendered output and check its color.
        if line1_y < buffer.area().height {
            // Scan to find "my-feature" in the rendered output.
            let mut found_colored = false;
            for x in inner_x..inner_x + 40 {
                if let Some(cell) = buffer.cell((x, line1_y))
                    && cell.symbol() == "m"
                    && cell.fg == expected_color
                {
                    found_colored = true;
                    break;
                }
            }
            assert!(
                found_colored,
                "param value 'my-feature' should use accent_action color"
            );
        }
    }

    #[rstest::rstest]
    fn arg_input_popup_last_line_no_trailing_backslash() {
        // Given a multi-segment command.
        let state = make_state_with_args("test", Some("echo hello && echo world"), "", 0);
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_arg_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the last command line does not end with \.
        let buffer = terminal.backend().buffer().clone();
        let input_args: Vec<String> = split_preserving_quotes("");
        let tmpl = CommandTemplate::parse("echo hello && echo world");
        let lines = tmpl.display_line_segments(&input_args);
        let content_rows = (lines.len() as u16) + 2;
        let popup_area = compute_arg_input_popup_rect(area, content_rows);
        let inner_x = popup_area.x + 1;

        // Last command line (second line, index 1).
        let last_cmd_y = popup_area.y + 2;
        if last_cmd_y < buffer.area().height {
            // Collect the line text.
            let row_text: String = (inner_x..inner_x + 40)
                .filter_map(|x| buffer.cell((x, last_cmd_y)).map(|c| c.symbol().to_owned()))
                .collect();
            assert!(
                !row_text.ends_with('\\'),
                "last command line should not end with backslash, got: {row_text}"
            );
        }
    }

    #[rstest::rstest]
    fn arg_input_popup_unfilled_param_shows_placeholder_in_color() {
        // Given a command with two params but no user input.
        let state = make_state_with_args("test", Some("mkdir <branch> && cd <target>"), "", 0);
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_arg_input(frame, area, &ctx);
            })
            .unwrap();

        // Then unfilled placeholders are shown in accent_action color.
        let buffer = terminal.backend().buffer().clone();
        let theme = crate::feat::theme::default_theme();
        let expected_color = theme.accent_action;

        let input_args: Vec<String> = split_preserving_quotes("");
        let tmpl = CommandTemplate::parse("mkdir <branch> && cd <target>");
        let lines = tmpl.display_line_segments(&input_args);
        let content_rows = (lines.len() as u16) + 2;
        let popup_area = compute_arg_input_popup_rect(area, content_rows);
        let inner_x = popup_area.x + 1;
        let line1_y = popup_area.y + 1;

        // Find "<branch>" on first line - it should be colored.
        if line1_y < buffer.area().height {
            let mut found_colored_bracket = false;
            for x in inner_x..inner_x + 40 {
                if let Some(cell) = buffer.cell((x, line1_y))
                    && cell.symbol() == "<"
                    && cell.fg == expected_color
                {
                    found_colored_bracket = true;
                    break;
                }
            }
            assert!(
                found_colored_bracket,
                "unfilled param placeholder should use accent_action color"
            );
        }
    }
}
