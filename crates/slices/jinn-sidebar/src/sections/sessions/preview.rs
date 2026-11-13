//! Session preview popup - shows the last few entries of a highlighted session.
//!
//! Rendered as a bordered overlay when the sidebar sessions section is focused.
//! The popup is anchored bottom-right: its right edge aligns with the right
//! edge of the terminal (just left of the sidebar), and its bottom edge sits
//! just above the sessions section. Displays the last 5 entries rendered using
//! the same entry pipeline as the real chat log, truncated to the last 20 lines.
//! A footer at the bottom shows keybinds across two lines, then the shared
//! status line the chat input's status bar draws — the previewed session's
//! cwd, then its stats and model — with every value taken from the
//! *previewed* session, never the active one.
//!
//! The lines themselves are *not* built here. They arrive already rendered from
//! the layout worker, keyed by the settled entries' content rather than by
//! history length, so a frame costs a refcount bump and a draw. A lookup that
//! misses — because the content or the width moved — falls back to the lines the
//! session is already holding, so the popup is never replaced by a spinner over
//! content it has drawn. Only a session that has never been drawn shows the
//! loading indicator; see [`render_session_preview_loading`].

use std::sync::Arc;

use jiff::SignedDuration;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;

use crate::sections::sessions::preview_load::preview_signature;
#[cfg(test)]
use jinn_chat_log_view::chat_log::{EntryGraphics, RenderContext};
#[cfg(test)]
use jinn_chat_log_view::kernel_element::render_preview as render_preview_lines;
#[cfg(test)]
use jinn_chat_log_view_msg::PREVIEW_ENTRY_COUNT;
use jinn_chat_log_view_msg::PREVIEW_MAX_LINES;
use jinn_chat_log_view_msg::PREVIEW_REQUEST_ENTRY_COUNT;
use jinn_kernel::common::app_state::AppState;
use jinn_provider_config::ModelCache;
use jinn_session_msg::SessionOrigin;
use jinn_session_state::ChatSessionState;
use jinn_slices::DrawContext;
use jinn_status_bar::{model_cache_of, render_session_cwd_line, render_session_info_line};
use jinn_theme::Theme;

/// Default max lines for tool entries when no preference is set.
pub(crate) const DEFAULT_TOOL_ENTRY_MAX_LINES: u16 = 6;
/// Rows the popup spends on its footer: two keybinds lines and the shared
/// two-line status line (cwd, then stats + model).
///
/// The bottom strip of the popup's inner area. The content area is whatever is
/// left of the inner area after these rows, which is what makes the popup's
/// height a fixed number rather than a function of how much text it happens to
/// hold.
pub(crate) const POPUP_FOOTER_ROWS: u16 = 4;
/// Rows between the popup and the cursor row it describes.
///
/// Two rows leaves a one-row gap, so the popup reads as a separate surface
/// rather than colliding with the highlighted session row.
const POPUP_GAP: u16 = 2;
/// The shortest popup worth drawing: two borders, four footer rows, and at
/// least one row of content — a box with nothing in it says nothing.
///
/// Also the room the cursor must leave above it. A box shorter than this would
/// have no content row at all, so a cursor with less room than this above it
/// gets no popup rather than one drawn over itself.
const MIN_POPUP_HEIGHT: u16 = 7;

/// Renders the session preview popup when a previewing sidebar section is focused.
///
/// Checks focus state (the sessions tree and the library cell both preview),
/// resolves the highlighted session through the focused section's own row
/// space, computes the popup rect anchored bottom-right (above the cursor
/// row, right-aligned with the terminal edge), and delegates to
/// [`render_session_preview`].
/// This is the primary entry point for the TUI render loop.
///
/// - `sidebar_rect`: the full sidebar column rect
/// - `frame_area`: the total frame area (used for right-edge alignment)
pub fn render_session_preview_for_state(
    frame: &mut Frame<'_>,
    sidebar_rect: Rect,
    frame_area: Rect,
    ctx: &dyn DrawContext<AppState>,
) {
    let state = ctx.state();

    // Which section is focused decides both the session and the row the
    // popup hangs from: the sessions tree and the library cell order the
    // same sessions differently, so the anchor math runs per section over
    // the section's own row space.
    let Some(
        section @ (jinn_sidebar_msg::SidebarSectionId::Sessions
        | jinn_sidebar_msg::SidebarSectionId::Library),
    ) = state.frontend.sidebar_section()
    else {
        return;
    };
    let Some(id) = crate::sections::sessions::preview_load::previewed_session_id(state) else {
        return;
    };
    // The cursor names a session; the popup needs the row to hang off. The
    // anchor is the same section-relative cursor row the layout math feeds
    // the scroll and the band — `cursor_row_in_section` resolves each
    // section's own convention (the library adds its two header rows, the
    // sessions tree none). A bare member index would forget the header rows
    // and hang the popup two rows above the cursor.
    let Some(idx) = crate::sections::layout::cursor_row_in_section(state, section) else {
        return;
    };

    let Some(session) = state.session.get(&id) else {
        return;
    };
    let theme = &state.frontend.theme;

    // Anchor the popup to the cursor through the same document layout the
    // sidebar renders with, so it stays attached while the column scrolls.
    let cursor_y =
        crate::sections::layout::frame_row_of(sidebar_rect, state, ctx.config(), section, idx);

    // The same derivation the pre-render pass records, with no floor applied:
    // both sides must produce the identical number, including zero. Flooring
    // here and not there would put the lookup one column away from the request
    // that filled it, which is the "loading forever" this whole function guards.
    let inner_width = preview_content_width(frame_area);

    // The cached lines are found by the same identity the keyboard path
    // requested with — session, content, width — so a hit means the worker has
    // already wrapped exactly this text at exactly this width. The `cloned` is
    // an `Arc` refcount bump, not a copy of the rendered lines.
    let signature = preview_signature(session.history(), PREVIEW_REQUEST_ENTRY_COUNT);
    let cached = state.frontend.with_sections(
        |s| {
            s.sessions
                .preview
                .cached(&id, signature, inner_width)
                // A miss on content or width falls back to whatever this session
                // is already holding rather than to the spinner. Both happen
                // routinely mid-turn: content moves on every streamed token and
                // the width moves on a resize, and in each case the lines on
                // screen are still the conversation the user is reading. Putting
                // a spinner over them — and blanking the popup while a re-render
                // is queued — is the flicker this fallback removes.
                .or_else(|| s.sessions.preview.drawable(&id))
                .map(Arc::clone)
        },
        || None,
    );

    // A miss with nothing in flight means the request that should have been
    // published never arrived — the popup will spin until the next cursor move.
    // The transient miss that follows a just-published request is normal and
    // not worth a line; a *stuck* one is the whole bug, so it is reported.
    if cached.is_none()
        && !state
            .frontend
            .with_sections(|s| s.sessions.preview.is_in_flight_for(&id), || false)
    {
        tracing::warn!(session_id=%id, signature, lookup_width=inner_width,
            recorded_width=state.frontend.with_sections(|s| s.sessions.preview_content_width, || 0),
            "session preview has nothing cached and nothing in flight");
    }
    // Resolved once for both states below, so the loading box and the ready box
    // are the same rect and the surface cannot resize when the lines land.
    // `None` means the cursor leaves no room above it for a box that has a
    // content row; there is then no rect that does not cover the cursor, so
    // nothing is drawn. Whichever way this goes, the cursor row is untouched.
    let Some(popup_rect) = session_preview_popup_rect(frame_area, cursor_y) else {
        return;
    };

    // The status line's two non-session inputs, resolved where the draw
    // context is in reach and passed down: the popup itself holds no cell
    // lookup. The working time is the *previewed* session's clock, measured
    // live so a session that worked ticks in its own preview.
    let model_cache = model_cache_of(state);
    let working_time = jinn_status_bar::session_working_time(session.session_id(), ctx.slices());

    let Some(lines) = cached else {
        // Nothing has ever been drawn for this session, at any content or width.
        // `cached` returning `None` is what distinguishes loading from empty — an
        // empty session renders zero lines but is still a cache hit, so it takes
        // the branch below and shows the empty state rather than spinning forever.
        render_session_preview_loading(
            frame,
            popup_rect,
            session,
            theme,
            model_cache.as_ref(),
            working_time,
        );
        return;
    };

    render_session_preview(
        frame,
        popup_rect,
        session,
        theme,
        &lines,
        model_cache.as_ref(),
        working_time,
    );
}

/// Renders the session preview popup into the given frame area.
///
/// Shows a bordered popup with:
/// - Session title in the top border
/// - Up to 20 lines of content from the last 5 entries
/// - A footer with two lines of keybinds and the shared two-line status
///   (cwd; then stats left, model right) for the previewed session
///
/// The status line's two non-session inputs arrive as parameters because the
/// popup holds no cell lookups of its own: `model_cache` is the app-level
/// provider cache (context limit, modality indicator; `None` degrades to
/// `???`/text-only exactly as the status bar does without a provider cell),
/// and `working_time` is the previewed session's clock, measured by the
/// caller through its draw context.
pub fn render_session_preview(
    frame: &mut Frame<'_>,
    popup_area: Rect,
    session: &ChatSessionState,
    theme: &Theme,
    lines: &[Line<'static>],
    model_cache: Option<&ModelCache>,
    working_time: SignedDuration,
) {
    render_session_preview_inner(
        frame,
        popup_area,
        session,
        theme,
        lines,
        model_cache,
        working_time,
    );
}

/// Computes the popup width: 60% of frame area, min 30, max frame width.
///
/// The width a preview's lines are wrapped at, and the width it is cached
/// under. Public because the keyboard path has to name the same width when it
/// asks for a render: if the two sides disagreed by even one column, the
/// rendered lines could never match the lookup and the preview would spin
/// forever. Both derive it here rather than each measuring for itself.
#[must_use]
pub fn preview_width(frame_area: Rect) -> u16 {
    let w = (f32::from(frame_area.width) * 0.6).ceil() as u16;
    w.max(30).min(frame_area.width)
}

/// The width a preview's lines are wrapped at, inside the popup's borders.
///
/// `0` before a frame has been measured: there is no width to wrap for yet.
#[must_use]
pub fn preview_content_width(frame_area: Rect) -> u16 {
    preview_width(frame_area).saturating_sub(2)
}

/// How far through the spin the current frame is.
///
/// Read from the wall clock rather than a stored step so the popup keeps no
/// animation state of its own. Wrapping at a whole number of frames keeps the
/// glyph cycling without letting the index grow without bound.
fn spinner_elapsed() -> std::time::Duration {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO);
    std::time::Duration::from_millis(u64::try_from(now.as_millis() % 1000).unwrap_or(0))
}

/// Renders the popup's chrome with a spinner where the content will go.
///
/// Drawn on the *last* row of the content area rather than the first, and
/// centred within it, matching the chat log's session-load line: the indicator
/// reads as a status line under the preview rather than floating at the top of
/// an empty box. The popup's own height is fixed, so there is no need to
/// reserve rows for it — the spinner occupies the row where content will appear,
/// which is the row content arrives on.
///
/// The chrome, title, badge, and footer are identical to the ready state, so
/// only the content area changes — the loading popup carries the same shared
/// status line for the session, so the footer never flickers either.
pub fn render_session_preview_loading(
    frame: &mut Frame<'_>,
    popup_area: Rect,
    session: &ChatSessionState,
    theme: &Theme,
    model_cache: Option<&ModelCache>,
    working_time: SignedDuration,
) {
    let inner_width = popup_area.width.saturating_sub(2);
    if inner_width == 0 || popup_area.height == 0 {
        return;
    }

    // Elapsed time drives the glyph rather than a stored animation step, so the
    // popup holds no clock of its own. See `jinn_slices::spinner`.
    let glyph = jinn_slices::spinner_glyph(spinner_elapsed());

    // Content is one line: a glyph followed by a short label, so the popup does
    // not read as empty while it waits. The label takes the theme's `streaming`
    // color — the same one the chat log's loading indicator uses — so "this is
    // working" reads identically wherever it appears. Muted grey said "nothing
    // here" rather than "wait".
    let text = format!("{glyph} loading\u{2026}");
    // A bare paragraph draws left-aligned, so the line is indented to centre it
    // the way the chat log's load line is: start half the slack in. Centring by
    // counting the *rendered* width rather than `text.len()` keeps it right if
    // the glyph is ever a wide character, where the two disagree.
    let line_width = u16::try_from(UnicodeSegmentation::graphemes(text.as_str(), true).count())
        .unwrap_or(u16::MAX);
    let indent = inner_width.saturating_sub(line_width) / 2;
    // Indent only, no trailing pad: the content area already ends in blanks, and
    // padding out to the full width would overrun it and wrap the label onto a
    // second row.
    let mut spans = Vec::with_capacity(2);
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(usize::from(indent))));
    }
    spans.push(Span::styled(text, Style::default().fg(theme.streaming)));
    let content = Line::from(spans);

    render_session_preview_inner(
        frame,
        popup_area,
        session,
        theme,
        &[content],
        model_cache,
        working_time,
    );
}

/// The color the popup's border is drawn in.
///
/// A preview is a way of asking what a session *is* before opening it, and for
/// a subagent or a bot that is the one question a title cannot answer —
/// the session list already tints those rows, and the popup that hangs off the
/// row was saying nothing. The border carries the same tint, so the popup and
/// the row that opened it agree. Every other kind keeps the ordinary unfocused
/// border: an ordinary session needs no distinguishing.
fn preview_border_fg(session: &ChatSessionState, theme: &Theme) -> ratatui::style::Color {
    match session.origin() {
        SessionOrigin::Bot => theme.bot_fg,
        SessionOrigin::Subagent => theme.subagent_fg,
        SessionOrigin::User | SessionOrigin::Fork => theme.border_unfocused,
    }
}

/// The preview's lines with any trailing blank rows dropped.
///
/// Every entry is padded above and below, so the last line a worker returns is
/// usually a blank spacer. Anchoring *that* to the row above the footer would
/// leave the newest actual text one row higher than the surface claims to put
/// it — the promise is that the newest text is the last visible content row, so
/// the trailing spacers are not part of it.
fn without_trailing_blanks<'a>(lines: &'a [Line<'static>]) -> &'a [Line<'static>] {
    let end = lines
        .iter()
        .rposition(|line| {
            !line
                .spans
                .iter()
                .all(|span| span.content.chars().all(char::is_whitespace))
        })
        .map_or(0, |last| last + 1);
    let (kept, _trailing) = lines.split_at(end);
    kept
}

/// How many rows a line occupies when wrapped to `width`.
///
/// Normally one: the worker pads every line out to exactly the content width,
/// and the content area is that same width. The `ceil` matters in the one case
/// where they disagree — a horizontally capped popup is narrower than the width
/// the lines were wrapped at, so a padded line re-wraps into more than one
/// visual row. Counting rows rather than lines is what keeps the bottom anchor
/// honest there: the newest line must land on the last row, and if it silently
/// grew to two rows the anchor would be a row off.
fn rendered_rows(line: &Line<'_>, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let line_width = u16::try_from(line.width()).unwrap_or(u16::MAX);
    if line_width == 0 {
        1
    } else {
        line_width.div_ceil(width).max(1)
    }
}

/// The trailing lines that fit in a content area `rows` rows tall, and how many
/// rows they actually occupy.
///
/// The preview is anchored to its *end*, not its start: the newest entry is
/// what the user is reading, and it is what should be where their eye already
/// is. A preview with fewer rows than the area keeps all of them; one with more
/// drops the excess from the front, so the newest line always survives.
///
/// When even the newest line does not fit — a box too small for the one row it
/// needs — it is taken anyway. A clipped line is still a preview; an empty
/// content area is not.
fn trailing_lines<'a>(
    lines: &'a [Line<'static>],
    rows: u16,
    width: u16,
) -> (&'a [Line<'static>], u16) {
    let mut used = 0u16;
    let mut start = lines.len();
    for (i, line) in lines.iter().enumerate().rev() {
        let line_rows = rendered_rows(line, width);
        if used.saturating_add(line_rows) > rows {
            break;
        }
        used = used.saturating_add(line_rows);
        start = i;
    }
    if start == lines.len()
        && let Some(last) = lines.last()
    {
        // Even the newest line does not fit; take it anyway, clipped to the
        // budget — a clipped line is still a preview, an empty area is not.
        return (
            std::slice::from_ref(last),
            rendered_rows(last, width).min(rows),
        );
    }
    (lines.get(start..).unwrap_or(&[]), used)
}

/// Divides the content area into the rectangles a bottom-anchored draw needs.
///
/// The tail sits on the content area's *last* row, with whatever room is left
/// above it for lines that did not fit. Two rects rather than one because the
/// tail is drawn top-down into its own rect and a line that re-wraps needs
/// somewhere to grow into; sizing the tail rect to the tail's true rendered
/// height and pinning it to the bottom is what keeps the newest line on the
/// last row. A tail that fills the area leaves the head rect empty.
fn split_content_area(content_area: Rect, tail_rows: u16) -> (Rect, Rect) {
    let tail_rows = tail_rows.min(content_area.height);
    let tail = Rect {
        x: content_area.x,
        y: content_area.y + content_area.height - tail_rows,
        width: content_area.width,
        height: tail_rows,
    };
    let head = Rect {
        x: content_area.x,
        y: content_area.y,
        width: content_area.width,
        height: content_area.height - tail_rows,
    };
    (head, tail)
}

/// Draws the popup's chrome and content.
///
/// One function for both states, because the chrome is the whole point: a popup
/// whose borders, title, badge, and footer appeared and disappeared with the
/// spinner would read as two different surfaces rather than one that is waiting.
/// The only difference between the callers is what they pass as `lines`.
fn render_session_preview_inner(
    frame: &mut Frame<'_>,
    popup_area: Rect,
    session: &ChatSessionState,
    theme: &Theme,
    lines: &[Line<'static>],
    model_cache: Option<&ModelCache>,
    working_time: SignedDuration,
) {
    let inner_width = popup_area.width.saturating_sub(2);
    if inner_width == 0 || popup_area.height == 0 {
        return;
    }

    let title = session.title().unwrap_or("Untitled Session");

    // Clear the popup area.
    frame.render_widget(Clear, popup_area);

    // Render the bordered block with session title.
    let block = {
        // Completion badge: `{completed}/{total} · {pct}%`, right-aligned in the
        // top border. Suppressed when the task list has no tasks.
        let badge = {
            let (completed, total) = session.task_list().completion_counts();
            (total > 0).then(|| {
                let pct = completed * 100 / total;
                format!(" {completed}/{total} \u{00B7} {pct}% ")
            })
        };
        let mut block = Block::default()
            .title(Span::styled(
                format!(" {title} "),
                Style::default().fg(theme.popup_title),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(preview_border_fg(session, theme)));
        if let Some(badge) = badge {
            block = block.title(
                Line::from(Span::styled(badge, Style::default().fg(theme.streaming)))
                    .alignment(Alignment::Right),
            );
        }
        block
    };
    frame.render_widget(block, popup_area);

    // Inner area (inside borders).
    let inner_area = Rect {
        x: popup_area.x + 1,
        y: popup_area.y + 1,
        width: inner_width,
        height: popup_area.height.saturating_sub(2),
    };
    if inner_area.width == 0 || inner_area.height == 0 {
        return;
    }

    // Content, anchored to the bottom of the content area. The footer occupies
    // the rows below it, so the newest line always sits directly above the
    // keybinds — in a capped box as well as a full one.
    let content_area = Rect {
        x: inner_area.x,
        y: inner_area.y,
        width: inner_area.width,
        height: content_area_height(inner_area),
    };
    if content_area.height > 0 {
        // Trailing spacer rows go first, so what is anchored to the last row is
        // the newest text rather than the blank line under it.
        let lines = without_trailing_blanks(lines);
        if !lines.is_empty() {
            let (tail, tail_rows) = trailing_lines(lines, content_area.height, content_area.width);
            let (head, tail_area) = split_content_area(content_area, tail_rows);
            let head_len = lines.len() - tail.len();
            let (head_lines, _tail_dup) = lines.split_at(head_len);
            if head.height > 0 {
                frame.render_widget(
                    Paragraph::new(head_lines.to_vec()).wrap(Wrap { trim: false }),
                    head,
                );
            }
            frame.render_widget(
                Paragraph::new(tail.to_vec()).wrap(Wrap { trim: false }),
                tail_area,
            );
        }
    }

    // Footer: keybinds + the shared status line at the bottom of the inner area.
    render_keybinds_bar(frame, inner_area, theme);
    render_status_line(frame, inner_area, session, theme, model_cache, working_time);
}

/// Renders the keybinds bar at the bottom of the popup.
///
/// Two lines:
/// - Line 1: `x close · a archive · i insert`
/// - Line 2: `c continue · r rename`
fn render_keybinds_bar(frame: &mut Frame<'_>, inner_area: Rect, theme: &Theme) {
    let key_style = Style::default()
        .fg(theme.accent_action)
        .add_modifier(Modifier::BOLD);
    let sep_style = Style::default().fg(theme.muted_text);

    // Line 1: x close · a archive · i insert
    let line1_y = inner_area.y + inner_area.height.saturating_sub(4);
    let line1_spans = vec![
        Span::styled("x", key_style),
        Span::styled(" close", sep_style),
        Span::styled(" · ", sep_style),
        Span::styled("a", key_style),
        Span::styled(" archive", sep_style),
        Span::styled(" · ", sep_style),
        Span::styled("i", key_style),
        Span::styled(" insert", sep_style),
    ];
    let line1_area = Rect {
        x: inner_area.x,
        y: line1_y,
        width: inner_area.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(Line::from(line1_spans)), line1_area);

    // Line 2: c continue · r rename
    let line2_y = inner_area.y + inner_area.height.saturating_sub(3);
    let line2_spans = vec![
        Span::styled("c", key_style),
        Span::styled(" continue", sep_style),
        Span::styled(" · ", sep_style),
        Span::styled("r", key_style),
        Span::styled(" rename", sep_style),
    ];
    let line2_area = Rect {
        x: inner_area.x,
        y: line2_y,
        width: inner_area.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(Line::from(line2_spans)), line2_area);
}

/// Renders the shared status line at the very bottom of the popup.
///
/// The same two lines the status bar under the chat input draws, for the
/// *previewed* session: the cwd on the first row (home collapsed to `~`),
/// then the session's stats left-aligned and its model right-aligned. No
/// aggregate is ever rendered here — the tree aggregate and the transient
/// hint are active-session chrome and stay on the status bar — so every
/// value on these rows is the previewed session's own.
fn render_status_line(
    frame: &mut Frame<'_>,
    inner_area: Rect,
    session: &ChatSessionState,
    theme: &Theme,
    model_cache: Option<&ModelCache>,
    working_time: SignedDuration,
) {
    let style = Style::default().fg(theme.muted_text);
    let cwd_area = Rect {
        x: inner_area.x,
        y: inner_area.y + inner_area.height.saturating_sub(2),
        width: inner_area.width,
        height: 1,
    };
    render_session_cwd_line(frame, cwd_area, session, style);

    let info_area = Rect {
        x: inner_area.x,
        y: inner_area.y + inner_area.height.saturating_sub(1),
        width: inner_area.width,
        height: 1,
    };
    // No right-side override: the preview's info line always ends in the
    // previewed session's model. The override is the status bar's
    // hint-replaces-model seam, and a hint is active-session chrome.
    render_session_info_line(
        frame,
        info_area,
        session,
        theme,
        model_cache,
        working_time,
        style,
        None,
    );
}

/// Computes the popup rectangle for the session preview overlay.
///
/// The popup is anchored to the right edge of the frame and sits just above
/// the cursor row, with a two-row gap ([`POPUP_GAP`]). Width is 60% of the
/// frame.
///
/// Height is *fixed*: the content area is [`PREVIEW_MAX_LINES`] rows, plus the
/// two borders and the four footer rows. It is deliberately not derived from
/// how many lines the content happens to render to. A preview that grew and
/// shrank with its text made the surface move under the cursor while scrolling
/// the list and re-shaped on every token of a streaming reply; the box now
/// stays put and the content changes inside it.
///
/// The only thing that varies the height is the cap — a terminal with less room
/// above the cursor than the popup wants gets a shorter box.
///
/// Returns `None` when there is no room above the cursor for even the shortest
/// box worth drawing. The popup hangs above the cursor rather than beside it, so
/// below that floor the only rect it could occupy is the cursor's own row: the
/// alternative is a box drawn over the row that opened it, which leaves the user
/// without either the cursor or the preview.
#[must_use]
pub fn session_preview_popup_rect(frame_area: Rect, cursor_y: u16) -> Option<Rect> {
    let popup_width = preview_width(frame_area);

    // Content rows come from the same budget the layout worker renders to, so
    // the box is exactly as tall as the preview it was sized for. Not a second
    // constant: a number asserted twice is a number that will disagree.
    let content_rows = u16::try_from(PREVIEW_MAX_LINES).unwrap_or(u16::MAX);
    // Total height: content + footer (4) + top border (1) + bottom border (1).
    let desired_height = content_rows
        .saturating_add(POPUP_FOOTER_ROWS)
        .saturating_add(2);
    // Cap to available space above the cursor (with 1-row gap). The floor is
    // applied once, here: a box shorter than `MIN_POPUP_HEIGHT` has no content
    // row left to put a preview in.
    let max_height = cursor_y
        .saturating_sub(frame_area.y)
        .saturating_sub(POPUP_GAP);
    if max_height < MIN_POPUP_HEIGHT {
        return None;
    }
    let popup_height = desired_height.min(max_height).max(MIN_POPUP_HEIGHT);

    // Right-align: right edge = frame right edge.
    let popup_x = frame_area.x + frame_area.width.saturating_sub(popup_width);
    // Bottom edge sits `POPUP_GAP` rows above the cursor. With `max_height`
    // above the floor this cannot underflow, so the popup's bottom edge plus
    // the gap is the cursor row — the gap is honoured exactly, never traded
    // for a taller box.
    let popup_y = cursor_y
        .saturating_sub(popup_height)
        .saturating_sub(POPUP_GAP);

    Some(Rect::new(popup_x, popup_y, popup_width, popup_height))
}

/// The content rows the popup's inner area has left once the footer is taken.
///
/// Where the preview's lines — and the loading indicator — are drawn. Smaller
/// than [`PREVIEW_MAX_LINES`] whenever the rect was capped, which is why the
/// content placement below is written against this rather than the budget.
fn content_area_height(inner_area: Rect) -> u16 {
    inner_area.height.saturating_sub(POPUP_FOOTER_ROWS)
}

#[cfg(test)]
mod worker_tests {
    //! The worker's half of the preview: a request arrives, lines come back.
    //!
    //! Asserts the worker's own arithmetic — the truncation to the trailing
    //! entries and to the line budget — which the bus test in the sidebar
    //! cannot isolate from delivery.
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

    use super::*;
    use jinn_kernel::protocol::ChatEntry;
    use jinn_session_state::ChatSessionState;

    /// The theme the worker's render context carries.
    fn default_theme() -> jinn_theme::Theme {
        jinn_kernel::common::app_state::AppState::default_with_scope_focus()
            .frontend
            .theme
    }

    /// A session with `count` one-line user entries.
    fn session_with(count: usize) -> ChatSessionState {
        let mut session = ChatSessionState::new();
        for i in 0..count {
            session.push_entry(ChatEntry::user(format!("message {i}")));
        }
        session
    }

    /// The preview of `session`, rendered the way the worker renders it.
    fn preview(session: &ChatSessionState) -> Vec<Line<'static>> {
        let ctx = RenderContext {
            content_width: 40,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: DEFAULT_TOOL_ENTRY_MAX_LINES,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
            graphics: EntryGraphics::default(),
            session_id: jinn_core_types::SessionId::new(),
        };
        render_preview_lines(
            session.history(),
            &ctx,
            PREVIEW_ENTRY_COUNT,
            PREVIEW_MAX_LINES,
        )
    }

    /// The whole preview as one string, for a substring assertion.
    fn preview_text(session: &ChatSessionState) -> String {
        preview(session)
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[rstest::rstest]
    fn the_worker_previews_only_the_trailing_entries() {
        // Given a session with far more entries than the preview reaches back
        // over.
        let session = session_with(50);

        // When the worker renders its preview.
        let text = preview_text(&session);

        // Then the oldest entries are left out. The walk is bounded even though
        // it is budget-driven: filling twenty rows does not license reading the
        // whole history.
        assert!(
            !text.contains("message 0"),
            "the preview must not include the oldest entry"
        );
        // The walk stops at the line budget long before the reach bound on a
        // history of three-row messages, so the preview starts where twenty rows
        // of tail ends rather than at the reach bound itself.
        assert!(
            !text.contains(&format!("message {}", 50 - PREVIEW_ENTRY_COUNT - 1)),
            "the preview must stop once its budget is full, not walk the reach bound"
        );
    }

    #[rstest::rstest]
    fn the_worker_previews_the_newest_entry() {
        // Given a session with more entries than the preview shows.
        let session = session_with(50);

        // When the worker renders its preview.
        let text = preview_text(&session);

        // Then the newest entry is the one at the bottom.
        assert!(
            text.contains("message 49"),
            "the preview must end at the newest entry"
        );
    }

    #[rstest::rstest]
    fn the_worker_returns_no_lines_for_an_empty_session() {
        // Given a session with no entries.
        let session = session_with(0);

        // When the worker renders its preview.
        let lines = preview(&session);

        // Then there is nothing to show, which is complete rather than loading.
        assert_eq!(lines, [] as [ratatui::prelude::Line<'_>; 0]);
    }

    #[rstest::rstest]
    fn the_worker_renders_the_same_lines_from_a_trimmed_tail() {
        // Given a session with more entries than the preview shows.
        let session = session_with(50);
        let full = preview(&session);
        let start = session.history().len().saturating_sub(PREVIEW_ENTRY_COUNT);
        let tail = &session.history()[start..];

        // When the worker is handed only the trailing entries instead.
        let ctx = RenderContext {
            content_width: 40,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: DEFAULT_TOOL_ENTRY_MAX_LINES,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
            graphics: EntryGraphics::default(),
            session_id: jinn_core_types::SessionId::new(),
        };
        let from_tail = render_preview_lines(tail, &ctx, PREVIEW_ENTRY_COUNT, PREVIEW_MAX_LINES);

        // Then the result is identical, so trimming at the request boundary is
        // invisible to the worker.
        assert_eq!(full, from_tail);
    }

    #[rstest::rstest]
    fn the_worker_respects_the_line_budget() {
        // Given a session whose trailing entries would overflow the line budget
        // if rendered in full.
        let mut session = ChatSessionState::new();
        for i in 0..PREVIEW_ENTRY_COUNT {
            let text = "x".repeat(40);
            session.push_entry(ChatEntry::user(format!("{i} {text}")));
        }

        // When the worker renders its preview.
        let lines = preview(&session);

        // Then the result fits the budget, so the popup has a bounded height.
        assert!(
            lines.len() <= PREVIEW_MAX_LINES,
            "preview returned {} lines, over the {PREVIEW_MAX_LINES} budget",
            lines.len()
        );
    }
}
