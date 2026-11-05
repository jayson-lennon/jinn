//! [`SessionsSection`] - the open sessions sidebar section.
//!
//! Implements [`SidebarSection`] for listing all sessions currently loaded
//! into memory. The active session (currently displayed) is highlighted with
//! a `▸` prefix. Navigating with j/k immediately switches the active session.

pub mod entry_line;
pub mod truncate;

#[cfg(test)]
mod entry_line_tests;

use std::ops::Range;
use std::time::Instant;

use crate::sections::section_trait::{SidebarSection, SidebarSectionId};
use crate::sections::sessions::state::{SessionEntry, SessionListKey, session_list_key};
use jinn_kernel::common::app_state::AppState;
use jinn_slices::DrawContext;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use throbber_widgets_tui::ThrobberState;

use crate::sections::sessions::state::sorted_open_sessions;
use entry_line::assemble_entry_line;
use jinn_sidebar_msg::{ArchiveTreePrompt, TreePromptAction};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Rows between a confirm banner and the cursor row it describes.
///
/// Two rows leaves a one-row gap, so the banner reads as a separate label
/// rather than colliding with the highlighted session row.
pub(crate) const BANNER_GAP: u16 = 2;

/// The open sessions sidebar section.
///
/// Renders all sessions loaded into memory with the active session highlighted.
#[derive(Debug)]
pub struct SessionsSection {
    /// Animation state for the working indicator.
    throbber_state: ThrobberState,
    /// Timestamp of the last animation frame advance.
    last_animation_step: Instant,
    /// The sessions tree, kept from the last frame it was built.
    cached_tree: Vec<SessionEntry>,
    /// The inputs `cached_tree` was built from.
    cached_key: Option<Vec<SessionListKey>>,
    /// How many times the tree has actually been rebuilt.
    rebuilds: u64,
}

impl Default for SessionsSection {
    fn default() -> Self {
        Self {
            throbber_state: ThrobberState::default(),
            last_animation_step: Instant::now(),
            cached_tree: Vec::new(),
            cached_key: None,
            rebuilds: 0,
        }
    }
}

impl SessionsSection {
    /// Creates a new sessions section.
    pub fn new() -> Self {
        Self::default()
    }

    /// The sessions tree, rebuilding it only if its inputs changed.
    ///
    /// Building the tree clones every session title and then every entry, so an
    /// unchanged frame reuses the previous result instead. Both `content_height`
    /// and `render` come through here, so they can never disagree on the count.
    fn sessions_tree(
        &mut self,
        state: &jinn_kernel::common::app_state::AppState,
    ) -> &[SessionEntry] {
        let key = session_list_key(state);
        if self.cached_key.as_ref() != Some(&key) {
            self.cached_tree = sorted_open_sessions(state);
            self.cached_key = Some(key);
            self.rebuilds += 1;
        }
        &self.cached_tree
    }

    /// Number of sessions in the currently cached tree.
    ///
    /// Exposed for tests that assert the height and the render agree.
    #[must_use]
    pub fn cached_session_count(&self) -> usize {
        self.cached_tree.len()
    }

    /// How many times the sessions tree has been rebuilt.
    ///
    /// Exposed for tests that assert the memo is actually skipping work.
    #[must_use]
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// Advances the animation frame if enough time has elapsed.
    fn maybe_advance_animation(&mut self) {
        if self.last_animation_step.elapsed() >= jinn_slices::SPINNER_INTERVAL {
            self.throbber_state.calc_next();
            self.last_animation_step = Instant::now();
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "idx modulo symbol count is always in bounds"
    )]
    fn current_throbber_symbol(&self) -> &'static str {
        let symbols = &throbber_widgets_tui::ASCII.symbols;
        let len = symbols.len() as i8;
        let mut index = self.throbber_state.index() % len;
        if index < 0 {
            index += len;
        }
        symbols
            .get(index as usize)
            .expect("index modulo throbber symbol count")
    }
}

impl SidebarSection for SessionsSection {
    fn id(&self) -> SidebarSectionId {
        jinn_sidebar_msg::SidebarSectionId::Sessions
    }

    fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        skip_rows: u16,
        ctx: &dyn DrawContext<jinn_kernel::common::app_state::AppState>,
    ) {
        let state = ctx.state();
        let theme = &state.frontend.theme;
        // Read the throbber before borrowing the tree: `sessions_tree` memoizes
        // through `&mut self`, so the cached tree cannot be held while `self` is
        // read again. Cloning this small state is cheaper than cloning the tree.
        let throbber = self.throbber_state.clone();
        let sessions = self.sessions_tree(state);
        let is_startup_hydrating = state.session.is_startup_hydrating();
        let sidebar_focused = state.frontend.is_sidebar();
        let section_focused = sidebar_focused
            && matches!(
                state.frontend.sidebar_section(),
                Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
            );

        // The cursor names a session; the band goes on the row it is drawn on.
        let selected_row = state
            .frontend
            .with_sections(|s| s.sessions.selected_id.clone(), || None)
            .and_then(|id| super::state::visible_row_of(state, &id));

        // The document window decides what is visible, so build lines only for
        // the rows inside it — with an uncapped session list, building a line
        // per session every frame would be the dominant cost.
        let entry_rows = u16::try_from(sessions.len()).unwrap_or(u16::MAX).max(1);
        let window = visible_window(entry_rows, skip_rows, area.height);
        let mut lines = Vec::new();

        if sessions.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                " No open sessions",
                Style::default().fg(theme.muted_text),
            )]));
        } else {
            for i in window.clone() {
                let Some(entry) = sessions.get(usize::from(i)) else {
                    break;
                };
                let is_selected = section_focused && selected_row == Some(usize::from(i));
                let max_title_len = area.width.saturating_sub(4) as usize;
                lines.push(assemble_entry_line(
                    entry,
                    is_selected,
                    max_title_len,
                    &throbber,
                    theme,
                ));
            }
        }

        // The footer is the block's last row, so it appears only once the window
        // reaches it.
        let footer_row = entry_rows.saturating_sub(1);
        let footer_visible = window.start <= footer_row && footer_row < window.end;

        if !sessions.is_empty() || is_startup_hydrating {
            self.maybe_advance_animation();
        }

        if !footer_visible {
            let widget = Paragraph::new(lines).block(Block::default().borders(Borders::NONE));
            frame.render_widget(widget, area);
            return;
        }

        // Footer: ╰─── Sessions ───╯ (with highlighted S)
        let is_startup_hydrating = state.session.is_startup_hydrating();
        let label = " Sessions ";
        let width = area.width as usize;
        // The spinner claims one glyph plus its separating space.
        let label_len = label.len() + 2 * usize::from(is_startup_hydrating);
        let dash_budget = width.saturating_sub(2).saturating_sub(label_len);
        let left_dashes = dash_budget / 2;
        let right_dashes = dash_budget - left_dashes;
        let before_s = format!("\u{2570}{}\u{0020}", "\u{2500}".repeat(left_dashes));
        let after_s = "essions ";
        let right_dashes = format!("{}\u{256F}", "\u{2500}".repeat(right_dashes));

        let footer_color = if section_focused {
            theme.focus_accent
        } else {
            theme.border_unfocused
        };

        let mut footer = vec![Span::styled(before_s, Style::default().fg(footer_color))];
        if is_startup_hydrating {
            footer.push(Span::styled(
                format!("{} ", self.current_throbber_symbol()),
                Style::default().fg(theme.streaming),
            ));
        }
        footer.extend([
            Span::styled("S".to_owned(), Style::default().fg(theme.accent_action)),
            Span::styled(after_s, Style::default().fg(footer_color)),
            Span::styled(right_dashes, Style::default().fg(footer_color)),
        ]);
        lines.push(Line::from(footer));

        let widget = Paragraph::new(lines).block(Block::default().borders(Borders::NONE));
        frame.render_widget(widget, area);
    }

    fn content_height(
        &mut self,
        ctx: &dyn DrawContext<jinn_kernel::common::app_state::AppState>,
    ) -> u16 {
        let entry_count = self.sessions_tree(ctx.state()).len() as u16;
        // entries(N).max(1) + footer(1)
        entry_count.max(1) + 1 // max(1) for the no-sessions placeholder line
    }
}

/// The row range visible in a window of `visible_rows` starting at `skip_rows`,
/// clamped to the block's `entry_rows`.
///
/// The footer occupies the last entry row, so the caller draws it only when the
/// window includes that index.
fn visible_window(entry_rows: u16, skip_rows: u16, visible_rows: u16) -> Range<u16> {
    let start = skip_rows.min(entry_rows);
    let end = start.saturating_add(visible_rows).min(entry_rows);
    start..end
}

/// Renders the close-session confirmation prompt as a floating surface.
///
/// Painted in the sidebar's floating-surface layer (`Region::FloatingSurfaces`),
/// after every base column, so the banner may extend left over the input box.
/// Anchored 1 row above the sidebar cursor row and right-aligned to the
/// frame's right edge — the same geometry as the archive-tree prompt.
pub fn render_close_session_prompt_for_state(
    frame: &mut Frame<'_>,
    sidebar_rect: Rect,
    frame_area: Rect,
    ctx: &dyn DrawContext<AppState>,
) {
    let state = ctx.state();
    if !state.frontend.close_session_prompt
        || !state.frontend.is_sidebar()
        || state.frontend.sidebar_section() != Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
    {
        return;
    }
    if state
        .frontend
        .with_sections(|s| s.sessions.selected_id.is_none(), || true)
    {
        return;
    }

    // Shared cursor-row math: see `render_sidebar_cursor_y`.
    let prompt_y = render_sidebar_cursor_y(
        sidebar_rect,
        state,
        ctx.config(),
        jinn_sidebar_msg::SidebarSectionId::Sessions,
    )
    .saturating_sub(BANNER_GAP);
    let text = " Press x again to teardown and archive 1 session ";
    render_right_aligned_banner(frame, frame_area, prompt_y, text, Color::Yellow);
}

/// Renders the archive-tree confirmation prompt as a floating surface.
///
/// Painted in the sidebar's floating-surface layer (`Region::FloatingSurfaces`),
/// after every base column, so the banner may extend left over the input box.
/// Anchored 1 row above the sidebar cursor row and right-aligned to the
/// frame's right edge, spanning whatever width it needs — it is a
/// overlay, not a sidebar element. Yellow = armed confirm ("Press A/X again
/// to archive/teardown-and-archive N sessions"); red = blocked (a member of
/// the subtree is busy).
pub fn render_archive_tree_prompt_for_state(
    frame: &mut Frame<'_>,
    sidebar_rect: Rect,
    frame_area: Rect,
    ctx: &dyn DrawContext<AppState>,
) {
    let state = ctx.state();
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
        .with_sections(|s| s.sessions.selected_id.is_none(), || true)
    {
        return;
    }

    // Shared cursor-row math: see `render_sidebar_cursor_y`.
    let prompt_y = render_sidebar_cursor_y(
        sidebar_rect,
        state,
        ctx.config(),
        jinn_sidebar_msg::SidebarSectionId::Sessions,
    )
    .saturating_sub(BANNER_GAP);

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

/// Computes a section's cursor row inside the sidebar rect.
///
/// Runs the same document layout the `Sidebar` container uses, so the
/// floating prompts stay attached to the cursor even when the sidebar is
/// scrolled. `section` says which section's cursor anchors the row; the
/// close-session and archive-tree prompts pass Sessions.
fn render_sidebar_cursor_y(
    sidebar_rect: Rect,
    state: &AppState,
    config: &jinn_slices::ConfigLayer,
    section: jinn_sidebar_msg::SidebarSectionId,
) -> u16 {
    let document = crate::sections::layout::document_with_cursor(state, config);
    let offset = document.offset(sidebar_rect.height);
    // `Sidebar::render` pushes a document shorter than the column down by the
    // slack — but only the document's tail section; the rest sit at the top.
    // The anchor must apply the same shift or the banner detaches.
    let slack = if document.is_last(section) {
        document.bottom_slack(sidebar_rect.height)
    } else {
        0
    };
    let span = document.span_or_empty(section);
    let row = crate::sections::layout::cursor_row_in_section(state, section).unwrap_or(0);
    sidebar_rect
        .y
        .saturating_add(slack)
        .saturating_add(span.top_in_view(offset))
        .saturating_add(row)
        .min(
            sidebar_rect
                .y
                .saturating_add(sidebar_rect.height.saturating_sub(1)),
        )
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
