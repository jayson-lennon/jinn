//! Autocomplete popup rendering - renders the prompt template and slash command autocomplete overlay.

use crate::common::app_state::AppState;
use crate::feat::chat_input::AutocompleteTrigger;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// Maximum number of visible rows in the autocomplete popup.
pub const AUTOCOMPLETE_MAX_VISIBLE: usize = 20;
/// Minimum popup width.
pub const AUTOCOMPLETE_MIN_WIDTH: u16 = 20;
/// Maximum popup width as fraction of terminal width.
pub const AUTOCOMPLETE_MAX_WIDTH_FRAC: f32 = 0.60;
/// Separator between name and description.
pub const AUTOCOMPLETE_NAME_DESC_SEP: &str = " - ";
/// Text shown when no prompt template matches found.
const NO_PROMPTS_FOUND: &str = "<no prompts found>";
/// Text shown when no slash command matches found.
const NO_COMMANDS_FOUND: &str = "<no commands found>";

/// Renders the autocomplete popup overlay above the input box.
///
/// The popup is a transient visual element - not a `UiElement`. It reads autocomplete
/// state directly from `AppState` and renders a bordered box with match entries.
/// The popup is horizontally anchored at the trigger token's screen column and sits
/// directly above the input box.
pub fn render_autocomplete_popup(frame: &mut Frame<'_>, input_area: Rect, state: &AppState) {
    // Snapshot what the popup draws so the input facade lock is never held
    // across rendering. Autocomplete matches are already owned values.
    let Some((matches, selected_index, trigger, token_visual)) = state.active_session().with_input(
        |i| {
            let ac = i.autocomplete().clone()?;
            Some((
                ac.matches().to_vec(),
                ac.selected_index(),
                ac.trigger(),
                i.autocomplete_token_visual_row_col(),
            ))
        },
        || None,
    ) else {
        return;
    };

    // `@` popup: matches come from `frontend.file_picker`, not `ac.matches()`.
    if matches!(trigger, AutocompleteTrigger::At | AutocompleteTrigger::AtAt) {
        render_at_popup(frame, input_area, state, selected_index);
        return;
    }

    let Some((token_row, token_col)) = token_visual else {
        return;
    };

    // Prompt indent is always 2 columns ("> " on first line, "  " on continuation).
    let prompt_indent: u16 = 2;
    let anchor_x = input_area.x + prompt_indent + token_col as u16;

    // Compute popup dimensions.
    let term_width = frame.area().width;
    let max_width = ((f32::from(term_width) * AUTOCOMPLETE_MAX_WIDTH_FRAC).ceil() as u16)
        .max(AUTOCOMPLETE_MIN_WIDTH)
        .min(term_width);

    let no_matches_text = match trigger {
        AutocompleteTrigger::Hash => NO_PROMPTS_FOUND,
        AutocompleteTrigger::Slash => NO_COMMANDS_FOUND,
        // `@` popup reads `frontend.file_picker`; rendered separately below.
        AutocompleteTrigger::At | AutocompleteTrigger::AtAt => "<empty>",
    };

    let content_width: u16 = if matches.is_empty() {
        no_matches_text
            .len()
            .try_into()
            .unwrap_or(AUTOCOMPLETE_MIN_WIDTH)
    } else {
        matches
            .iter()
            .map(|m| m.name.len() + AUTOCOMPLETE_NAME_DESC_SEP.len() + m.description.len())
            .max()
            .unwrap_or(0)
            .try_into()
            .unwrap_or(AUTOCOMPLETE_MIN_WIDTH)
    };

    // +2 for left and right border columns.
    let popup_width = content_width
        .saturating_add(2)
        .max(AUTOCOMPLETE_MIN_WIDTH)
        .min(max_width);

    let visible_count = matches.len().min(AUTOCOMPLETE_MAX_VISIBLE);
    let raw_popup_height: u16 = if matches.is_empty() {
        3 // border top + "no matches" + border bottom
    } else {
        u16::try_from(visible_count + 2).unwrap_or(u16::MAX)
    };

    // Position: horizontally anchored at the trigger's wrapped column, vertically
    // floating one row above the trigger's on-screen visual line (the cursor's
    // line) instead of the top of the whole input box.
    let scroll_offset = state.active_session().with_input(
        jinn_chat_input_msg::ChatInputBoxState::scroll_offset,
        Default::default,
    );
    let trigger_screen_y = input_area
        .y
        .saturating_add(token_row.saturating_sub(scroll_offset) as u16);
    let popup_height = clamp_popup_height(raw_popup_height, trigger_screen_y);
    let popup_y = trigger_screen_y.saturating_sub(popup_height);
    let popup_x = anchor_x.min(term_width.saturating_sub(popup_width));

    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    // Clear the popup area so content behind it doesn't show through.
    frame.render_widget(Clear, popup_area);

    // Render bordered block.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Render content.
    if matches.is_empty() {
        let line = Line::styled(no_matches_text, Style::default().fg(Color::DarkGray));
        let paragraph = Paragraph::new(line);
        frame.render_widget(paragraph, inner);
    } else {
        let mut lines = Vec::with_capacity(inner.height as usize);
        let (start, end) = scroll_window(selected_index, matches.len(), inner.height as usize);
        for (i, m) in matches.iter().enumerate().skip(start).take(end - start) {
            let text = if m.description.is_empty() {
                m.name.clone()
            } else {
                format!("{}{}{}", m.name, AUTOCOMPLETE_NAME_DESC_SEP, m.description)
            };
            let style = if i == selected_index {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::styled(text, style));
        }
        // Pad remaining rows with empty lines.
        while lines.len() < inner.height as usize {
            lines.push(Line::from(""));
        }
        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, inner);
    }
}

/// Clamps `popup_height` so the popup never extends above terminal row 0 when
/// its bottom is anchored at `bottom_y`. A minimum height (1 inner row + 2
/// borders = 3) is enforced so the popup never collapses entirely; this may
/// overlap the terminal top on very small terminals, matching prior behavior.
fn clamp_popup_height(raw_height: u16, bottom_y: u16) -> u16 {
    let min_height: u16 = 3;
    raw_height.min(bottom_y).max(min_height)
}

/// Text shown while a directory listing is in flight.
const AT_LOADING: &str = "<loading…>";
/// Text shown when a directory listing is empty/unreadable.
const AT_EMPTY: &str = "<empty>";

/// Renders the `@path` file popup from `frontend.file_picker`.
///
/// Dirs render with a trailing `/`; files render plain. While a listing is
/// in flight, shows `<loading…>`; when the listing is empty, shows `<empty>`.
fn render_at_popup(
    frame: &mut Frame<'_>,
    input_area: Rect,
    state: &AppState,
    selected_index: usize,
) {
    let Some((token_row, token_col, filter)) = state.active_session().with_input(
        |i| {
            i.autocomplete().as_ref()?;
            let (token_row, token_col) = i.autocomplete_token_visual_row_col()?;
            Some((
                token_row,
                token_col,
                i.autocomplete_filter().unwrap_or_default(),
            ))
        },
        || None,
    ) else {
        return;
    };
    let picker = &state.frontend.file_picker;

    // Build the display rows. The `@` popup narrows by the last path segment
    // of the current filter (what the user is typing), so render and confirm
    // share `visible_entries` as the single source of truth.
    let visible = picker.visible_entries(&filter);
    let rows: Vec<String> = if picker.loading {
        vec![AT_LOADING.to_owned()]
    } else if visible.is_empty() {
        vec![AT_EMPTY.to_owned()]
    } else {
        visible
            .iter()
            .map(|e| {
                if e.is_dir {
                    format!("{}/", e.name)
                } else {
                    e.name.clone()
                }
            })
            .collect()
    };

    let prompt_indent: u16 = 2;
    let anchor_x = input_area.x + prompt_indent + token_col as u16;
    let term_width = frame.area().width;
    let max_width = ((f32::from(term_width) * AUTOCOMPLETE_MAX_WIDTH_FRAC).ceil() as u16)
        .max(AUTOCOMPLETE_MIN_WIDTH)
        .min(term_width);

    let content_width: u16 = rows
        .iter()
        .map(|r| r.chars().count())
        .max()
        .unwrap_or(0)
        .try_into()
        .unwrap_or(AUTOCOMPLETE_MIN_WIDTH);
    let popup_width = content_width
        .saturating_add(2)
        .max(AUTOCOMPLETE_MIN_WIDTH)
        .min(max_width);

    let visible_count = rows.len().min(AUTOCOMPLETE_MAX_VISIBLE);
    let raw_popup_height: u16 = if rows.len() <= 1 {
        3 // border top + single line + border bottom
    } else {
        u16::try_from(visible_count + 2).unwrap_or(u16::MAX)
    };

    // Vertically float the popup one row above the trigger's on-screen visual
    // line (the cursor's line), matching the `#`/`/` popup.
    let scroll_offset = state.active_session().with_input(
        jinn_chat_input_msg::ChatInputBoxState::scroll_offset,
        Default::default,
    );
    let trigger_screen_y = input_area
        .y
        .saturating_add(token_row.saturating_sub(scroll_offset) as u16);
    let popup_height = clamp_popup_height(raw_popup_height, trigger_screen_y);
    let popup_y = trigger_screen_y.saturating_sub(popup_height);
    let popup_x = anchor_x.min(term_width.saturating_sub(popup_width));
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let loading = picker.loading;
    let (start, end) = scroll_window(selected_index, rows.len(), inner.height as usize);
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(inner.height as usize);
    for (i, text) in rows.iter().enumerate().skip(start).take(end - start) {
        let style = if loading || i != selected_index {
            Style::default()
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        };
        lines.push(Line::styled(text.as_str(), style));
    }
    // Pad remaining inner rows so the popup keeps a fixed height regardless of
    // where the scroll window sits (mirrors the `#`/`/` popup).
    lines.resize(inner.height as usize, Line::from(""));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Compute a scroll window `[start, end)` that keeps `selected` visible.
///
/// When `total <= visible`, returns `(0, total)`. Otherwise, follows the
/// selection: it advances the window one row at a time once the highlight
/// crosses the bottom edge of the previous window, so the selected entry is
/// always in view (the window trails the highlight, it does not re-center).
pub fn scroll_window(selected: usize, total: usize, visible: usize) -> (usize, usize) {
    if total <= visible {
        return (0, total);
    }
    let start = (selected + 1).saturating_sub(visible);
    let end = (start + visible).min(total);
    (start, end)
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
    use crate::common::app_state::AppState;
    use crate::feat::chat_input::intent::handle_insert_char;
    use crate::feat::file_lister::{FileEntry, FilePickerState};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Renders the `@` popup into a string for assertion.
    fn render_to_string(state: &AppState) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let input_area = ratatui::layout::Rect::new(0, 20, 80, 4);
        terminal
            .draw(|frame| {
                super::render_autocomplete_popup(frame, input_area, state);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect()
    }

    #[rstest::rstest]
    #[test]
    fn render_at_popup_shows_directory_entry_with_trailing_slash() {
        // Given an active @ popup seeded with a directory entry.
        let mut state = AppState::default();
        let _ = handle_insert_char('@', &mut state);
        state.frontend.file_picker = FilePickerState::with_entries(vec![
            FileEntry {
                name: "src".into(),
                is_dir: true,
            },
            FileEntry {
                name: "img.png".into(),
                is_dir: false,
            },
        ]);

        // When rendering.
        let rendered = render_to_string(&state);

        // Then the directory entry shows a trailing slash and the file does not.
        assert!(
            rendered.contains("src/"),
            "dir entry should have trailing slash"
        );
        assert!(
            rendered.contains("img.png"),
            "file entry should render plain"
        );
    }

    #[rstest::rstest]
    #[test]
    fn render_at_popup_shows_loading_state() {
        // Given an active @ popup with a listing in flight.
        let mut state = AppState::default();
        let _ = handle_insert_char('@', &mut state);
        state.frontend.file_picker = FilePickerState {
            loading: true,
            ..FilePickerState::default()
        };

        // When rendering.
        let rendered = render_to_string(&state);

        // Then the loading placeholder is shown.
        assert!(
            rendered.contains("loading"),
            "loading state should be rendered"
        );
    }

    #[rstest::rstest]
    #[test]
    fn render_at_popup_shows_empty_state() {
        // Given an active @ popup with an empty listing (not loading).
        let mut state = AppState::default();
        let _ = handle_insert_char('@', &mut state);
        state.frontend.file_picker = FilePickerState::default(); // empty, not loading

        // When rendering.
        let rendered = render_to_string(&state);

        // Then the empty placeholder is shown.
        assert!(rendered.contains("empty"), "empty state should be rendered");
    }

    #[rstest::rstest]
    #[test]
    fn render_at_popup_narrows_rows_by_typed_prefix() {
        // Given an active @ popup with several entries and a typed filter 'sr'.
        let mut state = AppState::default();
        let _ = handle_insert_char('@', &mut state);
        // Type 'sr' so the last path segment (the filename being typed) is 'sr'.
        let _ = handle_insert_char('s', &mut state);
        let _ = handle_insert_char('r', &mut state);
        state.frontend.file_picker = FilePickerState::with_entries(vec![
            FileEntry {
                name: "src".into(),
                is_dir: true,
            },
            FileEntry {
                name: "srv".into(),
                is_dir: true,
            },
            FileEntry {
                name: "static".into(),
                is_dir: true,
            },
            FileEntry {
                name: "img.png".into(),
                is_dir: false,
            },
        ]);

        // When rendering.
        let rendered = render_to_string(&state);

        // Then only entries starting with 'sr' are shown.
        assert!(rendered.contains("src/"), "src/ should match prefix 'sr'");
        assert!(rendered.contains("srv/"), "srv/ should match prefix 'sr'");
        assert!(
            !rendered.contains("static"),
            "static should be filtered out by prefix 'sr'"
        );
        assert!(
            !rendered.contains("img.png"),
            "img.png should be filtered out by prefix 'sr'"
        );
    }

    #[rstest::rstest]
    #[test]
    fn render_at_popup_shows_empty_when_no_prefix_match() {
        // Given an active @ popup whose entries don't match the typed filter.
        let mut state = AppState::default();
        let _ = handle_insert_char('@', &mut state);
        // Type 'zzz' which matches nothing.
        for ch in "zzz".chars() {
            let _ = handle_insert_char(ch, &mut state);
        }
        state.frontend.file_picker = FilePickerState::with_entries(vec![FileEntry {
            name: "src".into(),
            is_dir: true,
        }]);

        // When rendering.
        let rendered = render_to_string(&state);

        // Then the empty placeholder is shown (distinct from a directory that is
        // actually empty — here the directory has entries but none match).
        assert!(
            rendered.contains("empty"),
            "filtered-out entries should render as <empty>"
        );
    }

    /// Renders the `#` popup into a `TestBackend` buffer and returns the popup's
    /// `Rect` by locating its `┌` top-left border corner.
    ///
    /// Only one popup renders per draw, so the lone `┌` corner uniquely marks
    /// the popup origin; width/height are derived by walking the top/left border.
    /// Panics if no popup rendered.
    fn popup_rect(state: &AppState) -> ratatui::layout::Rect {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let input_area = ratatui::layout::Rect::new(0, 20, 80, 4);
        terminal
            .draw(|frame| {
                super::render_autocomplete_popup(frame, input_area, state);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();

        // Find the top-left corner of the popup.
        let mut origin: Option<(u16, u16)> = None;
        for y in 0..24 {
            for x in 0..80 {
                if buffer.cell((x, y)).map(ratatui::buffer::Cell::symbol) == Some("┌") {
                    origin = Some((x, y));
                }
            }
        }
        let (x, y) = origin.expect("popup corner found");

        // Width: walk right along the top border until it ends.
        let mut width = 1;
        while x + width < 80
            && buffer
                .cell((x + width, y))
                .map(ratatui::buffer::Cell::symbol)
                == Some("─")
        {
            width += 1;
        }
        // +1 for the `┐` right corner if present.
        if x + width < 80
            && buffer
                .cell((x + width, y))
                .map(ratatui::buffer::Cell::symbol)
                == Some("┐")
        {
            width += 1;
        }

        // Height: walk down along the left border until it ends.
        let mut height = 1;
        while y + height < 24
            && buffer
                .cell((x, y + height))
                .map(ratatui::buffer::Cell::symbol)
                == Some("│")
        {
            height += 1;
        }
        // +1 for the `└` bottom corner if present.
        if y + height < 24
            && buffer
                .cell((x, y + height))
                .map(ratatui::buffer::Cell::symbol)
                == Some("└")
        {
            height += 1;
        }

        ratatui::layout::Rect::new(x, y, width, height)
    }

    #[rstest::rstest]
    #[test]
    fn hash_popup_horizontal_anchor_follows_wrapped_trigger_col() {
        // Given a # popup whose trigger sits on a wrapped continuation line,
        // far from the terminal's right edge.
        let mut state = AppState::default();
        state.update_active_input(|i| i.set_wrap_width(5));
        state.update_active_input(|i| i.insert_text("aaaa bbbb#"));
        state.update_active_input(|i| {
            i.activate_autocomplete(
                9,
                crate::feat::chat_input::AutocompleteTrigger::Hash,
                vec![crate::feat::chat_input::AutocompleteMatch {
                    name: "tmpl".into(),
                    description: String::new(),
                }],
            );
        });

        // When rendering.
        let rect = popup_rect(&state);

        // Then the popup's left edge sits near the trigger's wrapped column
        // (prompt_indent 2 + display_col 4 = 6), not clamped to the right edge.
        assert_eq!(
            rect.x, 6,
            "popup left edge should follow the wrapped trigger column"
        );
        assert!(
            rect.x < 70,
            "popup should not be clamped to the terminal's right edge"
        );
    }

    #[rstest::rstest]
    #[test]
    fn hash_popup_sits_above_trigger_visual_line_not_input_top() {
        // Given a # popup whose trigger sits on the second wrapped visual line.
        let mut state = AppState::default();
        state.update_active_input(|i| i.set_wrap_width(5));
        state.update_active_input(|i| i.insert_text("aaaa bbbb#"));
        state.update_active_input(|i| {
            i.activate_autocomplete(
                9,
                crate::feat::chat_input::AutocompleteTrigger::Hash,
                vec![crate::feat::chat_input::AutocompleteMatch {
                    name: "tmpl".into(),
                    description: String::new(),
                }],
            );
        });

        // When rendering (input_area.y=20, trigger on visual row 1 → on-screen y=21).
        let rect = popup_rect(&state);

        // Then the popup floats with the trigger: popup bottom (y + height) sits
        // one row above the trigger's on-screen visual line (y=21).
        assert_eq!(
            rect.y + rect.height,
            21,
            "popup bottom should sit one row above the trigger visual line"
        );
    }

    #[rstest::rstest]
    #[test]
    fn hash_popup_follows_cursor_down_through_wrapped_lines() {
        // Given a # popup whose trigger sits on the THIRD wrapped visual line,
        // so the input-box-top anchor (y=20) and the cursor-line anchor diverge.
        let mut state = AppState::default();
        state.update_active_input(|i| i.set_wrap_width(5));
        // "aaaa aaaa a#" at width 5 → row0 "aaaa ", row1 "aaaa ", row2 "a#".
        state.update_active_input(|i| i.insert_text("aaaa aaaa a#"));
        state.update_active_input(|i| {
            i.activate_autocomplete(
                11,
                crate::feat::chat_input::AutocompleteTrigger::Hash,
                vec![crate::feat::chat_input::AutocompleteMatch {
                    name: "tmpl".into(),
                    description: String::new(),
                }],
            );
        });

        // When rendering.
        let rect = popup_rect(&state);

        // Then the popup floats with the trigger on visual row 2 (on-screen y=22),
        // not pinned to the input-box top (y=20). Popup height 3.
        assert_eq!(
            rect.y + rect.height,
            22,
            "popup bottom should follow the trigger down to row 2, not the input top"
        );
    }

    #[rstest::rstest]
    #[test]
    fn at_popup_uses_cursor_anchored_vertical_positioning() {
        // Given an @ popup whose trigger sits on a wrapped continuation line,
        // seeded with file-picker entries.
        use crate::feat::file_lister::{FileEntry, FilePickerState};
        let mut state = AppState::default();
        state.update_active_input(|i| i.set_wrap_width(5));
        // "aaaa bbbb@" → row0 "aaaa ", row1 "bbb@"? Use a clean wrap:
        // "aaaa aaaa @" at width 5 → row0 "aaaa ", row1 "aaaa ", row2 "@".
        state.update_active_input(|i| i.insert_text("aaaa aaaa @"));
        state.update_active_input(|i| {
            i.activate_autocomplete(11, crate::feat::chat_input::AutocompleteTrigger::At, vec![]);
        });
        state.frontend.file_picker = FilePickerState::with_entries(vec![FileEntry {
            name: "src".into(),
            is_dir: true,
        }]);

        // When rendering (the @ trigger is on visual row 2 → on-screen y=22).
        let rect = popup_rect(&state);

        // Then the @ popup floats with the trigger on row 2 (on-screen y=22),
        // not pinned to the input-box top.
        assert_eq!(
            rect.y + rect.height,
            22,
            "@ popup bottom should follow the trigger down to row 2"
        );
    }
    #[rstest::rstest]
    #[test]
    fn at_popup_horizontal_anchor_follows_wrapped_trigger_col() {
        // Given an @ popup whose trigger sits on a wrapped continuation line,
        // far from the terminal's right edge.
        use crate::feat::file_lister::{FileEntry, FilePickerState};
        let mut state = AppState::default();
        state.update_active_input(|i| i.set_wrap_width(5));
        // "aaaa bbbb@" at width 5 → row0 "aaaa ", row1 "bbbb@" with @ at
        // display col 4 on the wrapped continuation line.
        state.update_active_input(|i| i.insert_text("aaaa bbbb@"));
        state.update_active_input(|i| {
            i.activate_autocomplete(9, crate::feat::chat_input::AutocompleteTrigger::At, vec![]);
        });
        state.frontend.file_picker = FilePickerState::with_entries(vec![FileEntry {
            name: "src".into(),
            is_dir: true,
        }]);

        // When rendering.
        let rect = popup_rect(&state);

        // Then the popup's left edge sits near the trigger's wrapped column
        // (prompt_indent 2 + display_col 4 = 6), not clamped to the right edge.
        assert_eq!(
            rect.x, 6,
            "@ popup left edge should follow the wrapped trigger column"
        );
        assert!(
            rect.x < 70,
            "@ popup should not be clamped to the terminal's right edge"
        );
    }

    #[rstest::rstest]
    #[test]
    fn at_popup_keeps_selected_entry_visible_when_scrolled() {
        // Given an @ popup with 30 entries and the selection moved well past
        // the popup's inner height (18 rows on an 80x24 backend).
        let mut state = AppState::default();
        let _ = handle_insert_char('@', &mut state);
        let entries: Vec<FileEntry> = (0..30)
            .map(|i| FileEntry {
                name: format!("e{i:02}"),
                is_dir: false,
            })
            .collect();
        state.frontend.file_picker = FilePickerState::with_entries(entries);
        for _ in 0..20 {
            state.update_active_input(|i| i.autocomplete_move_down_bounded(30));
        }

        // When rendering.
        let rendered = render_to_string(&state);

        // Then the selected entry (index 20, "e20") is visible.
        assert!(
            rendered.contains("e20"),
            "selected entry should stay visible after scrolling past the bottom"
        );
    }

    #[rstest::rstest]
    #[test]
    fn at_popup_drops_top_entry_when_selection_scrolls_past() {
        // Given an @ popup with 30 entries and the selection scrolled to index 20.
        let mut state = AppState::default();
        let _ = handle_insert_char('@', &mut state);
        let entries: Vec<FileEntry> = (0..30)
            .map(|i| FileEntry {
                name: format!("e{i:02}"),
                is_dir: false,
            })
            .collect();
        state.frontend.file_picker = FilePickerState::with_entries(entries);
        for _ in 0..20 {
            state.update_active_input(|i| i.autocomplete_move_down_bounded(30));
        }

        // When rendering.
        let rendered = render_to_string(&state);

        // Then the top entry (index 0, "e00") is NOT rendered (window scrolled, not
        // clipped-everything).
        assert!(
            !rendered.contains("e00"),
            "top entry should be scrolled out of view once the window advances"
        );
    }

    #[rstest::rstest]
    #[test]
    fn at_popup_shows_top_entry_when_selection_at_zero() {
        // Given an @ popup with 30 entries and the selection at index 0.
        let mut state = AppState::default();
        let _ = handle_insert_char('@', &mut state);
        let entries: Vec<FileEntry> = (0..30)
            .map(|i| FileEntry {
                name: format!("e{i:02}"),
                is_dir: false,
            })
            .collect();
        state.frontend.file_picker = FilePickerState::with_entries(entries);

        // When rendering (selection stays at the default index 0).
        let rendered = render_to_string(&state);

        // Then the top entry (index 0, "e00") IS visible — no spurious scroll at
        // the top boundary.
        assert!(
            rendered.contains("e00"),
            "top entry should remain visible when the selection is at index 0"
        );
    }

    #[rstest::rstest]
    #[test]
    fn at_popup_keeps_last_entry_visible_at_tail() {
        // Given an @ popup with 30 entries and the selection at the last entry.
        let mut state = AppState::default();
        let _ = handle_insert_char('@', &mut state);
        let entries: Vec<FileEntry> = (0..30)
            .map(|i| FileEntry {
                name: format!("e{i:02}"),
                is_dir: false,
            })
            .collect();
        state.frontend.file_picker = FilePickerState::with_entries(entries);
        for _ in 0..30 {
            state.update_active_input(|i| i.autocomplete_move_down_bounded(30));
        }

        // When rendering.
        let rendered = render_to_string(&state);

        // Then the last entry (index 29, "e29") is visible — the window reaches
        // the tail rather than stopping short.
        assert!(
            rendered.contains("e29"),
            "last entry should stay visible when the selection reaches the tail"
        );
    }
}
