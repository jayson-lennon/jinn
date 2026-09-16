//! [`SessionsSection`] - the open sessions sidebar section.
//!
//! Implements [`SidebarSection`] for listing all sessions currently loaded
//! into memory. The active session (currently displayed) is highlighted with
//! a `▸` prefix. Navigating with j/k immediately switches the active session.

pub mod entry_line;
pub mod scroll_tag;
pub mod truncate;

#[cfg(test)]
mod entry_line_tests;

use std::time::Instant;

use crate::sections::section_trait::{SidebarSection, SidebarSectionId};
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::render_ctx::RenderCtx;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use throbber_widgets_tui::ThrobberState;

use super::{ANIMATION_INTERVAL, MAX_VISIBLE_SESSIONS};
use crate::sections::sessions::state::sorted_open_sessions;
use entry_line::assemble_entry_line;
use jinn_sidebar_msg::{ArchiveTreePrompt, TreePromptAction};
use scroll_tag::render_scroll_tag;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The open sessions sidebar section.
///
/// Renders all sessions loaded into memory with the active session highlighted.
#[derive(Debug)]
pub struct SessionsSection {
    /// Animation state for the working indicator.
    throbber_state: ThrobberState,
    /// Timestamp of the last animation frame advance.
    last_animation_step: Instant,
}

impl Default for SessionsSection {
    fn default() -> Self {
        Self {
            throbber_state: ThrobberState::default(),
            last_animation_step: Instant::now(),
        }
    }
}

impl SessionsSection {
    /// Creates a new sessions section.
    pub fn new() -> Self {
        Self::default()
    }

    /// Advances the animation frame if enough time has elapsed.
    fn maybe_advance_animation(&mut self) {
        if self.last_animation_step.elapsed() >= ANIMATION_INTERVAL {
            self.throbber_state.calc_next();
            self.last_animation_step = Instant::now();
        }
    }
}

impl SidebarSection for SessionsSection {
    fn id(&self) -> SidebarSectionId {
        jinn_sidebar_msg::SidebarSectionId::Sessions
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
        let state = ctx.state;
        let sessions = sorted_open_sessions(state);
        let theme = &state.frontend.theme;
        let sidebar_focused = state.frontend.is_sidebar();
        let section_focused = sidebar_focused
            && matches!(
                state.frontend.sidebar_section(),
                Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
            );

        let (selected_index, scroll_offset) = state.frontend.with_sections(
            |s| (s.sessions.selected_index, s.sessions.scroll_offset),
            || (None, 0),
        );

        let mut lines = Vec::new();

        if sessions.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                " No open sessions",
                Style::default().fg(theme.muted_text),
            )]));
        } else {
            let visible_count = MAX_VISIBLE_SESSIONS.min(sessions.len());
            let start = scroll_offset.min(sessions.len());
            let end = (start + visible_count).min(sessions.len());

            for (visual_i, entry) in sessions.get(start..end).unwrap_or(&[]).iter().enumerate() {
                let i = start + visual_i;
                let is_selected = section_focused && selected_index == Some(i);
                let max_title_len = area.width.saturating_sub(4) as usize;
                lines.push(assemble_entry_line(
                    entry,
                    is_selected,
                    max_title_len,
                    &self.throbber_state,
                    theme,
                ));
            }

            // Advance animation only when enough time has elapsed.
            self.maybe_advance_animation();

            // Scroll indicators.
            let lines_above = scroll_offset;
            let lines_below = sessions
                .len()
                .saturating_sub(scroll_offset)
                .saturating_sub(visible_count);

            if lines_above > 0 || lines_below > 0 {
                let indicator_style = Style::default().fg(Color::Black).bg(theme.age_fresh);

                if lines_above > 0 {
                    render_scroll_tag(frame, area, "\u{2191}", area.y, indicator_style);
                }

                if lines_below > 0 {
                    render_scroll_tag(
                        frame,
                        area,
                        "\u{2193}",
                        area.y + visible_count as u16 - 1,
                        indicator_style,
                    );
                }
            }
        }

        // Footer: ╰─── Sessions ───╯ (with highlighted S)
        let label = " Sessions ";
        let width = area.width as usize;
        let label_len = label.len();
        let dash_budget = width.saturating_sub(2).saturating_sub(label_len);
        let left_dashes = dash_budget / 2;
        let right_dashes = dash_budget - left_dashes;
        let before_s = format!("\u{2570}{}\u{0020}", "\u{2500}".repeat(left_dashes));
        let after_s = format!("essions {}\u{256F}", "\u{2500}".repeat(right_dashes));

        let footer_color = if section_focused {
            theme.focus_accent
        } else {
            theme.border_unfocused
        };

        lines.push(Line::from(vec![
            Span::styled(before_s, Style::default().fg(footer_color)),
            Span::styled("S".to_owned(), Style::default().fg(theme.accent_action)),
            Span::styled(after_s, Style::default().fg(footer_color)),
        ]));

        let widget = Paragraph::new(lines).block(Block::default().borders(Borders::NONE));
        frame.render_widget(widget, area);
    }

    fn content_height(&self, ctx: &RenderCtx) -> u16 {
        let state = ctx.state;
        let entry_count = sorted_open_sessions(state).len() as u16;
        let visible = entry_count.min(MAX_VISIBLE_SESSIONS as u16);
        // entries(N).max(1) + footer(1)
        visible.max(1) + 1 // max(1) for the no-sessions placeholder line
    }
}

/// Renders the close-session confirmation prompt as a late overlay.
///
/// Called AFTER the main column has rendered (from `jinn-tui`'s render pass),
/// so the banner may extend left over the input box. Anchored 1 row above the
/// sidebar cursor row and right-aligned to the frame's right edge — the same
/// geometry as the archive-tree prompt.
pub fn render_close_session_prompt_for_state(
    frame: &mut Frame<'_>,
    sidebar_rect: Rect,
    frame_area: Rect,
    ctx: &RenderCtx,
) {
    let state = ctx.state;
    if !state.frontend.close_session_prompt
        || !state.frontend.is_sidebar()
        || state.frontend.sidebar_section() != Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
    {
        return;
    }
    if state
        .frontend
        .with_sections(|s| s.sessions.selected_index.is_none(), || true)
    {
        return;
    }

    // Shared cursor-row math: see `render_sessions_cursor_y`.
    let prompt_y = render_sessions_cursor_y(sidebar_rect, state).saturating_sub(1);
    let text = " Press x again to teardown and archive 1 session ";
    render_right_aligned_banner(frame, frame_area, prompt_y, text, Color::Yellow);
}

/// Renders the archive-tree confirmation prompt as a late overlay.
///
/// Called AFTER the main column has rendered (from `jinn-tui`'s render pass,
/// right after the session preview), so the banner may extend left over the
/// input box. Anchored 1 row above the sidebar cursor row and right-aligned to
/// the frame's right edge, spanning whatever width it needs — it is an
/// overlay, not a sidebar element. Yellow = armed confirm ("Press A/X again
/// to archive/teardown-and-archive N sessions"); red = blocked (a member of
/// the subtree is busy).
pub fn render_archive_tree_prompt_for_state(
    frame: &mut Frame<'_>,
    sidebar_rect: Rect,
    frame_area: Rect,
    ctx: &RenderCtx,
) {
    let state = ctx.state;
    let Some(prompt) = &state.frontend.archive_tree_prompt else {
        return;
    };
    if !state.frontend.is_sidebar()
        || state.frontend.sidebar_section() != Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
    {
        return;
    }
    if state
        .frontend
        .with_sections(|s| s.sessions.selected_index.is_none(), || true)
    {
        return;
    }

    // Shared cursor-row math: see `render_sessions_cursor_y`.
    let prompt_y = render_sessions_cursor_y(sidebar_rect, state).saturating_sub(1);

    let (text, bg) = match prompt {
        ArchiveTreePrompt::Confirm { count, action } => {
            let (key, verb) = match action {
                TreePromptAction::Archive => ("A", "archive"),
                TreePromptAction::TeardownAndArchive => ("X", "teardown and archive"),
            };
            (
                format!(
                    " Press {key} again to {verb} {count} session{} ",
                    if *count == 1 { "" } else { "s" }
                ),
                Color::Yellow,
            )
        }
        ArchiveTreePrompt::Busy => (
            " Cannot archive tree while a session is busy ".to_owned(),
            Color::Red,
        ),
    };

    render_right_aligned_banner(frame, frame_area, prompt_y, &text, bg);
}

/// Computes the sidebar sessions cursor row inside the sidebar rect.
///
/// The sessions section is the last, bottom-anchored section of the sidebar;
/// this resolves the cursor's absolute frame row through the same
/// bottom-anchoring the `Sidebar` container applies when laying out sections.
fn render_sessions_cursor_y(sidebar_rect: Rect, state: &AppState) -> u16 {
    let sessions_height = {
        let entry_count = sorted_open_sessions(state).len() as u16;
        entry_count.min(MAX_VISIBLE_SESSIONS as u16).max(1) + 1
    };
    let sessions_top_y = sidebar_rect.y + sidebar_rect.height.saturating_sub(sessions_height);
    let scroll_offset = state
        .frontend
        .with_sections(|s| s.sessions.scroll_offset, || 0);
    let visual_row = state
        .frontend
        .with_sections(|s| s.sessions.selected_index, || None)
        .unwrap_or(0)
        .saturating_sub(scroll_offset) as u16;
    sessions_top_y + visual_row
}

/// Renders a one-row banner right-aligned to the frame's right edge.
///
/// Extends left over the main column as far as the banner needs. Clips to the
/// frame (grapheme-aware, never char-indexed) only if the banner could not fit
/// at all.
fn render_right_aligned_banner(
    frame: &mut Frame<'_>,
    frame_area: Rect,
    prompt_y: u16,
    text: &str,
    bg: Color,
) {
    // Right-align to the frame's right edge; extend left over the main column
    // as far as the banner needs. Clip to the frame (grapheme-aware, never
    // char-indexed) only if the banner could not fit at all.
    let (text, prompt_x) = {
        let text_width = text.width() as u16;
        if text_width > frame_area.width {
            let total = text.graphemes(true).count();
            let cropped: String = {
                let keep = frame_area.width as usize;
                text.graphemes(true)
                    .skip(total.saturating_sub(keep))
                    .collect()
            };
            let cropped_width = cropped.width() as u16;
            let x = frame_area.x + frame_area.width.saturating_sub(cropped_width);
            (cropped, x)
        } else {
            let x = frame_area.x + frame_area.width - text_width;
            (text.to_owned(), x)
        }
    };
    let text_width = text.width() as u16;
    let widget = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(Color::Black).bg(bg),
    )));
    frame.render_widget(
        widget,
        Rect {
            x: prompt_x,
            y: prompt_y,
            width: text_width,
            height: 1,
        },
    );
}
