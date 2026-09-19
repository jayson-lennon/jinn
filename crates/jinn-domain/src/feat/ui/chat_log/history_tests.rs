#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use crate::common::app_state::{AppState, FocusScope};
use crate::common::render_ctx::RenderCtx;
use crate::common::ui_element::UiElement;
use crate::protocol::ToolResultStatus;
use crate::feat::ui::chat_log::history::ChatLogElement;
use crate::feat::ui::chat_log::shared::GUTTER_WIDTH;
use crate::protocol::{ChatEntry, PinPosition};
use jinn_testutil::setup_term;
use ratatui::style::Color;

const G: u16 = GUTTER_WIDTH; // = 2

/// Creates an AppState with Normal scope (clears the default Input overlay).
///
/// Chat log rendering tests need Normal scope so that the gutter cursor
/// bar and selection highlighting are active.
fn normal_state() -> AppState {
    let s = AppState::default_with_scope_focus();
    s.frontend.scope_clear_overlays();
    s
}

/// Build a compaction entry with the given summary (struct literal — no
/// `ChatEntry::compaction(...)` constructor exists).
fn compaction_entry(summary: &str) -> ChatEntry {
    use crate::protocol::{ChatEntryId, ChatEntryKind};
    use crate::protocol::{ContextOverride, EntryTiming};
    ChatEntry {
        id: ChatEntryId::new(),
        timing: EntryTiming::instant_now(),
        kind: ChatEntryKind::Compaction {
            summary: summary.to_owned(),
            tokens_before: 100,
            tokens_after: 50,
            entries_compacted: 5,
            model_used: "test/model".to_owned(),
        },
        pin_position: None,
        context_override: ContextOverride::Default,
        context_history: Vec::new(),
        token_count: None,
    }
}

#[rstest::rstest]
fn name_returns_chat_log() {
    // Given a ChatLogElement.
    let element = ChatLogElement::new();

    // When querying the name.
    let name = element.name();

    // Then it is "chat-log".
    assert_eq!(name, "chat-log");
}

#[rstest::rstest]
fn render_few_messages_bottom_aligned() {
    // Given a ChatLogElement with one user entry in a 40x10 viewport.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the user text appears in the content area (above the bottom padding).
    let buffer = terminal.backend().buffer().clone();
    let content_cell = buffer.cell((G, 8)).expect("cell should exist");
    assert_eq!(content_cell.symbol(), "h");
}

#[rstest::rstest]
fn chat_log_element_is_selectable() {
    // Given a ChatLogElement.
    let element = ChatLogElement::new();

    // When calling is_selectable.
    let selectable: &dyn UiElement = &element;

    // Then it returns true.
    assert!(selectable.is_selectable());
}

#[rstest::rstest]
fn selected_entry_gutter_col0_has_context_fg_and_col1_has_cursor_bg() {
    // Given a ChatLogElement with 2 entries, first selected.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s.active_session_mut().push_entry(ChatEntry::user("world"));
        // push_entry auto-selects last (index 1). Move to index 0.
        s.active_session_mut().select_prev_entry();
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the selected entry's gutter col 0 has teal fg.
    // 2 entries × 3 lines = 6, 4 blank above. Entry 0 at rows 4-6.
    let buffer = terminal.backend().buffer().clone();
    let gutter_col0 = buffer.cell((0, 5)).expect("cell should exist");
    assert_eq!(
        gutter_col0.style().fg,
        Some(crate::feat::theme::default_theme().gutter_context_included)
    );

    // And the selected entry's gutter col 1 has yellow fg (cursor).
    let gutter_col1 = buffer.cell((1, 5)).expect("cell should exist");
    assert_eq!(gutter_col1.style().fg, Some(Color::Yellow));

    // And the unselected entry's gutter col 0 has context fg.
    let unselected_col0 = buffer.cell((0, 8)).expect("cell should exist");
    assert_eq!(
        unselected_col0.style().fg,
        Some(crate::feat::theme::default_theme().gutter_context_included)
    );

    // And the unselected entry's gutter col 1 has no yellow fg.
    let unselected_col1 = buffer.cell((1, 8)).expect("cell should exist");
    assert_ne!(unselected_col1.style().fg, Some(Color::Yellow));
}

#[rstest::rstest]
fn unselected_not_ignored_entry_shows_context_color() {
    // Given a ChatLogElement with 2 entries, second selected, first not ignored.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s.active_session_mut().push_entry(ChatEntry::user("world"));
        // push_entry auto-selects last (index 1). Entry 0 is unselected, not ignored.
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the unselected, not-ignored entry's gutter has the context included color.
    // 2 entries × 3 lines = 6, 4 blank above. Entry 0 at rows 4-6.
    let buffer = terminal.backend().buffer().clone();
    let gutter_cell = buffer.cell((0, 5)).expect("cell should exist");
    assert_eq!(
        gutter_cell.style().fg,
        Some(crate::feat::theme::default_theme().gutter_context_included)
    );
}

#[rstest::rstest]
fn unselected_ignored_entry_shows_gray() {
    // Given a ChatLogElement with 2 entries, first ignored, second selected.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut()
            .push_entry(ChatEntry::user("hello").with_ignored(true));
        s.active_session_mut().push_entry(ChatEntry::user("world"));
        // push_entry auto-selects last (index 1). Entry 0 is unselected, ignored.
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the unselected, ignored entry's gutter has the border_unfocused color.
    // 2 entries × 3 lines = 6, 4 blank above. Entry 0 at rows 4-6.
    let buffer = terminal.backend().buffer().clone();
    let gutter_cell = buffer.cell((0, 5)).expect("cell should exist");
    assert_eq!(
        gutter_cell.style().fg,
        Some(crate::feat::theme::default_theme().border_unfocused)
    );
}

#[rstest::rstest]
fn unselected_ignored_pinned_entry_shows_context_color() {
    // Given a ChatLogElement with 2 entries: first ignored+pinned, second selected.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut().push_entry(
            ChatEntry::user("hello")
                .with_ignored(true)
                .with_pin(PinPosition::Top),
        );
        s.active_session_mut().push_entry(ChatEntry::user("world"));
        // push_entry auto-selects last (index 1). Entry 0 is unselected, ignored but pinned.
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the unselected, ignored+pinned entry's gutter shows the context included color
    // (effective inclusion: pinned overrides ignored).
    // 2 entries × 3 lines = 6, 4 blank above. Entry 0 at rows 4-6.
    let buffer = terminal.backend().buffer().clone();
    // The pin icon is on the first line (row 4), but the gutter character on row 5 also shows
    // the context color (non-pin lines use gutter_style, not pin_highlight_style).
    let gutter_cell = buffer.cell((0, 5)).expect("cell should exist");
    assert_eq!(
        gutter_cell.style().fg,
        Some(crate::feat::theme::default_theme().gutter_context_included)
    );
}

#[rstest::rstest]
fn selected_entry_gutter_is_dark_gray_when_unfocused() {
    // Given a ChatLogElement with a selected entry, sidebar focused.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s.active_session_mut().push_entry(ChatEntry::user("world"));
        s.active_session_mut().select_prev_entry(); // index 0
        s.frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the selected unfocused entry's gutter has context color fg and no bg.
    // Entry is not ignored, so fg is teal. Unfocused means no cursor bg.
    // 2 entries × 3 lines = 6, 4 blank above. Entry 0 content at row 5.
    let buffer = terminal.backend().buffer().clone();
    let gutter_cell = buffer.cell((0, 5)).expect("cell should exist");
    assert_eq!(
        gutter_cell.style().fg,
        Some(crate::feat::theme::default_theme().gutter_context_included)
    );
}

#[rstest::rstest]
fn selected_entry_gutter_is_dark_gray_when_input_focused() {
    // Given a ChatLogElement with a selected entry, input focused.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s.active_session_mut().push_entry(ChatEntry::user("world"));
        s.active_session_mut().select_prev_entry(); // index 0
        s.frontend.scope_push(FocusScope::Input);
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the selected unfocused entry's gutter has context color fg and no bg.
    // Entry is not ignored, so fg is teal. Input focus means no cursor bg.
    // 2 entries × 3 lines = 6, 4 blank above. Entry 0 content at row 5.
    let buffer = terminal.backend().buffer().clone();
    let gutter_cell = buffer.cell((0, 5)).expect("cell should exist");
    assert_eq!(
        gutter_cell.style().fg,
        Some(crate::feat::theme::default_theme().gutter_context_included)
    );
}

#[rstest::rstest]
fn render_stores_viewport_state() {
    // Given a ChatLogElement with entries.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s.active_session_mut().push_entry(ChatEntry::user("world"));
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then viewport state is stored in the session.
    let range = state.active_session().visible_entry_range();
    assert!(
        !range.is_empty(),
        "entry_line_ranges should be populated after render"
    );
}

#[rstest::rstest]
fn render_pinned_entry_shows_pin_in_gutter() {
    // Given a ChatLogElement with one pinned user entry.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut()
            .push_entry(ChatEntry::user("hello").with_pin(PinPosition::Top));
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the gutter contains the 📌 character.
    let buffer = terminal.backend().buffer().clone();
    let has_pin = (0..10).any(|row| {
        (0..2).any(|col| {
            buffer
                .cell((col, row))
                .is_some_and(|c| c.symbol() == "\u{1F4CC}")
        })
    });
    assert!(
        has_pin,
        "pinned entry should show \u{1F4CC} pin icon in gutter"
    );
}

#[rstest::rstest]
fn render_unpinned_entry_has_no_pin_icon() {
    // Given a ChatLogElement with one unpinned user entry.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then no cell in the buffer contains the 📌 character.
    let buffer = terminal.backend().buffer().clone();
    let has_pin = (0..10).any(|row| {
        (0..40).any(|col| {
            buffer
                .cell((col, row))
                .is_some_and(|c| c.symbol() == "\u{1F4CC}")
        })
    });
    assert!(
        !has_pin,
        "unpinned entry should not show \u{1F4CC} pin icon"
    );
}

#[rstest::rstest]
fn render_pinned_multi_line_entry_shows_exactly_one_pin() {
    // Given a ChatLogElement with one pinned multi-line user entry.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut().push_entry(
            ChatEntry::user("line one\nline two\nline three").with_pin(PinPosition::Top),
        );
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then exactly one pin icon appears in the gutter.
    let buffer = terminal.backend().buffer().clone();
    let pin_count = (0..10)
        .filter(|&row| {
            (0..2).any(|col| {
                buffer
                    .cell((col, row))
                    .is_some_and(|c| c.symbol() == "\u{1F4CC}")
            })
        })
        .count();
    assert_eq!(
        pin_count, 1,
        "multi-line pinned entry should show exactly one pin icon, found {pin_count}"
    );
}

#[rstest::rstest]
fn render_scroll_to_selected_keeps_entry_visible() {
    // Given a ChatLogElement with many entries where the first is selected
    // and the viewport is small enough that it would normally be scrolled off.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        // Add 20 entries (each 1 line).
        for i in 0..20 {
            s.active_session_mut()
                .push_entry(ChatEntry::user(format!("msg {i}")));
        }
        // Select the first entry (index 0).
        s.active_session_mut().select_next_entry(); // selects index 0
        s
    };

    let (mut terminal, area) = setup_term(40, 5); // 5-line viewport

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the selected entry's gutter col 1 (yellow bg) should be visible in the viewport.
    let buffer = terminal.backend().buffer().clone();
    let has_yellow_gutter = (0..5).any(|row| {
        buffer
            .cell((1, row))
            .is_some_and(|c| c.style().fg == Some(Color::Yellow))
    });
    assert!(
        has_yellow_gutter,
        "selected entry should be visible in viewport when scroll-to-selected is active"
    );
}

#[rstest::rstest]
fn render_thinking_entry_appears_above_assistant() {
    // Given a ChatLogElement with thinking then assistant entries.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut()
            .push_entry(ChatEntry::thinking("reasoning"));
        s.active_session_mut()
            .push_entry(ChatEntry::assistant("response"));
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the thinking entry appears above the assistant entry.
    // Thinking = 2 lines (pad + content), assistant = 3 lines (pad + content + pad).
    // Total = 5, 5 blank above. Thinking content at row 6, assistant content at row 8.
    let buffer = terminal.backend().buffer().clone();
    // Row 6 has the thinking content ("reasoning").
    let thinking_cell = buffer.cell((G, 6)).expect("cell should exist");
    assert_eq!(thinking_cell.symbol(), "r");
    // Row 8 has the assistant content ("response").
    let assistant_cell = buffer.cell((G, 8)).expect("cell should exist");
    assert_eq!(assistant_cell.symbol(), "r");
}

#[rstest::rstest]
fn render_pinned_selected_entry_gutter_has_focus_accent_bg() {
    // Given a ChatLogElement with one pinned user entry (auto-selected).
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.active_session_mut()
            .push_entry(ChatEntry::user("hello").with_pin(PinPosition::Top));
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the pinned entry's gutter pin icon has yellow bg (cursor).
    // Entry is 3 lines (pad + content + pad), starts at row 7 in 10-line viewport.
    // The pin icon appears on the first line of the entry (row 7).
    let buffer = terminal.backend().buffer().clone();
    let gutter_cell = buffer.cell((0, 7)).expect("cell should exist");
    assert_eq!(
        gutter_cell.style().bg,
        Some(Color::Yellow),
        "pinned selected entry gutter should have yellow background (cursor)"
    );
}

#[rstest::rstest]
fn render_pinned_unselected_entry_gutter_has_default_bg() {
    // Given a ChatLogElement with a pinned entry and an unpinned entry (unpinned selected).
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut()
            .push_entry(ChatEntry::user("pinned").with_pin(PinPosition::Top));
        s.active_session_mut()
            .push_entry(ChatEntry::user("unpinned"));
        // push_entry auto-selects last (index 1, unpinned).
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the pinned (unselected) entry's gutter col 0 has no yellow foreground.
    // 2 entries × 3 lines = 6, 4 blank above. Pinned entry (index 0) at rows 4-6.
    // Check row 5 (middle of pinned entry), not row 8 (which is the selected entry).
    let buffer = terminal.backend().buffer().clone();
    let gutter_cell = buffer.cell((1, 5)).expect("cell should exist");
    assert_ne!(
        gutter_cell.style().fg,
        Some(Color::Yellow),
        "pinned unselected entry gutter should have no background"
    );
}

#[rstest::rstest]
fn render_unpinned_selected_entry_gutter_col0_no_bg_col1_has_cursor_bg() {
    // Given a ChatLogElement with one unpinned user entry (auto-selected).
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the unpinned selected entry's gutter col 0 has context fg.
    // 1 entry × 3 lines = 3, 7 blank above. Entry at rows 7-9.
    let buffer = terminal.backend().buffer().clone();
    let gutter_col0 = buffer.cell((0, 9)).expect("cell should exist");
    assert_eq!(
        gutter_col0.style().fg,
        Some(crate::feat::theme::default_theme().gutter_context_included),
        "unpinned selected entry gutter col 0 should have context fg"
    );

    // And the gutter col 1 has yellow fg (cursor).
    let gutter_col1 = buffer.cell((1, 9)).expect("cell should exist");
    assert_eq!(
        gutter_col1.style().fg,
        Some(Color::Yellow),
        "unpinned selected entry gutter col 1 should have yellow foreground (cursor)"
    );
}

#[rstest::rstest]
fn render_pinned_selected_unfocused_entry_gutter_has_border_unfocused_bg() {
    // Given a ChatLogElement with one pinned entry selected, sidebar focused.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut()
            .push_entry(ChatEntry::user("hello").with_pin(PinPosition::Top));
        s.frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the pinned unfocused entry's gutter pin icon has context fg (not yellow).
    // The pin icon uses context fg (not cursor color) when unfocused.
    // 1 entry × 3 lines = 3, 7 blank above. Entry at rows 7-9, pin icon at row 7.
    let buffer = terminal.backend().buffer().clone();
    let gutter_cell = buffer.cell((0, 7)).expect("cell should exist");
    assert_ne!(
        gutter_cell.style().fg,
        Some(Color::Yellow),
        "pinned selected unfocused entry gutter should not have yellow foreground"
    );
}

#[rstest::rstest]
fn render_long_session_shows_last_entry_at_bottom() {
    // Given a ChatLogElement with many assistant entries containing word-wrapping text.
    // Assistant entries are not padded, so they wrap at word boundaries.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        for i in 0..20 {
            s.active_session_mut()
                .push_entry(ChatEntry::assistant(format!(
                    "This is message number {i} with some long words that will wrap"
                )));
        }
        s
    };

    // 30-wide, 10-tall viewport (content width = 28 after 2-char gutter).
    let (mut terminal, area) = setup_term(30, 10);

    // When rendering at bottom (auto-scroll).
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the last entry's text appears near the bottom of the buffer.
    // Each entry is 3 lines (pad + content + pad), so the bottom row is padding.
    // Check rows 8-9 for the content or padding.
    let buffer = terminal.backend().buffer().clone();
    let has_last_entry = (7..10).any(|row| {
        let row_text: String = (0..30)
            .filter_map(|x| buffer.cell((x, row)).map(|c| c.symbol().to_owned()))
            .collect();
        row_text.contains("wrap") || row_text.contains("will") || row_text.contains("19")
    });
    assert!(
        has_last_entry,
        "last entry's text should be visible near the bottom of the viewport"
    );
}

#[rstest::rstest]
fn render_scroll_to_bottom_shows_full_last_entry() {
    // Given a ChatLogElement with assistant entries containing word-wrapping text.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        for i in 0..15 {
            s.active_session_mut()
                .push_entry(ChatEntry::assistant(format!(
                    "This is message number {i} with some long words that will wrap"
                )));
        }
        // Simulate pressing G: scroll to bottom + select last entry.
        s.active_session_mut().scroll_to_bottom();
        let max = s.active_session().history().len() - 1;
        s.active_session_mut().set_selected_entry_index(max);
        s
    };

    let (mut terminal, area) = setup_term(30, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the last entry's content ("message number 14") is visible in the viewport.
    let buffer = terminal.backend().buffer().clone();
    let has_last_entry = (0..10).any(|row| {
        let row_text: String = (0..30)
            .filter_map(|x| buffer.cell((x, row)).map(|c| c.symbol().to_owned()))
            .collect();
        row_text.contains("14")
    });
    assert!(
        has_last_entry,
        "last entry (message number 14) should be visible after scroll to bottom"
    );
}

#[rstest::rstest]
fn render_scroll_to_selected_middle_entry_adjusts_viewport() {
    // Given a ChatLogElement with many entries where a middle entry is selected.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        // 30 entries, each with word-wrapping text.
        for i in 0..30 {
            s.active_session_mut()
                .push_entry(ChatEntry::assistant(format!(
                    "This is message number {i} with some long words that will wrap"
                )));
        }
        // Select entry 10 (middle of 30).
        s.active_session_mut().set_selected_entry_index(10);
        s
    };

    let (mut terminal, area) = setup_term(30, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the selected entry is visible (yellow gutter col 1 bg in viewport).
    let buffer = terminal.backend().buffer().clone();
    let has_yellow_gutter = (0..10).any(|row| {
        buffer
            .cell((1, row))
            .is_some_and(|c| c.style().fg == Some(Color::Yellow))
    });
    assert!(
        has_yellow_gutter,
        "selected middle entry should be visible in viewport after scroll-to-selected"
    );

    // And the selected entry's text ("message number 10") is visible.
    let has_entry_10 = (0..10).any(|row| {
        let row_text: String = (0..30)
            .filter_map(|x| buffer.cell((x, row)).map(|c| c.symbol().to_owned()))
            .collect();
        row_text.contains("10")
    });
    assert!(
        has_entry_10,
        "selected middle entry's text should be visible in viewport"
    );
}

#[rstest::rstest]
fn render_scroll_down_through_tall_entry_works() {
    // Given a tall entry (50 lines) in a small (10-line) viewport, scrolled to show
    // the middle of the entry.
    let mut element = ChatLogElement::new();
    let mut state = AppState::default_with_scope_focus();
    let long_text: String = (0..50)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    state
        .active_session_mut()
        .push_entry(ChatEntry::assistant(long_text));
    // First render to populate last_max_offset, then scroll.
    let (mut terminal, area) = setup_term(40, 10);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();
    // Now scroll up to show the middle of the tall entry.
    state.active_session_mut().scroll_up(20);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the viewport shows text from the middle of the entry, not the top.
    let buffer = terminal.backend().buffer().clone();
    let viewport_text: String = (0..10)
        .map(|row| {
            (0..40)
                .filter_map(|col| buffer.cell((col, row)).map(|c| c.symbol().to_owned()))
                .collect::<String>()
        })
        .collect();
    assert!(
        !viewport_text.contains("line 0"),
        "viewport should not show line 0 when scrolled to middle, got: {viewport_text}"
    );
}

#[rstest::rstest]
fn render_tall_entry_snaps_when_completely_below_viewport() {
    // Given a tall entry at the end and the viewport scrolled to the top,
    // with the tall entry selected.
    let mut element = ChatLogElement::new();
    let mut state = AppState::default_with_scope_focus();
    // Push 20 short entries to fill space.
    for i in 0..20 {
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant(format!("msg {i}")));
    }
    // Push a tall entry (50 lines).
    let long_text: String = (0..50)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    state
        .active_session_mut()
        .push_entry(ChatEntry::assistant(long_text));
    // push_entry auto-selects last entry (the tall one).

    let (mut terminal, area) = setup_term(40, 5);

    // First render to populate last_max_offset.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Scroll to top so the tall entry is completely below the viewport.
    state.active_session_mut().scroll_to_top();
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the renderer snaps to show the tall entry's start.
    let buffer = terminal.backend().buffer().clone();
    let viewport_text: String = (0..5)
        .map(|row| {
            (0..40)
                .filter_map(|col| buffer.cell((col, row)).map(|c| c.symbol().to_owned()))
                .collect::<String>()
        })
        .collect();
    assert!(
        viewport_text.contains("line 0"),
        "tall entry below viewport should snap to show its start, got: {viewport_text}"
    );
}

#[rstest::rstest]
fn virtualization_populates_cache_after_render() {
    // Given a ChatLogElement with many entries.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        for i in 0..30 {
            s.active_session_mut()
                .push_entry(ChatEntry::assistant(format!("msg {i}")));
        }
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the cache has entries for all 30 entries.
    assert_eq!(
        state.frontend.caches.entry_line_cache.read().len(),
        30,
        "cache should have entries for all 30 entries after render"
    );
}

#[rstest::rstest]
fn expand_collapse_invalidates_and_rerenders() {
    // Given a ChatLogElement with a long tool result entry.
    let mut element = ChatLogElement::new();
    let long_content: String = (0..20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let entry = ChatEntry::tool_result("call1", "bash", &long_content, ToolResultStatus::Success);
    let entry_id = entry.id.clone();
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().push_entry(entry);

    let (mut terminal, area) = setup_term(80, 30);

    // When rendering (truncated - max_lines=5 by default).
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the truncation indicator is visible in the buffer.
    let buffer = terminal.backend().buffer().clone();
    let has_more_lines = (0..30).any(|row| {
        let row_text: String = (2..80)
            .filter_map(|col| buffer.cell((col, row)).map(|c| c.symbol().to_owned()))
            .collect();
        row_text.contains("lines hidden above")
    });
    assert!(
        has_more_lines,
        "truncated tool result should show truncation indicator"
    );

    // When expanding the entry and re-rendering.
    state.active_session_mut().toggle_expand_entry(entry_id);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the expanded content shows all lines.
    let buffer2 = terminal.backend().buffer().clone();
    let has_line_19 = (0..30).any(|row| {
        let row_text: String = (2..80)
            .filter_map(|col| buffer2.cell((col, row)).map(|c| c.symbol().to_owned()))
            .collect();
        row_text.contains("line 19")
    });
    assert!(
        has_line_19,
        "expanded tool result should show all content including line 19"
    );
}

#[rstest::rstest]
fn resize_clears_cache_and_rerenders() {
    // Given a ChatLogElement rendered at width 40.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        for i in 0..5 {
            s.active_session_mut()
                .push_entry(ChatEntry::assistant(format!("message {i}")));
        }
        s
    };

    let (mut terminal, area) = setup_term(40, 10);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // When rendering at a different width (simulating resize).
    let (mut terminal2, area2) = setup_term(60, 10);
    terminal2
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area2, &ctx);
        })
        .unwrap();

    // Then the cache is still populated (re-populated at new width).
    assert_eq!(
        state.frontend.caches.entry_line_cache.read().len(),
        5,
        "cache should be re-populated after resize"
    );

    // And the last message is visible near the bottom.
    let buffer = terminal2.backend().buffer().clone();
    let has_last_message = (7..10).any(|row| {
        let row_text: String = (0..60)
            .filter_map(|x| buffer.cell((x, row)).map(|c| c.symbol().to_owned()))
            .collect();
        row_text.contains('4')
    });
    assert!(
        has_last_message,
        "last message should be visible after resize"
    );
}

#[rstest::rstest]
fn streaming_content_change_invalidates_cache() {
    // Given a ChatLogElement rendered during active streaming.
    let mut element = ChatLogElement::new();
    let (mut terminal, area) = setup_term(40, 10);

    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().begin_streaming();
    state
        .active_session_mut()
        .append_stream_token("initial", jiff::Timestamp::now())
        .expect("ok");

    // When rendering with initial streaming content.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    assert_eq!(
        state.frontend.caches.entry_line_cache.read().len(),
        1,
        "cache should have 1 entry"
    );

    // When more tokens arrive (content changes, fingerprint changes).
    state
        .active_session_mut()
        .append_stream_token(" + more text", jiff::Timestamp::now())
        .expect("ok");

    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the cache still has 1 entry (re-computed with new fingerprint).
    assert_eq!(
        state.frontend.caches.entry_line_cache.read().len(),
        1,
        "cache should have 1 entry after streaming token append"
    );

    // And the updated content is visible.
    let buffer = terminal.backend().buffer().clone();
    let has_more = (0..10).any(|row| {
        let row_text: String = (2..40)
            .filter_map(|col| buffer.cell((col, row)).map(|c| c.symbol().to_owned()))
            .collect();
        row_text.contains("more")
    });
    assert!(
        has_more,
        "updated content should be visible after streaming"
    );
}

#[rstest::rstest]
fn render_transient_entry_has_muted_text_color() {
    // Given a ChatLogElement with a transient entry.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut()
            .push_entry(ChatEntry::transient("Welcome to jinn!"));
        s
    };

    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the transient text appears with the theme text color.
    let buffer = terminal.backend().buffer().clone();
    let transient_cell = buffer.cell((G, 8)).expect("cell should exist");
    assert_eq!(transient_cell.symbol(), "W");
    assert_eq!(
        transient_cell.fg, state.frontend.theme.primary_text,
        "transient entry should use theme text color (from markdown renderer)"
    );
}

#[rstest::rstest]
fn render_auto_scrolls_jumped_compaction_into_view() {
    // Given a history taller than a 6-line viewport, with a compaction as the
    // FIRST entry and many user entries below it. The default viewport shows the
    // bottom (newest) entries, so the compaction is scrolled off the top.
    use crate::feat::chat_entry_selection::intent::handle_jump_prev_entry;

    let mut element = ChatLogElement::new();
    let mut state = normal_state();
    state
        .active_session_mut()
        .push_entry(compaction_entry("top-compaction"));
    let compaction_id = state.active_session().history()[0].id.clone();
    for n in 0..12 {
        state
            .active_session_mut()
            .push_entry(ChatEntry::user(format!("msg-{n}")));
    }

    let (mut terminal, area) = setup_term(40, 6);

    // Initial render: viewport defaults to the newest entries, so the compaction
    // (history index 0) is NOT in the visible range.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();
    let range_before = state.active_session().visible_entry_range();
    assert!(
        !range_before.contains(&0),
        "compaction at index 0 should be off-screen before the jump; range = {range_before:?}"
    );

    // When jumping to the previous compaction from the last entry (no selection
    // -> anchor on last entry; the prev jump lands on the only compaction at index 0).
    handle_jump_prev_entry(
        &mut state,
        crate::protocol::ChatEntry::is_compaction,
    );
    assert_eq!(
        state.active_session().selected_cursor_id(),
        Some(compaction_id),
        "prev jump must land on the compaction entry"
    );

    // Re-render: the viewport must auto-scroll so the jumped-to compaction is now visible.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();
    let range_after = state.active_session().visible_entry_range();
    assert!(
        range_after.contains(&0),
        "compaction at index 0 must be scrolled into view after the jump; range = {range_after:?}"
    );
}

/// Collect every rendered cell symbol into a single string (row-major),
/// so substring assertions can scan the whole viewport.
fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buffer.cell((x, y)).map_or("", |c| c.symbol()));
        }
        out.push('\n');
    }
    out
}

#[rstest::rstest]
fn render_annotation_entry_collapsed_by_default_shows_hint() {
    // Given a ChatLogElement with an annotation entry carrying one citation.
    use jinn_provider::UrlCitation;
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        s.active_session_mut()
            .push_entry(ChatEntry::annotation(vec![UrlCitation {
                url: "https://example.com/a".to_owned(),
                title: "Source A".to_owned(),
                content: None,
                start_index: None,
                end_index: None,
            }]));
        s
    };

    let (mut terminal, area) = setup_term(60, 10);

    // When rendering with no expand toggle.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the header and expand hint appear in the rendered output.
    let text = buffer_text(terminal.backend().buffer());
    assert!(
        text.contains("Sources (1)"),
        "collapsed header should render: {text:?}"
    );
    assert!(
        text.contains("(e to expand)"),
        "collapsed hint should render: {text:?}"
    );
    // And the citation title and URL are hidden.
    assert!(
        !text.contains("Source A"),
        "collapsed block should hide citation titles: {text:?}"
    );
    assert!(
        !text.contains("https://example.com/a"),
        "collapsed block should hide citation urls: {text:?}"
    );
}

#[rstest::rstest]
fn render_annotation_entry_expanded_shows_source_title_and_url() {
    // Given a ChatLogElement with an expanded annotation entry carrying one citation.
    use jinn_provider::UrlCitation;
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        let entry = ChatEntry::annotation(vec![UrlCitation {
            url: "https://example.com/a".to_owned(),
            title: "Source A".to_owned(),
            content: None,
            start_index: None,
            end_index: None,
        }]);
        let entry_id = entry.id.clone();
        s.active_session_mut().push_entry(entry);
        s.active_session_mut().toggle_expand_entry(entry_id);
        s
    };

    let (mut terminal, area) = setup_term(60, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then both the citation title and its URL appear in the rendered output.
    let text = buffer_text(terminal.backend().buffer());
    assert!(
        text.contains("Source A"),
        "citation title should render: {text:?}"
    );
    assert!(
        text.contains("https://example.com/a"),
        "citation url should render: {text:?}"
    );
    // And the expand hint is gone.
    assert!(
        !text.contains("e to expand"),
        "expanded block should not show the hint: {text:?}"
    );
}

// ---------------------------------------------------------------------------
// Subagent waiting line
// ---------------------------------------------------------------------------

/// Seeds a `task` tool call entry (linked to `child_id` when given) plus an
/// optional child session in the given phase.
fn task_waiting_fixture(
    child_id: Option<crate::protocol::SessionId>,
    child_phase: Option<crate::feat::session::phase_machine::PhaseKind>,
) -> AppState {
    use crate::protocol::ChatEntryKind;
    use jinn_tools_msg::TASK_TOOL_NAME;

    let mut state = AppState::default_with_scope_focus();
    let call_id = "tc_task_render";
    let entry = ChatEntry::tool_call(call_id, TASK_TOOL_NAME, r#"{"prompt": "hi"}"#);
    let entry = {
        let mut e = entry;
        if let Some(child) = &child_id
            && let ChatEntryKind::ToolCall { child_session, .. } = &mut e.kind
        {
            *child_session = Some(child.clone());
        }
        e
    };
    state.active_session_mut().push_entry(entry);
    if let (Some(child), Some(phase)) = (child_id, child_phase) {
        let child_session = state.session.get_or_create(&child);
        match phase {
            crate::feat::session::phase_machine::PhaseKind::Sending => {
                child_session.begin_sending();
            }
            crate::feat::session::phase_machine::PhaseKind::Streaming => {
                child_session.begin_sending();
                child_session.begin_streaming();
            }
            _ => {}
        }
    }
    state
}

fn buffer_contains(buffer: &ratatui::buffer::Buffer, needle: &str) -> bool {
    buffer_text(buffer).contains(needle)
}

#[rstest::rstest]
fn waiting_line_renders_for_pending_task_call_with_running_child() {
    use crate::feat::session::phase_machine::PhaseKind;

    // Given a pending task call linked to an in-memory child in Sending phase.
    let mut element = ChatLogElement::new();
    let child_id = crate::protocol::SessionId::new();
    let state = task_waiting_fixture(Some(child_id), Some(PhaseKind::Sending));

    let (mut terminal, area) = setup_term(80, 12);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the waiting line is visible.
    let buffer = terminal.backend().buffer();
    assert!(
        buffer_contains(buffer, "Waiting for subagent session to complete"),
        "waiting line should render: {buffer:?}"
    );
}

#[rstest::rstest]
fn waiting_line_absent_for_non_task_tool_call() {
    // Given a pending non-task tool call entry.
    let mut element = ChatLogElement::new();
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().push_entry(ChatEntry::tool_call(
        "tc_read",
        "read",
        r#"{"path": "a.rs"}"#,
    ));

    let (mut terminal, area) = setup_term(80, 12);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then no waiting line is rendered.
    let buffer = terminal.backend().buffer();
    assert!(
        !buffer_contains(buffer, "Waiting for subagent session"),
        "non-task call should not show a waiting line: {buffer:?}"
    );
}

#[rstest::rstest]
fn waiting_line_absent_when_task_call_has_paired_result() {
    use jinn_tools_msg::TASK_TOOL_NAME;

    // Given a task call with its completed (paired) result.
    let mut element = ChatLogElement::new();
    let mut state = AppState::default_with_scope_focus();
    {
        let s = state.active_session_mut();
        s.push_entry(ChatEntry::tool_call("tc_done", TASK_TOOL_NAME, "{}"));
        s.push_entry(ChatEntry::tool_result(
            "tc_done",
            TASK_TOOL_NAME,
            "done",
            ToolResultStatus::Success,
        ));
    }

    let (mut terminal, area) = setup_term(80, 12);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then no waiting line is rendered.
    let buffer = terminal.backend().buffer();
    assert!(
        !buffer_contains(buffer, "Waiting for subagent session"),
        "completed task call should not show a waiting line: {buffer:?}"
    );
}

#[rstest::rstest]
fn waiting_line_absent_when_child_not_in_memory() {
    use jinn_tools_msg::TASK_TOOL_NAME;

    // Given a linked task call whose child session is not loaded.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = AppState::default_with_scope_focus();
        let entry = ChatEntry::tool_call("tc_orphan", TASK_TOOL_NAME, "{}");
        let entry = {
            use crate::protocol::ChatEntryKind;
            let mut e = entry;
            if let ChatEntryKind::ToolCall { child_session, .. } = &mut e.kind {
                *child_session = Some(crate::protocol::SessionId::new());
            }
            e
        };
        s.active_session_mut().push_entry(entry);
        s
    };

    let (mut terminal, area) = setup_term(80, 12);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then no waiting line is rendered.
    let buffer = terminal.backend().buffer();
    assert!(
        !buffer_contains(buffer, "Waiting for subagent session"),
        "unloaded child should not show a waiting line: {buffer:?}"
    );
}

#[rstest::rstest]
fn waiting_line_disappears_when_child_finishes_without_manual_invalidation() {
    // Given a rendered pending task call whose linked child is running.
    let mut element = ChatLogElement::new();
    let child_id = crate::protocol::SessionId::new();
    let mut state = task_waiting_fixture(
        Some(child_id.clone()),
        Some(crate::feat::session::phase_machine::PhaseKind::Streaming),
    );

    let (mut terminal, area) = setup_term(80, 12);

    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();
    assert!(
        buffer_contains(terminal.backend().buffer(), "Waiting for subagent session"),
        "waiting line should render while child streams"
    );

    // When the child finishes (Idle) and the render re-runs with no cache
    // invalidation.
    let child = state.session.get_mut(&child_id).expect("child");
    child.finish_streaming(false, jiff::Timestamp::now());
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the waiting line is gone — the render variant change alone
    // invalidated the cached lines.
    let buffer = terminal.backend().buffer();
    assert!(
        !buffer_contains(buffer, "Waiting for subagent session"),
        "waiting line should disappear once the child is Idle: {buffer:?}"
    );
}

// ---------------------------------------------------------------------------
// Subagent block
// ---------------------------------------------------------------------------

/// Every cell of a rendered row, from the content column onward.
fn content_row(
    buffer: &ratatui::buffer::Buffer,
    area_x: u16,
    y: u16,
) -> Vec<ratatui::buffer::Cell> {
    (area_x..buffer.area.width)
        .filter_map(|x| buffer.cell((x, y)).cloned())
        .collect()
}

#[rstest::rstest]
fn task_call_entry_renders_on_subagent_block() {
    use jinn_tools_msg::TASK_TOOL_NAME;

    // Given a session containing only a pending task call.
    let mut element = ChatLogElement::new();
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .push_entry(ChatEntry::tool_call("tc_block", TASK_TOOL_NAME, "{}"));
    let theme = crate::feat::theme::default_theme();

    let (mut terminal, area) = setup_term(80, 12);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the call row is fully painted in the subagent background across
    // the content column.
    let buffer = terminal.backend().buffer().clone();
    let content_x = area.x + GUTTER_WIDTH;
    let row_text = |y: u16| -> String {
        content_row(&buffer, content_x, y)
            .iter()
            .map(|c| c.symbol().to_owned())
            .collect()
    };
    let call_y = (area.y..area.bottom())
        .find(|&y| row_text(y).contains("task"))
        .expect("task call text should render");
    let row = content_row(&buffer, content_x, call_y);
    assert!(
        row.iter().all(|c| c.style().bg == Some(theme.subagent_bg)),
        "task call row should be fully on subagent_bg"
    );
}

#[rstest::rstest]
fn non_task_call_entry_does_not_use_subagent_block() {
    // Given a session containing a non-task tool call.
    let mut element = ChatLogElement::new();
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().push_entry(ChatEntry::tool_call(
        "tc_plain",
        "read",
        r#"{"path":"a.rs"}"#,
    ));
    let theme = crate::feat::theme::default_theme();

    let (mut terminal, area) = setup_term(80, 12);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then no row uses the subagent background.
    let buffer = terminal.backend().buffer();
    let content_x = area.x + GUTTER_WIDTH;
    let uses_block = (area.y..area.bottom()).any(|y| {
        content_row(buffer, content_x, y)
            .iter()
            .any(|c| c.style().bg == Some(theme.subagent_bg))
    });
    assert!(!uses_block, "non-task call should not use subagent_bg");
}

#[rstest::rstest]
fn completed_task_result_shows_finished_status_row() {
    use jinn_tools_msg::TASK_TOOL_NAME;

    // Given a task call with its completed success result.
    let mut element = ChatLogElement::new();
    let mut state = AppState::default_with_scope_focus();
    {
        let s = state.active_session_mut();
        s.push_entry(ChatEntry::tool_call("tc_status", TASK_TOOL_NAME, "{}"));
        s.push_entry(ChatEntry::tool_result(
            "tc_status",
            TASK_TOOL_NAME,
            "done",
            ToolResultStatus::Success,
        ));
    }
    let theme = crate::feat::theme::default_theme();

    let (mut terminal, area) = setup_term(80, 12);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = crate::common::overlay_views::OverlayViews::new();
            let ctx = RenderCtx::new(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the buffer contains the "Subagent task finished" outcome row,
    // white on the success background.
    let buffer = terminal.backend().buffer().clone();
    let content_x = area.x + GUTTER_WIDTH;
    let status_y = (area.y..area.bottom())
        .find(|&y| {
            let text: String = content_row(&buffer, content_x, y)
                .iter()
                .map(|c| c.symbol().to_owned())
                .collect();
            text.contains("Subagent task finished")
        })
        .unwrap_or_else(|| panic!("finished status row should render"));
    let row = content_row(&buffer, content_x, status_y);
    assert!(
        row.iter()
            .filter(|c| !c.symbol().trim().is_empty())
            .all(|c| c.style().bg == Some(theme.tool_success_bg)
                && c.style().fg == Some(ratatui::style::Color::White)),
        "status row should be white on success bg"
    );
}
