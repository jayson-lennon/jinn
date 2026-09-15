//! Task list preview popup — shows the tasks of the focused phase.
//!
//! Rendered as a bordered overlay to the **left** of the sidebar when the task
//! list section is focused. Its right edge touches the sidebar's left edge and
//! its top edge aligns with the top of the task list section (the row after the
//! persona and pins sections). The popup overlays everything (chat log, minimap,
//! border) with no layout changes — it is drawn after the base layers and uses a
//! `Clear` widget to punch a hole in whatever was rendered beneath it.
//!
//! The popup is read-only: it only displays the selected phase's tasks. Scrolling
//! is driven by `PageUp`/`PageDown`, which page by the measured inner height and
//! clamp to `[0, max_offset]`. Moving the phase cursor (`j`/`k`) or re-entering
//! the section resets the scroll to 0 (see [`TaskListSectionState`]).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::common::app_state::AppState;
use crate::common::app_state::FocusScope;
use crate::common::render_ctx::RenderCtx;
use crate::feat::theme::Theme;
use crate::feat::todo_list::{Phase, Task, TaskStatus};
use crate::feat::ui::sidebar::persona_section::persona_section_content_height;
use crate::feat::ui::sidebar::pins::pins_section_content_height;
use crate::feat::ui::sidebar::task_list_section::clamp_scroll;

/// Minimum popup width in columns (mirrors the session preview floor).
const MIN_POPUP_WIDTH: u16 = 30;
/// Bordered overlay height floor: borders (2) + at least 3 content rows.
const MIN_POPUP_HEIGHT: u16 = 5;
/// Indent for task descriptions (1 indicator + 1 space = 2 columns).
const TASK_INDENT: usize = 2;

/// Word-wraps a description string to the given available width.
///
/// Returns a single-element vec when `available_width` is too small to wrap.
fn wrap_description(text: &str, available_width: usize) -> Vec<String> {
    use std::borrow::Cow;
    use textwrap::Options;

    if available_width < 2 {
        return vec![text.to_owned()];
    }
    textwrap::wrap(text, Options::new(available_width))
        .into_iter()
        .map(Cow::into_owned)
        .collect()
}

/// Style for a task line based on its status.
fn task_style(theme: &Theme, status: TaskStatus) -> Style {
    match status {
        TaskStatus::Pending => Style::default().fg(theme.primary_text),
        TaskStatus::Completed | TaskStatus::Postponed => Style::default().fg(theme.muted_text),
        // Cancelled tasks are shown with strikethrough.
        TaskStatus::Cancelled => Style::default()
            .fg(theme.muted_text)
            .add_modifier(Modifier::CROSSED_OUT),
    }
}

/// Builds the wrapped, styled lines for a single task.
///
/// `available_width` is the popup's inner content width (between borders). The
/// status indicator sits on the first wrapped segment; continuation lines are
/// indented beneath the description.
fn task_lines(theme: &Theme, available_width: usize, task: &Task) -> Vec<Line<'static>> {
    let text_width = available_width.saturating_sub(TASK_INDENT);
    let indicator = task.status().indicator();
    let style = task_style(theme, task.status());
    let wrapped = wrap_description(task.description(), text_width);
    wrapped
        .iter()
        .enumerate()
        .map(|(i, segment)| {
            if i == 0 {
                Line::from(Span::styled(format!("{indicator} {segment}"), style))
            } else {
                Line::from(Span::styled(format!("  {segment}"), style))
            }
        })
        .collect()
}

/// Builds the wrapped, styled lines for a phase's tasks, or the `(no tasks)`
/// placeholder. `available_width` is the popup's inner content width.
fn phase_task_lines(theme: &Theme, available_width: usize, phase: &Phase) -> Vec<Line<'static>> {
    if phase.is_empty() {
        return vec![Line::from(Span::styled(
            "(no tasks)",
            Style::default().fg(theme.muted_text),
        ))];
    }
    phase
        .tasks()
        .iter()
        .flat_map(|task| task_lines(theme, available_width, task))
        .collect()
}

/// Computes the popup width: 60% of the frame area, with a floor of 30 columns,
/// capped to the available space left of the sidebar.
fn popup_width(frame_area: Rect, sidebar_x: u16) -> u16 {
    let w = (f32::from(frame_area.width) * 0.6).ceil() as u16;
    let space_left_of_sidebar = sidebar_x.saturating_sub(frame_area.x);
    w.max(MIN_POPUP_WIDTH).min(space_left_of_sidebar)
}

/// Resolves the phase the preview should show, or `None` if the popup is hidden.
///
/// The popup is hidden when the task list section is not focused, the task list
/// is empty, no phase is selected, or the selected index is out of range.
fn previewed_phase(state: &AppState) -> Option<&Phase> {
    if !matches!(state.frontend.scope(), FocusScope::SidebarTaskList) {
        return None;
    }
    let list = state.active_session().task_list();
    if list.is_empty() {
        return None;
    }
    let idx = state.frontend.task_list_section.selected_phase_index?;
    list.phases().get(idx)
}

/// Computes the task list preview popup rectangle.
///
/// The popup is anchored to the area left of the sidebar:
/// - **Right edge** touches the sidebar's left edge (`popup.x + width == sidebar.x`).
/// - **Top edge** aligns with the top of the task list section (persona + pins
///   heights below the frame/sidebar top).
/// - **Width** is 60% of the frame, floored at 30, capped to the space left of
///   the sidebar.
/// - **Height** is the natural content height (borders + content lines), capped
///   to the space from the top down to the status bar.
///
/// Returns `None` when there is no room to draw (width or available height below
/// the floor).
pub(crate) fn task_list_preview_popup_rect(
    state: &AppState,
    frame_area: Rect,
    sidebar_rect: Rect,
    content_line_count: usize,
) -> Option<Rect> {
    let popup_width = popup_width(frame_area, sidebar_rect.x);
    if popup_width < MIN_POPUP_WIDTH {
        return None;
    }

    // Top edge: frame top + persona height + pins height.
    let above = persona_section_content_height(state) + pins_section_content_height(state);
    let popup_y = frame_area.y.saturating_add(above);

    // Natural content height + borders (2).
    let desired_height = content_line_count.saturating_add(2) as u16;
    // Cap to the space from the top down to the status bar (2 rows).
    let available_height = frame_area.height.saturating_sub(above).saturating_sub(2);
    if available_height < MIN_POPUP_HEIGHT {
        return None;
    }
    let popup_height = desired_height.min(available_height).max(MIN_POPUP_HEIGHT);

    // Right edge touches the sidebar's left edge.
    let popup_x = sidebar_rect.x.saturating_sub(popup_width);

    Some(Rect::new(popup_x, popup_y, popup_width, popup_height))
}

/// Measures the preview popup and writes its geometry into state.
///
/// Run from the render pre-pass (which holds a write lock on state) so the
/// scroll intents can page by a full viewport and clamp without re-wrapping.
/// Writes [`TaskListSectionState::preview_content_line_count`] and
/// [`TaskListSectionState::preview_viewport_height`], then clamps the scroll.
///
/// When the popup is hidden (no focus / no selection / no room), the viewport
/// height is set to `0`, which makes both scroll intents no-op.
pub fn write_preview_geometry(state: &mut AppState, frame_area: Rect, sidebar_rect: Rect) {
    // Resolve phase + compute content line count without holding a mutable
    // borrow on state; we only need the (optional) count from this phase.
    // Wrap to the popup's inner content width (borders excluded), not the narrow
    // sidebar column, so descriptions fill the preview.
    let available_width = usize::from(popup_width(frame_area, sidebar_rect.x).saturating_sub(2));

    let line_count = previewed_phase(state).map_or(0usize, |phase| {
        let theme = &state.frontend.theme;
        phase_task_lines(theme, available_width, phase).len()
    });

    // Recompute the popup rect (reads persona/pins heights from state) while
    // no mutable borrow is outstanding.
    let rect = task_list_preview_popup_rect(state, frame_area, sidebar_rect, line_count);

    let section = &mut state.frontend.task_list_section;
    section.preview_content_line_count = line_count;
    section.preview_viewport_height = rect.map_or(0u16, |r| r.height.saturating_sub(2));
    clamp_scroll(section);
}

/// Renders the task list preview popup when the task list section is focused.
///
/// Reads the already-measured scroll offset from state, resolves the selected
/// phase, builds its wrapped task lines, recomputes the popup rect, then draws a
/// `Clear` + bordered `Block` + scrolled `Paragraph`. This never writes state —
/// all geometry measurement happens in [`write_preview_geometry`] during the
/// pre-render pass.
///
/// - `sidebar_rect`: the full sidebar column rect (right-edge alignment + width cap).
/// - `frame_area`: the total frame area.
pub fn render_task_list_preview_for_state(
    frame: &mut Frame<'_>,
    sidebar_rect: Rect,
    frame_area: Rect,
    ctx: &RenderCtx,
) {
    let state = ctx.state;
    let Some(phase) = previewed_phase(state) else {
        return;
    };

    let theme = &state.frontend.theme;
    let available_width = usize::from(popup_width(frame_area, sidebar_rect.x).saturating_sub(2));
    let content_lines = phase_task_lines(theme, available_width, phase);
    let line_count = content_lines.len();

    let Some(popup_rect) =
        task_list_preview_popup_rect(state, frame_area, sidebar_rect, line_count)
    else {
        return;
    };

    // Punch a hole, then draw the bordered block over it.
    frame.render_widget(Clear, popup_rect);

    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", phase.description()),
            Style::default().fg(theme.popup_title),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_unfocused));
    frame.render_widget(block, popup_rect);

    // Inner area (inside the borders).
    let inner_width = popup_rect.width.saturating_sub(2);
    let inner_height = popup_rect.height.saturating_sub(2);
    if inner_width == 0 || inner_height == 0 {
        return;
    }

    let scroll = state.frontend.task_list_section.preview_scroll;
    let content = Paragraph::new(content_lines).scroll((scroll as u16, 0));
    let inner_area = Rect {
        x: popup_rect.x + 1,
        y: popup_rect.y + 1,
        width: inner_width,
        height: inner_height,
    };
    frame.render_widget(content, inner_area);
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unreachable,
        clippy::string_slice,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use super::*;
    use crate::common::app_state::{AppState, FocusScope};
    use crate::feat::theme::default_theme;
    use crate::feat::todo_list::{TaskList, TaskPosition, TaskStatus};
    use ratatui::{Terminal, backend::TestBackend};

    fn frame_area() -> Rect {
        Rect::new(0, 0, 120, 40)
    }

    /// Sidebar occupies the rightmost 30 columns of a 120-wide frame.
    fn sidebar_rect() -> Rect {
        Rect::new(90, 0, 30, 40)
    }

    fn setup_two_phases_focused_on(phase_index: usize) -> AppState {
        let mut app = AppState::default_with_scope_focus();
        let session = app.session.active_session_mut();
        let p0 = session.task_list_mut().add_phase("Research");
        session
            .task_list_mut()
            .add_task(&p0, "Read docs", TaskPosition::End)
            .unwrap();
        let p1 = session.task_list_mut().add_phase("Build");
        session
            .task_list_mut()
            .add_task(&p1, "Write code", TaskPosition::End)
            .unwrap();
        app.frontend.scope_push(FocusScope::SidebarTaskList);
        app.frontend.task_list_section.selected_phase_index = Some(phase_index);
        app
    }

    #[rstest::rstest]
    #[test]
    fn popup_hidden_when_section_unfocused() {
        // Given a populated task list but no SidebarTaskList focus.
        let app = setup_two_phases_focused_on(0);
        app.frontend.scope_pop();

        // When rendering the popup.
        let text = render_popup_text(&app);

        // Then the popup draws nothing (no phase title, no tasks).
        assert!(
            !text.contains("Research"),
            "popup must not render when unfocused"
        );
        assert!(
            !text.contains("Read docs"),
            "popup content must not render when unfocused"
        );
    }

    #[rstest::rstest]
    #[test]
    fn rect_some_when_phase_selected_and_focused() {
        // Given a focused task list with a selected phase.
        let app = setup_two_phases_focused_on(0);

        // When computing the popup rect.
        let rect = task_list_preview_popup_rect(&app, frame_area(), sidebar_rect(), 5);

        // Then a rect is produced.
        assert!(rect.is_some());
    }

    #[rstest::rstest]
    #[test]
    fn rect_right_edge_meets_sidebar_left() {
        // Given a focused task list.
        let app = setup_two_phases_focused_on(0);
        let sidebar = sidebar_rect();

        // When computing the popup rect.
        let rect = task_list_preview_popup_rect(&app, frame_area(), sidebar, 5).unwrap();

        // Then the popup's right edge touches the sidebar's left edge.
        assert_eq!(rect.x + rect.width, sidebar.x);
    }

    #[rstest::rstest]
    #[test]
    fn rect_top_aligns_with_persona_plus_pins() {
        // Given a focused task list (persona=4, pins=0 by default).
        let app = setup_two_phases_focused_on(0);
        let expected_top = frame_area().y
            + persona_section_content_height(&app)
            + pins_section_content_height(&app);

        // When computing the popup rect.
        let rect = task_list_preview_popup_rect(&app, frame_area(), sidebar_rect(), 5).unwrap();

        // Then the popup top equals the task list section top.
        assert_eq!(rect.y, expected_top);
    }

    #[rstest::rstest]
    #[test]
    fn rect_width_capped_to_space_left_of_sidebar() {
        // Given a very narrow frame where 60% would exceed the space left of the sidebar.
        let app = setup_two_phases_focused_on(0);
        let sidebar = Rect::new(35, 0, 30, 40); // only 35 cols left of sidebar
        let frame = Rect::new(0, 0, 65, 40);

        // When computing the popup rect.
        let rect = task_list_preview_popup_rect(&app, frame, sidebar, 5).unwrap();

        // Then the width is capped to the available space (and starts at x=0).
        assert!(rect.width <= sidebar.x);
        assert_eq!(rect.x, 0);
    }

    #[rstest::rstest]
    #[test]
    fn popup_hidden_when_no_phase_selected() {
        // Given a focused task list with no selected phase.
        let mut app = setup_two_phases_focused_on(0);
        app.frontend.task_list_section.selected_phase_index = None;

        // When rendering the popup.
        let text = render_popup_text(&app);

        // Then the popup draws nothing.
        assert!(
            !text.contains("Research"),
            "popup must not render without a selected phase"
        );
        assert!(
            !text.contains("Read docs"),
            "popup content must not render without a selected phase"
        );
    }

    /// Renders the popup into a buffer and returns its trimmed text.
    fn render_popup_text(app: &AppState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|f| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(app, &slices, &overlay_views);
                render_task_list_preview_for_state(f, sidebar_rect(), frame_area(), &ctx);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol().to_owned())
            .collect()
    }

    #[rstest::rstest]
    #[test]
    fn popup_renders_only_selected_phase_tasks() {
        // Given focus on phase 0 ("Read docs"), with phase 1 holding "Write code".
        let app = setup_two_phases_focused_on(0);

        // When rendering the popup.
        let text = render_popup_text(&app);

        // Then phase 0's task text appears.
        assert!(
            text.contains("Read docs"),
            "selected phase task must appear"
        );
        // And phase 1's task text does not.
        assert!(
            !text.contains("Write code"),
            "other phase task must not appear"
        );
    }

    #[rstest::rstest]
    #[test]
    fn popup_phase_with_no_tasks_shows_placeholder() {
        // Given focus on an empty phase.
        let mut app = AppState::default_with_scope_focus();
        let session = app.session.active_session_mut();
        let _p0 = session.task_list_mut().add_phase("Empty Phase");
        app.frontend.scope_push(FocusScope::SidebarTaskList);
        app.frontend.task_list_section.selected_phase_index = Some(0);

        // When rendering the popup.
        let text = render_popup_text(&app);

        // Then the placeholder line appears.
        assert!(
            text.contains("(no tasks)"),
            "empty phase must show placeholder"
        );
    }

    #[rstest::rstest]
    #[test]
    fn write_geometry_clamps_stale_scroll_when_content_shrinks() {
        // Given a focused phase whose content is short, but scroll is stale from a
        // previously long phase (set before measurement).
        let mut app = setup_two_phases_focused_on(0);
        // Phase 0 has two short tasks; this scroll would be past the end.
        app.frontend.task_list_section.preview_content_line_count = 0;
        app.frontend.task_list_section.preview_viewport_height = 0;
        app.frontend.task_list_section.preview_scroll = 99;

        // When the pre-render pass measures geometry for the (short) phase.
        write_preview_geometry(&mut app, frame_area(), sidebar_rect());

        // Then the measured viewport height is set and the stale scroll is clamped
        // to the valid range (no longer 99).
        let section = &app.frontend.task_list_section;
        assert!(
            section.preview_viewport_height > 0,
            "viewport must be measured"
        );
        assert!(
            section.preview_scroll < 99,
            "stale scroll must be clamped after content shrink; got {}",
            section.preview_scroll
        );
    }

    #[rstest::rstest]
    #[test]
    fn long_task_wraps_to_popup_width_not_sidebar_width() {
        // Given a focused phase with one 60-char task description.
        // Popup inner width = 72 - 2 = 70, so text_width = 70 - 6 = 64 -> fits on 1 line.
        // Sidebar text_width = 30 - 6 = 24, which would wrap it to 3 lines (the bug).
        let mut app = AppState::default_with_scope_focus();
        let session = app.session.active_session_mut();
        let p0 = session.task_list_mut().add_phase("Research");
        let long_desc = "x".repeat(60);
        session
            .task_list_mut()
            .add_task(&p0, &long_desc, TaskPosition::End)
            .unwrap();
        app.frontend.scope_push(FocusScope::SidebarTaskList);
        app.frontend.task_list_section.selected_phase_index = Some(0);

        // When measuring preview geometry.
        write_preview_geometry(&mut app, frame_area(), sidebar_rect());

        // Then the task fits on a single line (popup width), not three (sidebar width).
        assert_eq!(
            app.frontend.task_list_section.preview_content_line_count, 1,
            "60-char task must wrap to popup width (1 line), not sidebar width (3 lines)"
        );
    }

    #[rstest::rstest]
    #[test]
    fn task_line_has_no_leading_margin() {
        // Given a phase with a pending task.
        let mut list = TaskList::new();
        let pid = list.add_phase("Research");
        list.add_task(&pid, "Read docs", TaskPosition::End).unwrap();
        let phase = &list.phases()[0];

        // When rendering task lines at a generous width.
        let lines = phase_task_lines(&default_theme(), 80, phase);

        // Then the first line starts with the status indicator, with no leading margin.
        let first = lines[0].spans.first().expect("task line has a span");
        assert!(
            first.content.starts_with(TaskStatus::Pending.indicator()),
            "task line must not carry a leftover sidebar indent; got: {first:?}"
        );
    }
}
