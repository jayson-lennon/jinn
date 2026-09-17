//! [`TaskListSection`] - the task list sidebar section.
//!
//! Interactive section that displays the phased task list for the active session.
//! Phases are collapsed by default; the focused phase expands to show its tasks.
//! The section is hidden when the task list is empty.

pub mod preview;

use std::borrow::Cow;

use crate::sections::section_trait::{
    EnterFrom, SectionNavResult, SidebarIntent, SidebarSection, SidebarSectionId,
};
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::render_ctx::RenderCtx;
use jinn_domain::feat::theme::Theme;
use jinn_tools_msg::{Phase, PhaseId, TaskList};
use jinn_domain::protocol::IntentResult;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use textwrap::Options;

/// The task list sidebar section.
///
/// Renders phases and tasks from the active session's task list.
/// Phases are collapsed when unfocused; the selected phase expands when focused.
#[derive(Debug)]
pub struct TaskListSection;
pub use jinn_sidebar_msg::TaskListSectionState;

/// Navigate within the task list section.
///
/// Moves the phase cursor up/down. Returns `Exhausted` at boundaries.
pub fn navigate(intent: &SidebarIntent, state: &mut AppState) -> SectionNavResult {
    let Some(index) = state
        .frontend
        .with_sections(|s| s.task_list.selected_phase_index, || None)
    else {
        return SectionNavResult::Exhausted;
    };
    let phase_count = state.active_session().task_list().phases().len();
    if phase_count == 0 {
        return SectionNavResult::Exhausted;
    }
    match intent {
        SidebarIntent::MoveDown => {
            if index + 1 < phase_count {
                state.frontend.update_sections(|s| {
                    s.task_list.selected_phase_index = Some(index + 1);
                    s.task_list.preview_scroll = 0;
                });
                SectionNavResult::Moved
            } else {
                SectionNavResult::Exhausted
            }
        }
        SidebarIntent::MoveUp => {
            if index > 0 {
                state.frontend.update_sections(|s| {
                    s.task_list.selected_phase_index = Some(index - 1);
                    s.task_list.preview_scroll = 0;
                });
                SectionNavResult::Moved
            } else {
                SectionNavResult::Exhausted
            }
        }
        SidebarIntent::Action(_) => SectionNavResult::Exhausted,
    }
}

/// Place the cursor on this section.
///
/// Sets the selected phase index based on entry direction.
pub fn receive_cursor(state: &mut AppState, enter_from: EnterFrom) {
    let phase_count = state.active_session().task_list().phases().len();
    if phase_count == 0 {
        return;
    }
    state.frontend.update_sections(|s| {
        s.task_list.selected_phase_index = Some(match enter_from {
            EnterFrom::Top => 0,
            EnterFrom::Bottom => phase_count - 1,
        });
        s.task_list.preview_scroll = 0;
    });
}

/// Scrolls the preview popup one viewport toward the oldest task line.
///
/// Pages `preview_scroll` up by `preview_viewport_height` and clamps to
/// `[0, max_offset]`. No-op when the viewport height has not yet been measured
/// (e.g. before the first render).
pub fn handle_preview_scroll_up(state: &mut AppState) -> IntentResult {
    let viewport = state
        .frontend
        .with_sections(|s| s.task_list.preview_viewport_height, || 0);
    if viewport == 0 {
        return IntentResult::empty();
    }
    let page = usize::from(viewport);
    state.frontend.update_sections(|s| {
        s.task_list.preview_scroll = s.task_list.preview_scroll.saturating_sub(page);
        clamp_scroll(&mut s.task_list);
    });
    IntentResult::empty()
}

/// Scrolls the preview popup one viewport toward the newest task line.
///
/// Pages `preview_scroll` down by `preview_viewport_height` and clamps to
/// `[0, max_offset]`. No-op when the viewport height has not yet been measured.
pub fn handle_preview_scroll_down(state: &mut AppState) -> IntentResult {
    let viewport = state
        .frontend
        .with_sections(|s| s.task_list.preview_viewport_height, || 0);
    if viewport == 0 {
        return IntentResult::empty();
    }
    let page = usize::from(viewport);
    state.frontend.update_sections(|s| {
        s.task_list.preview_scroll = s.task_list.preview_scroll.saturating_add(page);
        clamp_scroll(&mut s.task_list);
    });
    IntentResult::empty()
}

/// Clamps `preview_scroll` to the valid range for the current content.
///
/// Called defensively by the render path and by both scroll handlers so that
/// content shrinkage (e.g. a phase switch to a shorter phase) can never leave
/// `preview_scroll` pointing past the end.
pub(crate) fn clamp_scroll(section: &mut TaskListSectionState) {
    let max_offset = section
        .preview_content_line_count
        .saturating_sub(usize::from(section.preview_viewport_height));
    section.preview_scroll = section.preview_scroll.min(max_offset);
}

impl SidebarSection for TaskListSection {
    fn id(&self) -> SidebarSectionId {
        jinn_sidebar_msg::SidebarSectionId::TaskList
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
        let state = ctx.state;
        let list = state.active_session().task_list();
        if list.is_empty() {
            return;
        }

        let lines = build_render_lines(list, state);
        let widget = Paragraph::new(lines);
        frame.render_widget(widget, area);
    }

    fn content_height(&self, ctx: &RenderCtx) -> u16 {
        let state = ctx.state;
        let list = state.active_session().task_list();
        if list.is_empty() {
            return 0;
        }
        compute_height(list, state)
    }
}

const PHASE_INDENT: usize = 2;

/// Display width of the phase collapse/expand indicator (`“▸ ”` / `“◂ ”` = 2 columns).
///
/// This is the **display-column** width of the indicator, deliberately not its byte
/// length (`“▸ ”` is 4 UTF-8 bytes). `textwrap` measures by display columns, so
/// this constant must be used everywhere the wrap width is computed for phase headers,
/// keeping `build_render_lines` and `compute_height` in lockstep.
const PHASE_INDICATOR_WIDTH: usize = 2;

/// Word-wraps a description string to the given available width.
///
/// Returns a single-element vec when `available_width` is too small to wrap.
fn wrap_description(text: &str, available_width: usize) -> Vec<String> {
    if available_width < 2 {
        return vec![text.to_owned()];
    }
    textwrap::wrap(text, Options::new(available_width))
        .into_iter()
        .map(Cow::into_owned)
        .collect()
}

/// Returns the expanded phase index if the sidebar is focused on the task list section.
fn expanded_phase_index(state: &AppState) -> Option<usize> {
    if state.frontend.sidebar_section() == Some(jinn_sidebar_msg::SidebarSectionId::TaskList) {
        state
            .frontend
            .with_sections(|s| s.task_list.selected_phase_index, || None)
    } else {
        None
    }
}

/// Shared visual capabilities for rendering the task list.
///
/// Bundles the theme, available sidebar width, and the focused phase index so that
/// every visual part derives its wrap widths from one place. Centralizing the width
/// math here keeps `build_render_lines` and `compute_height` in lockstep — the two
/// previously diverged on the indicator's display vs byte width (see
/// `collapsed_render_line_count_matches_computed_height_when_wrapping`).
struct TaskListView<'a> {
    theme: &'a Theme,
    sidebar_width: usize,
    expanded: Option<usize>,
}

impl<'a> TaskListView<'a> {
    /// Builds the view from application state.
    fn from_state(state: &'a AppState) -> Self {
        Self {
            theme: &state.frontend.theme,
            sidebar_width: state.frontend.sidebar_width.into(),
            expanded: expanded_phase_index(state),
        }
    }

    /// Available text width for a phase header's wrapped description.
    ///
    /// Accounts for the phase indent and the collapse/expand indicator's display width.
    fn phase_text_width(&self) -> usize {
        self.sidebar_width
            .saturating_sub(PHASE_INDENT + PHASE_INDICATOR_WIDTH)
    }

    /// True when the phase at `index` is the focused, expanded one.
    fn is_expanded(&self, index: usize) -> bool {
        self.expanded == Some(index)
    }

    /// Phase header foreground color: streaming for the active phase, muted when no
    /// work remains, otherwise the primary text color.
    fn phase_header_color(&self, phase: &Phase, active_phase_id: Option<&PhaseId>) -> Color {
        if active_phase_id == Some(phase.id()) {
            self.theme.streaming
        } else if phase.has_pending_work() {
            self.theme.primary_text
        } else {
            self.theme.muted_text
        }
    }

    /// Style for a phase header line, with reversed colors when the phase is selected.
    fn phase_header_style(
        &self,
        phase: &Phase,
        index: usize,
        active_phase_id: Option<&PhaseId>,
    ) -> Style {
        let mut style = Style::default()
            .fg(self.phase_header_color(phase, active_phase_id))
            .add_modifier(Modifier::BOLD);
        if self.is_expanded(index) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        style
    }

    /// The title line at the top of the section.
    fn header_line(&self, phase_count: usize) -> Line<'static> {
        Line::from(vec![Span::styled(
            format!(
                " Task List \u{2014} {} phase{}",
                phase_count,
                if phase_count == 1 { "" } else { "s" }
            ),
            Style::default()
                .fg(self.theme.primary_text)
                .add_modifier(Modifier::BOLD),
        )])
    }

    /// Phase header lines: the collapse/expand indicator on the first wrapped segment,
    /// continuation lines indented beneath the description.
    fn phase_header_lines(
        &self,
        phase: &Phase,
        index: usize,
        active_phase_id: Option<&PhaseId>,
    ) -> Vec<Line<'static>> {
        let indicator = if self.is_expanded(index) {
            "\u{25C2} " // ◂ preview is to the left
        } else {
            "\u{25B8} " // ▸ collapsed
        };
        let style = self.phase_header_style(phase, index, active_phase_id);
        let wrapped = wrap_description(phase.description(), self.phase_text_width());
        wrapped
            .iter()
            .enumerate()
            .map(|(i, segment)| {
                let prefix = if i == 0 {
                    format!("  {indicator}{segment}")
                } else {
                    format!("    {}{segment}", " ".repeat(PHASE_INDICATOR_WIDTH))
                };
                Line::from(Span::styled(prefix, style))
            })
            .collect()
    }

    /// Number of rows a phase contributes at `index`: just its wrapped header.
    ///
    /// Task rows live in the preview popup, never inline, so the inline height
    /// is independent of expansion. Used by `compute_height` to stay in lockstep
    /// with `build_render_lines`.
    fn phase_height(&self, phase: &Phase, _index: usize) -> usize {
        wrap_description(phase.description(), self.phase_text_width()).len()
    }
}

/// Builds the render lines for a task list.
///
/// Renders only phase headers — the focused phase shows a `◂` indicator pointing
/// at the preview popup to its left, and gets `REVERSED` styling. Task contents are
/// shown in the preview popup (see `preview.rs`), never inline.
fn build_render_lines(list: &TaskList, state: &AppState) -> Vec<Line<'static>> {
    let view = TaskListView::from_state(state);
    let active_phase_id = list.active_phase().map(Phase::id);

    let mut lines = Vec::new();

    // Header + blank separator.
    lines.push(view.header_line(list.phases().len()));
    lines.push(Line::from(""));

    // One phase per iteration: the header only (tasks live in the preview popup).
    for (phase_idx, phase) in list.phases().iter().enumerate() {
        lines.extend(view.phase_header_lines(phase, phase_idx, active_phase_id));
    }

    // Trailing gap — matches Persona/Pins/McpServers, each of which ends with a
    // blank line separating it from the section below (here: McpServers).
    lines.push(Line::from(""));

    lines
}

/// Computes the content height for a non-empty task list.
///
/// Mirrors [`build_render_lines`]: the same header rows, the same per-phase wrap
/// width, and the same expansion rule. Keeping the two in lockstep via
/// [`TaskListView`] prevents the render/height divergence that previously clipped
/// phase headers (see `collapsed_render_line_count_matches_computed_height_when_wrapping`).
fn compute_height(list: &TaskList, state: &AppState) -> u16 {
    let view = TaskListView::from_state(state);

    // Header + blank separator.
    let mut height: usize = 2;

    for (phase_idx, phase) in list.phases().iter().enumerate() {
        height += view.phase_height(phase, phase_idx);
    }

    // Trailing gap (mirrors build_render_lines).
    height += 1;

    height as u16
}

#[cfg(test)]
mod tests {
    #![allow(
        unused_mut,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unreachable,
        clippy::string_slice,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use super::*;
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::common::render_ctx::RenderCtx;
    use jinn_tools_msg::{PhaseInput, TaskStatus};

    fn setup_with_tasks() -> AppState {
        let mut app = AppState::default_with_scope_focus();
        let session = app.session.active_session_mut();
        session.task_list_mut().set_from_inputs(&[
            PhaseInput {
                description: "Research".to_owned(),
                tasks: vec![
                    ("Read docs".to_owned(), TaskStatus::Pending),
                    ("Call API".to_owned(), TaskStatus::Pending),
                ],
            },
            PhaseInput {
                description: "Build".to_owned(),
                tasks: vec![("Write code".to_owned(), TaskStatus::Pending)],
            },
        ]);
        app
    }

    /// Helper: set up focus on a specific phase so it expands.
    fn setup_focused_on_phase(app: &mut AppState, phase_index: usize) {
        app.frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::TaskList.focus_scope());
        app.frontend
            .update_sections(|s| s.task_list.selected_phase_index = Some(phase_index));
    }

    /// Extract all text content from render lines.
    fn extract_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.to_string())
            .collect()
    }

    #[rstest::rstest]
    #[test]
    fn content_height_is_zero_when_empty() {
        let app = AppState::default_with_scope_focus();
        let section = TaskListSection;
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
        assert_eq!(
            section.content_height(&RenderCtx::new(&app, &slices, &overlay_views)),
            0
        );
    }

    #[rstest::rstest]
    #[test]
    fn content_height_is_nonzero_when_has_phases() {
        let app = setup_with_tasks();
        let section = TaskListSection;
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
        let height = section.content_height(&RenderCtx::new(&app, &slices, &overlay_views));
        assert!(height > 0, "expected non-zero height, got {height}");
    }

    #[rstest::rstest]
    #[test]
    fn render_ends_with_trailing_gap_line() {
        // Given a non-empty task list.
        let app = setup_with_tasks();
        let list = app.session.active_session().task_list().clone();

        // When building render lines.
        let lines = build_render_lines(&list, &app);

        // Then the last line is the section's trailing gap (empty).
        let final_line = lines.last().expect("at least one line");
        let text: String = final_line
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(
            text.trim().is_empty(),
            "trailing gap line should be blank; got: {text:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn content_height_matches_rendered_line_count() {
        // Given a non-empty task list.
        let app = setup_with_tasks();
        let list = app.session.active_session().task_list().clone();
        let section = TaskListSection;

        // When computing the height and the render line count.
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
        let height = section.content_height(&RenderCtx::new(&app, &slices, &overlay_views));
        let line_count = build_render_lines(&list, &app).len() as u16;

        // Then they agree (render/height lockstep).
        assert_eq!(
            height, line_count,
            "content_height must match build_render_lines"
        );
    }

    #[rstest::rstest]
    #[test]
    fn navigate_returns_exhausted_without_selection() {
        let mut app = AppState::default_with_scope_focus();
        let result = navigate(&SidebarIntent::MoveDown, &mut app);
        assert_eq!(result, SectionNavResult::Exhausted);
    }

    #[rstest::rstest]
    #[test]
    fn navigate_moves_down_within_bounds() {
        let mut app = setup_with_tasks();
        setup_focused_on_phase(&mut app, 0);
        let result = navigate(&SidebarIntent::MoveDown, &mut app);
        assert_eq!(result, SectionNavResult::Moved);
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.selected_phase_index, || None),
            Some(1)
        );
    }

    #[rstest::rstest]
    #[test]
    fn navigate_moves_up_within_bounds() {
        let mut app = setup_with_tasks();
        setup_focused_on_phase(&mut app, 1);
        let result = navigate(&SidebarIntent::MoveUp, &mut app);
        assert_eq!(result, SectionNavResult::Moved);
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.selected_phase_index, || None),
            Some(0)
        );
    }

    #[rstest::rstest]
    #[test]
    fn navigate_exhausted_at_bottom() {
        let mut app = setup_with_tasks();
        setup_focused_on_phase(&mut app, 1); // last phase
        let result = navigate(&SidebarIntent::MoveDown, &mut app);
        assert_eq!(result, SectionNavResult::Exhausted);
    }

    #[rstest::rstest]
    #[test]
    fn navigate_exhausted_at_top() {
        let mut app = setup_with_tasks();
        setup_focused_on_phase(&mut app, 0); // first phase
        let result = navigate(&SidebarIntent::MoveUp, &mut app);
        assert_eq!(result, SectionNavResult::Exhausted);
    }

    #[rstest::rstest]
    #[test]
    fn receive_cursor_sets_first_phase_from_top() {
        let mut app = setup_with_tasks();
        receive_cursor(&mut app, EnterFrom::Top);
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.selected_phase_index, || None),
            Some(0)
        );
    }

    #[rstest::rstest]
    #[test]
    fn receive_cursor_sets_last_phase_from_bottom() {
        let mut app = setup_with_tasks();
        receive_cursor(&mut app, EnterFrom::Bottom);
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.selected_phase_index, || None),
            Some(1)
        );
    }

    #[rstest::rstest]
    #[test]
    fn receive_cursor_no_panic_on_empty_list() {
        let mut app = AppState::default_with_scope_focus();
        receive_cursor(&mut app, EnterFrom::Top);
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.selected_phase_index, || None),
            None
        );
    }

    #[rstest::rstest]
    #[test]
    fn id_returns_task_list() {
        let section = TaskListSection;
        assert_eq!(section.id(), jinn_sidebar_msg::SidebarSectionId::TaskList);
    }

    #[rstest::rstest]
    #[test]
    fn collapsed_rendering_shows_no_tasks() {
        // When unfocused, task descriptions should NOT appear.
        let app = setup_with_tasks();
        let list = app.session.active_session().task_list().clone();
        let lines = build_render_lines(&list, &app);
        let combined = extract_text(&lines);
        assert!(
            combined.contains("Research"),
            "should contain phase: Research"
        );
        assert!(combined.contains("Build"), "should contain phase: Build");
        assert!(
            !combined.contains("Read docs"),
            "collapsed should NOT contain task: Read docs"
        );
        assert!(
            !combined.contains("Write code"),
            "collapsed should NOT contain task: Write code"
        );
    }

    #[rstest::rstest]
    #[test]
    fn collapsed_shows_collapse_indicator() {
        let app = setup_with_tasks();
        let list = app.session.active_session().task_list().clone();
        let lines = build_render_lines(&list, &app);
        let combined = extract_text(&lines);
        assert!(
            combined.contains('\u{25B8}'),
            "collapsed phases should show \u{25B8} indicator"
        );
    }

    #[rstest::rstest]
    #[test]
    fn no_blank_lines_between_phases() {
        let app = setup_with_tasks();
        let list = app.session.active_session().task_list().clone();
        let lines = build_render_lines(&list, &app);
        // The last line is the section's trailing gap (mirrors Persona/Pins/McpServers),
        // and line index 1 is the header separator blank. Both are expected; every
        // line *between* phases should have content.
        let trailing_index = lines.len().saturating_sub(1);
        for (i, line) in lines.iter().enumerate() {
            if i == 1 || i == trailing_index {
                continue; // header separator / trailing gap are expected
            }
            let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
            assert!(
                !text.trim().is_empty(),
                "line {i} should not be blank between phases: {text:?}"
            );
        }
    }

    #[rstest::rstest]
    #[test]
    fn selected_phase_header_has_reversed_modifier() {
        let mut app = setup_with_tasks();
        setup_focused_on_phase(&mut app, 0);
        let list = app.session.active_session().task_list().clone();
        let lines = build_render_lines(&list, &app);
        // Find a line containing the first phase name.
        let has_reversed = lines.iter().any(|line| {
            let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
            text.contains("Research")
                && line
                    .spans
                    .iter()
                    .any(|s| s.style.add_modifier.contains(Modifier::REVERSED))
        });
        assert!(
            has_reversed,
            "selected phase header should have REVERSED modifier for cursor highlight"
        );
    }

    /// The inline section is always collapsed; the focused phase shows a
    /// left-pointing arrow (U+25C2 `◂`) signalling that its preview popup is
    /// drawn to the left of the sidebar. Collapsed phases show U+25B8 `▸`.
    #[rstest::rstest]
    #[test]
    fn focused_phase_shows_left_arrow_indicator() {
        let mut app = setup_with_tasks();
        setup_focused_on_phase(&mut app, 0);
        let list = app.session.active_session().task_list().clone();
        let combined = extract_text(&build_render_lines(&list, &app));

        assert!(
            combined.contains('\u{25C2}'),
            "focused phase should show left-arrow indicator \u{25C2}"
        );
        assert!(
            combined.contains('\u{25B8}'),
            "unfocused phases should show right-arrow indicator \u{25B8}"
        );
    }

    /// Regression: the last phase header must appear in the rendered output while
    /// collapsed. Previously the byte-length-vs-display-width divergence made
    /// `build_render_lines` emit more lines than `compute_height` reserved, and
    /// ratatui's `Paragraph` clipped the overflow — dropping the last phase header
    /// until focus shifted it out of the clipped region.
    #[rstest::rstest]
    #[test]
    fn collapsed_render_includes_last_phase_header() {
        // Given a sidebar of default width (30) with three phases, the last one's
        // description chosen to wrap at the buggy render width (24) but not the
        // correct one (26).
        let mut app = AppState::default_with_scope_focus();
        app.frontend.sidebar_width = 30;
        let session = app.session.active_session_mut();
        session
            .task_list_mut()
            .add_phase("initialize the loader tail");
        session
            .task_list_mut()
            .add_phase("validate the payload tail");
        session
            .task_list_mut()
            .add_phase("register the handler tail");
        let list = app.session.active_session().task_list().clone();

        // When rendering collapsed (no focus).
        let text = extract_text(&build_render_lines(&list, &app));

        // Then the last phase header is present in the painted output.
        assert!(
            text.contains("handler"),
            "last phase header must be rendered, got: {text}"
        );
    }

    /// Helper: find a line containing `phase_name` and return its first span's foreground color.
    fn phase_header_fg(lines: &[Line<'static>], phase_name: &str) -> Option<ratatui::style::Color> {
        lines
            .iter()
            .find(|line| {
                let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
                text.contains(phase_name)
            })
            .and_then(|line| line.spans.first().map(|s| s.style.fg))
            .flatten()
    }

    #[rstest::rstest]
    #[test]
    fn active_phase_header_uses_streaming_color() {
        // Given 3 phases: first has pending tasks (active), second has pending tasks, third all completed.
        let mut app = AppState::default_with_scope_focus();
        let session = app.session.active_session_mut();
        session.task_list_mut().set_from_inputs(&[
            PhaseInput {
                description: "Research".to_owned(),
                tasks: vec![("Read docs".to_owned(), TaskStatus::Pending)],
            },
            PhaseInput {
                description: "Build".to_owned(),
                tasks: vec![("Write code".to_owned(), TaskStatus::Pending)],
            },
            PhaseInput {
                description: "Test".to_owned(),
                tasks: vec![("Run tests".to_owned(), TaskStatus::Completed)],
            },
        ]);
        let list = session.task_list().clone();

        // When rendering (no focus).
        let lines = build_render_lines(&list, &app);

        // Then the active phase header (Research) uses streaming color.
        let fg = phase_header_fg(&lines, "Research").expect("Research phase header");
        assert_eq!(
            fg, app.frontend.theme.streaming,
            "active phase header should use streaming color"
        );
    }

    #[rstest::rstest]
    #[test]
    fn completed_phase_header_uses_muted_text_color() {
        // Given a phase with all tasks completed.
        let mut app = AppState::default_with_scope_focus();
        let session = app.session.active_session_mut();
        session.task_list_mut().set_from_inputs(&[
            PhaseInput {
                description: "Research".to_owned(),
                tasks: vec![("Read docs".to_owned(), TaskStatus::Completed)],
            },
            PhaseInput {
                description: "Build".to_owned(),
                tasks: vec![("Write code".to_owned(), TaskStatus::Pending)],
            },
        ]);
        let list = session.task_list().clone();

        // When rendering.
        let lines = build_render_lines(&list, &app);

        // Then the completed phase header (Research) uses muted_text color.
        let fg = phase_header_fg(&lines, "Research").expect("Research phase header");
        assert_eq!(
            fg, app.frontend.theme.muted_text,
            "completed phase header should use muted_text color"
        );
    }

    #[rstest::rstest]
    #[test]
    fn upcoming_phase_header_uses_primary_text_color() {
        // Given 2 phases: first has pending tasks (active), second has pending tasks (upcoming/blocked).
        let mut app = AppState::default_with_scope_focus();
        let session = app.session.active_session_mut();
        session.task_list_mut().set_from_inputs(&[
            PhaseInput {
                description: "Research".to_owned(),
                tasks: vec![("Read docs".to_owned(), TaskStatus::Pending)],
            },
            PhaseInput {
                description: "Build".to_owned(),
                tasks: vec![("Write code".to_owned(), TaskStatus::Pending)],
            },
        ]);
        let list = session.task_list().clone();

        // When rendering.
        let lines = build_render_lines(&list, &app);

        // Then the upcoming phase header (Build) uses primary_text color.
        let fg = phase_header_fg(&lines, "Build").expect("Build phase header");
        assert_eq!(
            fg, app.frontend.theme.primary_text,
            "upcoming phase header should use primary_text color"
        );
    }

    #[rstest::rstest]
    #[test]
    fn selected_active_phase_header_has_reversed_and_streaming_color() {
        // Given 2 phases with pending tasks, focused on first (active) phase.
        let mut app = AppState::default_with_scope_focus();
        let session = app.session.active_session_mut();
        session.task_list_mut().set_from_inputs(&[
            PhaseInput {
                description: "Research".to_owned(),
                tasks: vec![("Read docs".to_owned(), TaskStatus::Pending)],
            },
            PhaseInput {
                description: "Build".to_owned(),
                tasks: vec![("Write code".to_owned(), TaskStatus::Pending)],
            },
        ]);
        let list = session.task_list().clone();
        setup_focused_on_phase(&mut app, 0);

        // When rendering.
        let lines = build_render_lines(&list, &app);

        // Then the active phase header has both streaming color AND REVERSED.
        let has_both = lines.iter().any(|line| {
            let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
            text.contains("Research")
                && line.spans.iter().any(|s| {
                    s.style.fg == Some(app.frontend.theme.streaming)
                        && s.style.add_modifier.contains(Modifier::REVERSED)
                })
        });
        assert!(
            has_both,
            "selected active phase header should have streaming color AND REVERSED modifier"
        );
    }

    // ---- Phase 1: preview scroll / clamp / reset ----

    /// Sets viewport + content so `preview_scroll` can page and clamp.
    fn setup_preview(viewport: u16, content_lines: usize, scroll: usize) -> AppState {
        let mut app = AppState::default_with_scope_focus();
        app.frontend
            .update_sections(|s| s.task_list.preview_viewport_height = viewport);
        app.frontend
            .update_sections(|s| s.task_list.preview_content_line_count = content_lines);
        app.frontend
            .update_sections(|s| s.task_list.preview_scroll = scroll);
        app
    }

    #[rstest::rstest]
    #[test]
    fn preview_scroll_up_decreases_by_viewport() {
        // Given viewport 5 and scroll at 10.
        let mut app = setup_preview(5, 20, 10);

        // When scrolling up by a page.
        handle_preview_scroll_up(&mut app);

        // Then scroll decreases by the viewport height.
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.preview_scroll, || 0),
            5
        );
    }

    #[rstest::rstest]
    #[test]
    fn preview_scroll_down_increases_by_viewport() {
        // Given viewport 5 and scroll at 0.
        let mut app = setup_preview(5, 20, 0);

        // When scrolling down by a page.
        handle_preview_scroll_down(&mut app);

        // Then scroll increases by the viewport height.
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.preview_scroll, || 0),
            5
        );
    }

    #[rstest::rstest]
    #[test]
    fn preview_scroll_up_clamps_at_zero() {
        // Given viewport 5 with scroll 2 (less than one page).
        let mut app = setup_preview(5, 20, 2);

        // When scrolling up.
        handle_preview_scroll_up(&mut app);

        // Then scroll clamps to 0 rather than underflowing.
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.preview_scroll, || 0),
            0
        );
    }

    #[rstest::rstest]
    #[test]
    fn preview_scroll_down_clamps_at_max_offset() {
        // Given viewport 5 and content of 8 lines (max offset 3).
        let mut app = setup_preview(5, 8, 0);

        // When scrolling down past the end.
        handle_preview_scroll_down(&mut app);

        // Then scroll clamps to max_offset 3, not 5.
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.preview_scroll, || 0),
            3
        );
    }

    #[rstest::rstest]
    #[test]
    fn preview_scroll_up_noop_when_viewport_unmeasured() {
        // Given an unmeasured viewport (0, before first render).
        let mut app = setup_preview(0, 20, 7);

        // When scrolling up.
        handle_preview_scroll_up(&mut app);

        // Then scroll is unchanged.
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.preview_scroll, || 0),
            7
        );
    }

    #[rstest::rstest]
    #[test]
    fn preview_scroll_down_noop_when_viewport_unmeasured() {
        // Given an unmeasured viewport (0, before first render).
        let mut app = setup_preview(0, 20, 7);

        // When scrolling down.
        handle_preview_scroll_down(&mut app);

        // Then scroll is unchanged.
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.preview_scroll, || 0),
            7
        );
    }

    #[rstest::rstest]
    #[test]
    fn navigate_down_resets_preview_scroll() {
        // Given a task list with scroll at 7 and focus on phase 0.
        let mut app = setup_with_tasks();
        setup_focused_on_phase(&mut app, 0);
        app.frontend
            .update_sections(|s| s.task_list.preview_scroll = 7);

        // When moving down to the next phase.
        let result = navigate(&SidebarIntent::MoveDown, &mut app);

        // Then navigation moved and scroll reset to 0.
        assert_eq!(result, SectionNavResult::Moved);
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.preview_scroll, || 0),
            0
        );
    }

    #[rstest::rstest]
    #[test]
    fn navigate_up_resets_preview_scroll() {
        // Given a task list with scroll at 7 and focus on phase 1.
        let mut app = setup_with_tasks();
        setup_focused_on_phase(&mut app, 1);
        app.frontend
            .update_sections(|s| s.task_list.preview_scroll = 7);

        // When moving up to the previous phase.
        let result = navigate(&SidebarIntent::MoveUp, &mut app);

        // Then navigation moved and scroll reset to 0.
        assert_eq!(result, SectionNavResult::Moved);
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.preview_scroll, || 0),
            0
        );
    }

    #[rstest::rstest]
    #[test]
    fn receive_cursor_resets_preview_scroll() {
        // Given a task list with scroll at 7.
        let mut app = setup_with_tasks();
        app.frontend
            .update_sections(|s| s.task_list.preview_scroll = 7);

        // When re-entering the section from the top.
        receive_cursor(&mut app, EnterFrom::Top);

        // Then scroll reset to 0.
        assert_eq!(
            app.frontend
                .with_sections(|s| s.task_list.preview_scroll, || 0),
            0
        );
    }
}
