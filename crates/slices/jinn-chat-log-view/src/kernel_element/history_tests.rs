#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use crate::chat_log::GUTTER_WIDTH;
use crate::kernel_element::history::{ChatLogElement, STREAM_RENDER_INTERVAL};
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::render_ctx::RenderCtx;
use jinn_kernel::common::ui_element::UiElement;
use jinn_kernel::protocol::ToolResultStatus;
use jinn_kernel::protocol::{ChatEntry, ChatEntryId, PinPosition};
use jinn_slices::FocusScope;
use jinn_testutil::setup_term;
use ratatui::layout::Rect;
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
    use jinn_kernel::protocol::{ChatEntryId, ChatEntryKind};
    use jinn_kernel::protocol::{ContextOverride, EntryTiming};
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the selected entry's gutter col 0 has teal fg.
    // 2 entries × 3 lines = 6, 4 blank above. Entry 0 at rows 4-6.
    let buffer = terminal.backend().buffer().clone();
    let gutter_col0 = buffer.cell((0, 5)).expect("cell should exist");
    assert_eq!(
        gutter_col0.style().fg,
        Some(jinn_theme::default_theme().gutter_context_included)
    );

    // And the selected entry's gutter col 1 has yellow fg (cursor).
    let gutter_col1 = buffer.cell((1, 5)).expect("cell should exist");
    assert_eq!(gutter_col1.style().fg, Some(Color::Yellow));

    // And the unselected entry's gutter col 0 has context fg.
    let unselected_col0 = buffer.cell((0, 8)).expect("cell should exist");
    assert_eq!(
        unselected_col0.style().fg,
        Some(jinn_theme::default_theme().gutter_context_included)
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the unselected, not-ignored entry's gutter has the context included color.
    // 2 entries × 3 lines = 6, 4 blank above. Entry 0 at rows 4-6.
    let buffer = terminal.backend().buffer().clone();
    let gutter_cell = buffer.cell((0, 5)).expect("cell should exist");
    assert_eq!(
        gutter_cell.style().fg,
        Some(jinn_theme::default_theme().gutter_context_included)
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the unselected, ignored entry's gutter has the border_unfocused color.
    // 2 entries × 3 lines = 6, 4 blank above. Entry 0 at rows 4-6.
    let buffer = terminal.backend().buffer().clone();
    let gutter_cell = buffer.cell((0, 5)).expect("cell should exist");
    assert_eq!(
        gutter_cell.style().fg,
        Some(jinn_theme::default_theme().border_unfocused)
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
        Some(jinn_theme::default_theme().gutter_context_included)
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
        Some(jinn_theme::default_theme().gutter_context_included)
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
        Some(jinn_theme::default_theme().gutter_context_included)
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the unpinned selected entry's gutter col 0 has context fg.
    // 1 entry × 3 lines = 3, 7 blank above. Entry at rows 7-9.
    let buffer = terminal.backend().buffer().clone();
    let gutter_col0 = buffer.cell((0, 9)).expect("cell should exist");
    assert_eq!(
        gutter_col0.style().fg,
        Some(jinn_theme::default_theme().gutter_context_included),
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();
    // Now scroll up to show the middle of the tall entry.
    state.active_session_mut().scroll_up(20);

    // When rendering again.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Scroll to top so the tall entry is completely below the viewport.
    state.active_session_mut().scroll_to_top();

    // When rendering again.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the cache has entries for all 30 entries.
    assert_eq!(
        state
            .frontend
            .line_cache_cell()
            .expect("catalog registered the line-cache cell")
            .read()
            .len(),
        30,
        "cache should have entries for all 30 entries after render"
    );
}

/// A state holding one long tool-result entry (20 lines) plus the id needed
/// to toggle it. Rendering it at 80x30 truncates it to 5 lines.
fn long_tool_result_state() -> (AppState, jinn_kernel::protocol::ChatEntryId) {
    let long_content: String = (0..20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let entry = ChatEntry::tool_result("call1", "bash", &long_content, ToolResultStatus::Success);
    let entry_id = entry.id.clone();
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().push_entry(entry);
    (state, entry_id)
}

/// True when any row of a 80-wide, 30-tall buffer contains `needle`.
fn buffer_contains_rows(buffer: &ratatui::buffer::Buffer, needle: &str) -> bool {
    (0..30).any(|row| {
        let row_text: String = (2..80)
            .filter_map(|col| buffer.cell((col, row)).map(|c| c.symbol().to_owned()))
            .collect();
        row_text.contains(needle)
    })
}

#[rstest::rstest]
fn collapsed_tool_result_renders_truncation_indicator() {
    // Given a ChatLogElement with a long, collapsed tool result entry.
    let mut element = ChatLogElement::new();
    let (state, _entry_id) = long_tool_result_state();
    let (mut terminal, area) = setup_term(80, 30);

    // When rendering (truncated — max_lines=5 by default).
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the truncation indicator is visible in the buffer.
    let buffer = terminal.backend().buffer().clone();
    assert!(
        buffer_contains_rows(&buffer, "lines hidden above"),
        "truncated tool result should show truncation indicator"
    );
}

#[rstest::rstest]
fn expanded_tool_result_renders_all_lines() {
    // Given a ChatLogElement with a long tool result entry that is expanded.
    let mut element = ChatLogElement::new();
    let (mut state, entry_id) = long_tool_result_state();
    state.active_session_mut().toggle_expand_entry(entry_id);
    let (mut terminal, area) = setup_term(80, 30);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the content beyond the collapsed cutoff is visible in the buffer.
    let buffer = terminal.backend().buffer().clone();
    assert!(
        buffer_contains_rows(&buffer, "line 19"),
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // When rendering at a different width (simulating resize).
    let (mut terminal2, area2) = setup_term(60, 10);
    terminal2
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area2, &ctx);
        })
        .unwrap();

    // Then the cache is still populated (re-populated at new width).
    assert_eq!(
        state
            .frontend
            .line_cache_cell()
            .expect("catalog registered the line-cache cell")
            .read()
            .len(),
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

/// How many full content fingerprints the shared line cache has computed.
fn cache_fingerprint_count(state: &AppState) -> u64 {
    state
        .frontend
        .line_cache_cell()
        .expect("catalog registered the line-cache cell")
        .read()
        .fingerprint_computations()
}

/// A state mid-stream with `initial` already appended as the active entry.
fn streaming_state() -> AppState {
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().begin_streaming();
    state
        .active_session_mut()
        .append_stream_token("initial", jiff::Timestamp::now())
        .expect("ok");
    state
}

#[rstest::rstest]
fn initial_stream_render_populates_one_cache_entry() {
    // Given a ChatLogElement rendering an active stream with one token.
    let mut element = ChatLogElement::new();
    let state = streaming_state();
    let (mut terminal, area) = setup_term(40, 10);

    // When rendering with initial streaming content.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the cache holds the single streamed entry.
    assert_eq!(
        state
            .frontend
            .line_cache_cell()
            .expect("catalog registered the line-cache cell")
            .read()
            .len(),
        1,
        "cache should have 1 entry"
    );
}

#[rstest::rstest]
fn streaming_token_append_keeps_one_cache_entry() {
    // Given a ChatLogElement already rendered a stream with one token.
    let mut element = ChatLogElement::new();
    let mut state = streaming_state();
    let (mut terminal, area) = setup_term(40, 10);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // When more tokens arrive (content changes, fingerprint changes).
    state
        .active_session_mut()
        .append_stream_token(" + more text", jiff::Timestamp::now())
        .expect("ok");
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the cache still has 1 entry (re-computed with new fingerprint).
    assert_eq!(
        state
            .frontend
            .line_cache_cell()
            .expect("catalog registered the line-cache cell")
            .read()
            .len(),
        1,
        "cache should have 1 entry after streaming token append"
    );
}

#[rstest::rstest]
fn streaming_token_append_renders_updated_content() {
    // Given a ChatLogElement already rendered a stream with one token.
    let mut element = ChatLogElement::new();
    let mut state = streaming_state();
    let (mut terminal, area) = setup_term(40, 10);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // When more tokens arrive and the stream render interval has elapsed.
    state
        .active_session_mut()
        .append_stream_token(" + more text", jiff::Timestamp::now())
        .expect("ok");
    std::thread::sleep(STREAM_RENDER_INTERVAL);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the updated content is visible.
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
fn a_throttled_frame_reuses_the_previous_rendered_lines() {
    // Given a ChatLogElement that rendered a stream one frame ago.
    let mut element = ChatLogElement::new();
    let mut state = streaming_state();
    let (mut terminal, area) = setup_term(40, 10);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();
    let before = cache_fingerprint_count(&state);

    // When a token arrives and the log is re-rendered within the interval.
    state
        .active_session_mut()
        .append_stream_token(" + more text", jiff::Timestamp::now())
        .expect("ok");
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the frame did no work for the entry: the throttled path returns
    // before the cache probe, so no fingerprint was hashed.
    assert_eq!(
        cache_fingerprint_count(&state),
        before,
        "a throttled frame should not re-hash the streaming entry"
    );
}

#[rstest::rstest]
fn a_throttled_frame_still_paints_the_streaming_entry() {
    // Given a ChatLogElement that rendered a stream one frame ago.
    let mut element = ChatLogElement::new();
    let mut state = streaming_state();
    let (mut terminal, area) = setup_term(40, 10);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // When a throttled frame is drawn.
    state
        .active_session_mut()
        .append_stream_token(" + more text", jiff::Timestamp::now())
        .expect("ok");
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the entry is still on screen rather than blanked — suppression
    // reuses the previous lines, it never drops them.
    let buffer = terminal.backend().buffer().clone();
    let has_initial = (0..10).any(|row| {
        let row_text: String = (2..40)
            .filter_map(|col| buffer.cell((col, row)).map(|c| c.symbol().to_owned()))
            .collect();
        row_text.contains("initial")
    });
    assert!(
        has_initial,
        "a throttled frame must reuse the previous lines, not blank the entry"
    );
}

#[rstest::rstest]
fn a_settled_entry_is_never_throttled() {
    // Given a ChatLogElement rendering history with nothing streaming.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.active_session_mut()
            .push_entry(ChatEntry::assistant("settled text"));
        s
    };
    let (mut terminal, area) = setup_term(40, 10);

    // When the same settled content is rendered repeatedly with no interval.
    for _ in 0..3 {
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_slices::OverlayViews::new();
                let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
                element.render(frame, area, &ctx);
            })
            .unwrap();
    }

    // Then it is painted on the very first frame, with no prior render to reuse.
    let buffer = terminal.backend().buffer().clone();
    let has_text = (0..10).any(|row| {
        let row_text: String = (2..40)
            .filter_map(|col| buffer.cell((col, row)).map(|c| c.symbol().to_owned()))
            .collect();
        row_text.contains("settled")
    });
    assert!(has_text, "a settled entry must render immediately");
}

#[rstest::rstest]
fn the_stream_render_interval_elapses_between_frames() {
    // Given an element that rendered the streaming entry just now.
    let mut element = ChatLogElement::new();
    let state = streaming_state();
    let (mut terminal, area) = setup_term(40, 10);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // When the render interval elapses.
    std::thread::sleep(STREAM_RENDER_INTERVAL);

    // Then the entry is due a render again.
    assert!(
        element.stream_render_due(),
        "the throttle must reopen once the interval has passed"
    );
}

#[rstest::rstest]
fn the_stream_render_is_due_before_any_render_has_happened() {
    // Given a fresh element that has never rendered.
    let element = ChatLogElement::new();

    // When asking whether a stream render is due.
    let due = element.stream_render_due();

    // Then it is, so the first frame is never suppressed.
    assert!(due, "the first streaming frame must always render");
}

#[rstest::rstest]
fn reusable_lines_are_withheld_for_a_different_entry() {
    // Given an element holding a previous render of one entry.
    let element = ChatLogElement::new();
    let other = ChatEntryId::new();

    // When asking whether those lines can serve a different entry.
    let reusable = element.reusable_stream_lines(&other);

    // Then nothing is offered, so a different entry renders normally.
    assert!(
        reusable.is_none(),
        "one entry's lines must never be painted as another's"
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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

/// A history taller than a 6-line viewport whose FIRST entry is a compaction,
/// followed by 12 user entries, with visual state ready to render. Returns
/// the state and the compaction's entry id.
fn history_leading_with_compaction() -> (AppState, jinn_kernel::protocol::ChatEntryId) {
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
    (state, compaction_id)
}

#[rstest::rstest]
fn compaction_is_off_screen_until_a_jump_reaches_it() {
    // Given a history taller than a 6-line viewport, with a compaction as the
    // FIRST entry and many user entries below it. The default viewport shows the
    // bottom (newest) entries, so the compaction is scrolled off the top.
    use crate::chat_entry_selection::intent::handle_jump_prev_entry;
    let mut element = ChatLogElement::new();
    let (mut state, compaction_id) = history_leading_with_compaction();
    let (mut terminal, area) = setup_term(40, 6);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();
    let range_before = state.active_session().visible_entry_range();

    // When jumping to the previous compaction from the last entry (no selection
    // -> anchor on last entry; the prev jump lands on the only compaction at index 0).
    handle_jump_prev_entry(&mut state, jinn_kernel::protocol::ChatEntry::is_compaction);

    // Then the compaction at index 0 was off-screen before the jump.
    assert!(
        !range_before.contains(&0),
        "compaction at index 0 should be off-screen before the jump; range = {range_before:?}"
    );
    // And the jump lands on that compaction.
    assert_eq!(
        state.active_session().selected_cursor_id(),
        Some(compaction_id),
        "prev jump must land on the compaction entry"
    );
}

#[rstest::rstest]
fn render_auto_scrolls_jumped_compaction_into_view() {
    // Given a history leading with an off-screen compaction that the previous
    // -compaction jump has just selected.
    use crate::chat_entry_selection::intent::handle_jump_prev_entry;
    let mut element = ChatLogElement::new();
    let (mut state, _compaction_id) = history_leading_with_compaction();
    let (mut terminal, area) = setup_term(40, 6);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();
    handle_jump_prev_entry(&mut state, jinn_kernel::protocol::ChatEntry::is_compaction);

    // When re-rendering after the jump.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the viewport auto-scrolled so the compaction is now visible.
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
    child_id: Option<jinn_core_types::SessionId>,
    child_phase: Option<jinn_session_msg::PhaseKind>,
) -> AppState {
    use jinn_kernel::protocol::ChatEntryKind;
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
            jinn_session_msg::PhaseKind::Sending => {
                child_session.begin_sending();
            }
            jinn_session_msg::PhaseKind::Streaming => {
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
    use jinn_session_msg::PhaseKind;

    // Given a pending task call linked to an in-memory child in Sending phase.
    let mut element = ChatLogElement::new();
    let child_id = jinn_core_types::SessionId::new();
    let state = task_waiting_fixture(Some(child_id), Some(PhaseKind::Sending));

    let (mut terminal, area) = setup_term(80, 12);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            use jinn_kernel::protocol::ChatEntryKind;
            let mut e = entry;
            if let ChatEntryKind::ToolCall { child_session, .. } = &mut e.kind {
                *child_session = Some(jinn_core_types::SessionId::new());
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
    let child_id = jinn_core_types::SessionId::new();
    let mut state = task_waiting_fixture(
        Some(child_id.clone()),
        Some(jinn_session_msg::PhaseKind::Streaming),
    );

    let (mut terminal, area) = setup_term(80, 12);

    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
    let theme = jinn_theme::default_theme();

    let (mut terminal, area) = setup_term(80, 12);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
    let theme = jinn_theme::default_theme();

    let (mut terminal, area) = setup_term(80, 12);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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
    let theme = jinn_theme::default_theme();

    let (mut terminal, area) = setup_term(80, 12);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
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

// ---------------------------------------------------------------------------
// Streaming tool-call variant lookup
// ---------------------------------------------------------------------------

/// Renders `state` through a chat log element and returns each content row
/// joined into a string, so tests can assert on visible text.
fn rendered_content_rows(state: &AppState, width: u16, height: u16) -> Vec<String> {
    let mut element = ChatLogElement::new();
    let (mut terminal, area) = setup_term(width, height);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    (area.y..area.bottom())
        .map(|row| {
            content_row(&buffer, area.x + G, row)
                .iter()
                .map(|c| c.symbol().to_owned())
                .collect()
        })
        .collect()
}

#[rstest::rstest]
fn streaming_tool_call_ids_contains_only_the_streaming_entry() {
    // Given a session with many completed tool calls and one streaming.
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().begin_streaming();
    for i in 0..200 {
        state.active_session_mut().push_entry(ChatEntry::tool_call(
            format!("tc_done_{i}"),
            "read_file",
            "{}",
        ));
    }
    state
        .active_session_mut()
        .begin_tool_call(200, "tc_live", "read_file", jiff::Timestamp::now());

    // When reading the streaming tool-call ids.
    let ids = state.active_session().streaming_tool_call_ids();

    // Then exactly one id is reported, and it is the streaming entry's.
    assert_eq!(ids.len(), 1);
    let streaming_entry = state
        .active_session()
        .history()
        .last()
        .expect("history should not be empty");
    assert!(ids.contains(&streaming_entry.id));
}

#[rstest::rstest]
fn streaming_tool_call_ids_is_empty_when_not_streaming() {
    // Given a session with a tool call but no active streaming phase.
    let mut state = AppState::default_with_scope_focus();
    state
        .active_session_mut()
        .push_entry(ChatEntry::tool_call("tc_1", "read_file", "{}"));

    // When reading the streaming tool-call ids.
    let ids = state.active_session().streaming_tool_call_ids();

    // Then the set is empty.
    assert!(ids.is_empty());
}

#[rstest::rstest]
fn streaming_tool_call_renders_multiline_arguments() {
    // Given a session with a completed tool call and a streaming one whose
    // arguments contain an escaped newline.
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().begin_streaming();
    state
        .active_session_mut()
        .push_entry(ChatEntry::tool_call("tc_done", "read_file", "{}"));
    state
        .active_session_mut()
        .begin_tool_call(1, "tc_live", "read_file", jiff::Timestamp::now());
    state
        .active_session_mut()
        .append_tool_call_delta(1, r#"line_one\nline_two"#)
        .expect("append delta");

    // When rendering.
    let rows = rendered_content_rows(&state, 40, 10);

    // Then the arguments split across two rows, which only the streaming
    // variant does — the collapsed variant renders a single line.
    assert!(
        rows.iter().any(|r| r.contains("line_one")),
        "streaming tool call should render the first argument line"
    );
    assert!(
        rows.iter().any(|r| r.contains("line_two")),
        "streaming tool call should render the second argument line"
    );
}

#[rstest::rstest]
#[case("task")]
#[case("write")]
#[case("bash")]
fn tool_call_arguments_render_after_a_phase_transition_mid_stream(#[case] tool: &str) {
    // Given a turn that opened with a tool call: the call is registered while
    // the session is still Sending, and its first argument delta has landed.
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().begin_sending();
    state
        .active_session_mut()
        .begin_tool_call(0, "tc_live", tool, jiff::Timestamp::now());
    state
        .active_session_mut()
        .append_tool_call_delta(0, r#"line_one\n"#)
        .expect("first delta should land on a registered call");

    // When a prose token drives Sending -> Streaming between two deltas of the
    // same call, and the rest of the arguments arrive.
    state.active_session_mut().begin_streaming();
    state
        .active_session_mut()
        .append_tool_call_delta(0, r#"line_two\n"#)
        .expect("second delta should survive the transition");
    state
        .active_session_mut()
        .append_tool_call_delta(0, r#"line_three"#)
        .expect("third delta should survive the transition");

    // Then the chat log renders the arguments that arrived after the transition.
    let rows = rendered_content_rows(&state, 60, 20);

    assert!(
        rows.iter().any(|r| r.contains("line_two")),
        "the delta arriving after the phase transition should render; got {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("line_three")),
        "every later delta should render; got {rows:?}"
    );
}

#[rstest::rstest]
fn completed_tool_call_renders_collapsed_after_streaming_finishes() {
    // Given a session that streamed a tool call and has since finished.
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().begin_streaming();
    state
        .active_session_mut()
        .begin_tool_call(0, "tc_live", "read_file", jiff::Timestamp::now());
    state
        .active_session_mut()
        .append_tool_call_delta(0, r#"line_one\nline_two"#)
        .expect("append delta");
    state
        .active_session_mut()
        .finish_streaming(true, jiff::Timestamp::now());

    // When rendering.
    let rows = rendered_content_rows(&state, 40, 10);

    // Then the entry is collapsed onto a single row, so the two argument
    // lines no longer render as separate rows.
    let first = rows
        .iter()
        .position(|r| r.contains("line_one"))
        .unwrap_or_else(|| panic!("tool call should still render its arguments"));
    let second = rows
        .iter()
        .position(|r| r.contains("line_two"))
        .unwrap_or_else(|| panic!("tool call arguments should still be visible"));
    assert_eq!(
        first, second,
        "a finished tool call collapses to one row, not two"
    );
}

#[rstest::rstest]
fn streaming_tool_call_renders_streaming_variant_after_many_completed_calls() {
    // Given a session where 150 completed tool calls precede a streaming one.
    // This guards the index-to-id resolution behind the streaming snapshot:
    // a wrong index would pick the wrong entry and render the wrong variant.
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().begin_streaming();
    for i in 0..150 {
        state.active_session_mut().push_entry(ChatEntry::tool_call(
            format!("tc_done_{i}"),
            "read_file",
            "{}",
        ));
    }
    state
        .active_session_mut()
        .begin_tool_call(150, "tc_live", "read_file", jiff::Timestamp::now());
    state
        .active_session_mut()
        .append_tool_call_delta(150, r#"alpha\nbeta"#)
        .expect("append delta");

    // When rendering.
    let rows = rendered_content_rows(&state, 60, 20);

    // Then the streaming entry renders as a multi-line block.
    let alpha_row = rows
        .iter()
        .position(|r| r.contains("alpha"))
        .unwrap_or_else(|| panic!("streaming entry should render its arguments"));
    let beta_row = rows
        .iter()
        .position(|r| r.contains("beta"))
        .unwrap_or_else(|| panic!("streaming entry should render its second line"));
    assert_eq!(
        beta_row,
        alpha_row + 1,
        "the streaming entry's arguments should be adjacent lines"
    );
}

#[rstest::rstest]
fn large_session_frame_does_not_refingerprint_unchanged_entries() {
    // Given a session holding many large tool results, rendered once to warm
    // the line cache.
    let mut state = AppState::default_with_scope_focus();
    for i in 0..120 {
        state
            .active_session_mut()
            .push_entry(ChatEntry::tool_result(
                format!("tr_{i}"),
                "bash",
                format!("{} output line\n", "x".repeat(2_000)),
                ToolResultStatus::Success,
            ));
    }
    let (mut terminal, area) = setup_term(80, 24);
    let mut element = ChatLogElement::new();
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    let after_warmup = state
        .frontend
        .line_cache_cell()
        .expect("catalog registered the line-cache cell")
        .read()
        .fingerprint_computations();

    // When redrawing several times with nothing changed.
    for _ in 0..5 {
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_slices::OverlayViews::new();
                let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
                element.render(frame, area, &ctx);
            })
            .unwrap();
    }

    // Then no further full content fingerprints were computed.
    let after_redraws = state
        .frontend
        .line_cache_cell()
        .expect("catalog registered the line-cache cell")
        .read()
        .fingerprint_computations();
    assert_eq!(
        after_redraws, after_warmup,
        "redrawing an unchanged session should not rehash entry content"
    );
    assert!(
        after_warmup <= 120,
        "the first frame hashes at most one fingerprint per entry, got {after_warmup}"
    );
}

#[rstest::rstest]
fn streaming_tool_output_renders_the_appended_text() {
    // Given a session streaming a bash tool result.
    let mut state = AppState::default_with_scope_focus();
    state.active_session_mut().begin_streaming();
    state
        .active_session_mut()
        .begin_tool_result("tr_live", "bash", jiff::Timestamp::now());
    state.active_session_mut().append_tool_result_output(
        "tr_live",
        "first chunk of output",
        jinn_tools_msg::ToolOutputKind::Normal,
    );

    let (mut terminal, area) = setup_term(60, 12);
    let mut element = ChatLogElement::new();
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // When more output arrives, growing the entry's content.
    state.active_session_mut().append_tool_result_output(
        "tr_live",
        "second chunk",
        jinn_tools_msg::ToolOutputKind::Normal,
    );
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the appended text is rendered, so the cached lines were invalidated.
    let rows = rendered_content_rows(&state, 60, 12);
    assert!(
        rows.iter().any(|r| r.contains("second")),
        "appended tool output should be rendered after re-render"
    );
}

/// A session whose wrapped content exceeds `u16::MAX` (65,535) lines.
///
/// Each entry wraps to many rows at a narrow width, so only a few thousand
/// entries are needed to overflow the old `u16` line math.
fn oversized_session(entries: usize) -> AppState {
    let mut s = normal_state();
    // ~40 wrapped rows per entry at 36 content columns.
    let body = "wrapped content line that is long enough to wrap repeatedly. ".repeat(10);
    for i in 0..entries {
        s.active_session_mut()
            .push_entry(ChatEntry::user(format!("{i}: {body}")));
    }
    s
}

#[rstest::rstest]
fn line_math_survives_a_session_past_65535_wrapped_lines() {
    // Given a session long enough that its wrapped line total exceeds u16::MAX.
    let state = oversized_session(4_000);
    let mut element = ChatLogElement::new();
    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the recorded total wrapped line count is the true value, not a u16
    // wraparound, so the scroll math has something real to clamp against.
    let total = state.active_session().rendered_max_offset();
    assert!(
        total > u32::from(u16::MAX),
        "a 4000-entry wrapping session should exceed 65535 wrapped lines, got {total}"
    );
}

#[rstest::rstest]
fn an_oversized_session_scrolls_to_a_recent_entry() {
    // Given a session past the old u16 wraparound point.
    let state = oversized_session(4_000);
    let mut element = ChatLogElement::new();
    let (mut terminal, area) = setup_term(40, 10);

    // When rendering and asking for the last entry's screen row.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the selected entry maps to a real row inside the viewport rather
    // than a wrapped-around one.
    let row = state
        .active_session()
        .selected_entry_screen_y(0)
        .expect("selected entry should have a screen row");
    assert!(
        row < area.height,
        "the selected entry should land inside the viewport, got row {row}"
    );
}

/// Render every visible cell of the chat log as a comparable string.
fn render_rows(
    state: &AppState,
    element: &mut ChatLogElement,
    width: u16,
    height: u16,
) -> Vec<String> {
    let (mut terminal, area) = setup_term(width, height);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| {
                    buffer
                        .cell((x, y))
                        .map_or(" ", ratatui::buffer::Cell::symbol)
                        .to_owned()
                })
                .collect::<String>()
        })
        .collect()
}

/// A history mixing every entry kind the chat log renders differently.
fn mixed_history() -> AppState {
    let mut s = normal_state();
    s.active_session_mut()
        .push_entry(ChatEntry::system("system note"));
    s.active_session_mut()
        .push_entry(ChatEntry::user("hello world"));
    s.active_session_mut()
        .push_entry(ChatEntry::assistant("an answer"));
    s.active_session_mut().push_entry(ChatEntry::tool_result(
        "call-1",
        "read",
        "file contents",
        ToolResultStatus::Success,
    ));
    s.active_session_mut().push_entry(ChatEntry::tool_result(
        "call-2",
        "write",
        "wrote it",
        ToolResultStatus::Failure,
    ));
    s.active_session_mut()
        .push_entry(ChatEntry::error("it broke"));
    s.active_session_mut().push_entry(ChatEntry::user(
        "a long line that will wrap across the content width repeatedly for gutter padding",
    ));
    s
}

#[rstest::rstest]
fn a_warm_cache_renders_identically_to_a_cold_one() {
    // Given a mixed history, rendered once to warm every cache.
    let state = mixed_history();
    let mut element = ChatLogElement::new();
    let cold = render_rows(&state, &mut element, 44, 14);

    // When it is rendered again from the warm cache.
    let warm = render_rows(&state, &mut element, 44, 14);

    // Then the output is byte-identical — the fingerprint memo, the LRU, and
    // the threaded gutter count must all preserve what is drawn.
    assert_eq!(
        cold, warm,
        "a warm cache must render exactly like a cold one"
    );
}

#[rstest::rstest]
fn gutter_padding_matches_content_rows_for_a_wrapping_entry() {
    // Given a single entry long enough to wrap several rows.
    let mut s = normal_state();
    s.active_session_mut()
        .push_entry(ChatEntry::user("w ".repeat(200)));
    let mut element = ChatLogElement::new();

    // When rendered.
    let rows = render_rows(&s, &mut element, 44, 14);

    // Then the gutter is two columns wide — a non-space indicator then a
    // cursor bar — and a wrapping entry produces one pair per content row.
    let gutter_rows = rows
        .iter()
        .filter(|row| {
            let mut chars = row.chars();
            matches!(chars.next(), Some(c) if c != ' ') && matches!(chars.next(), Some('┃' | ' '))
        })
        .count();
    assert!(
        gutter_rows > 1,
        "a wrapping entry should produce more than one gutter row, got {gutter_rows}"
    );
}

/// Times the per-frame cache probe for a large session, and separately times
/// what the pre-signature path cost: a full fingerprint per entry per frame.
#[rstest::rstest]
fn a_large_session_frame_avoids_rehashing_its_content() {
    // Given a session whose total text is large (many multi-KB entries).
    let state = {
        let mut s = normal_state();
        // ~4KB per entry, 600 entries => ~2.4MB of tool-result text.
        let big = "x".repeat(4_000);
        for _ in 0..600 {
            s.active_session_mut().push_entry(ChatEntry::tool_result(
                "id",
                "bash",
                &big,
                ToolResultStatus::Success,
            ));
        }
        s
    };
    let mut element = ChatLogElement::new();
    let (mut terminal, area) = setup_term(100, 40);

    // Warm the cache with one frame.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new_with_default_config(&state, &slices, &overlay_views);
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // When the cache is probed for the same entries 20 more times — the work a
    // second through twentieth frame would repeat.
    let entry_count = 600usize;
    let start = std::time::Instant::now();
    for _ in 0..20 {
        for entry in state.active_session().history() {
            let _ = entry.content_signature();
        }
    }
    let signature_elapsed = start.elapsed();

    // And when the equivalent volume of full-fingerprint work runs — what the
    // pre-signature path did on every one of those frames.
    let start = std::time::Instant::now();
    for _ in 0..20 {
        for entry in state.active_session().history() {
            let _ = entry.content_fingerprint();
        }
    }
    let fingerprint_elapsed = start.elapsed();

    // Then the signature path is cheaper, and the avoided hashing scales with
    // session bytes rather than entry count.
    assert_eq!(entry_count, state.active_session().history().len());
    let ratio = fingerprint_elapsed.as_secs_f64() / signature_elapsed.as_secs_f64().max(1e-9);
    assert!(
        ratio > 1.0,
        "the signature path should beat full fingerprinting, got {ratio:.1}x"
    );
}

#[rstest::rstest]
fn unchanged_frame_rewrites_no_visual_items() {
    // Given a chat log that has rendered one frame.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s.active_session_mut().push_entry(ChatEntry::user("world"));
        s
    };
    let (mut terminal, area) = setup_term(40, 10);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new(
                &state,
                &slices,
                &overlay_views,
                jinn_config::empty_config_layer(),
            );
            element.render(frame, area, &ctx);
        })
        .unwrap();
    let after_first = state.active_session().visual_items_writes();
    assert_eq!(after_first, 1, "the first frame must publish the list");

    // When several more frames render with nothing changed.
    for _ in 0..5 {
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_slices::OverlayViews::new();
                let ctx = RenderCtx::new(
                    &state,
                    &slices,
                    &overlay_views,
                    jinn_config::empty_config_layer(),
                );
                element.render(frame, area, &ctx);
            })
            .unwrap();
    }

    // Then the visual items list was not written back again.
    assert_eq!(
        state.active_session().visual_items_writes(),
        after_first,
        "an unchanged frame must reuse the stored visual items"
    );
}

#[rstest::rstest]
fn unchanged_frame_rewrites_no_line_ranges() {
    // Given a chat log that has rendered one frame.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s.active_session_mut().push_entry(ChatEntry::user("world"));
        s
    };
    let (mut terminal, area) = setup_term(40, 10);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new(
                &state,
                &slices,
                &overlay_views,
                jinn_config::empty_config_layer(),
            );
            element.render(frame, area, &ctx);
        })
        .unwrap();
    let after_first = state.active_session().entry_line_ranges_writes();
    assert_eq!(after_first, 1, "the first frame must publish the ranges");

    // When several more frames render with nothing changed.
    for _ in 0..5 {
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_slices::OverlayViews::new();
                let ctx = RenderCtx::new(
                    &state,
                    &slices,
                    &overlay_views,
                    jinn_config::empty_config_layer(),
                );
                element.render(frame, area, &ctx);
            })
            .unwrap();
    }

    // Then the per-entry line ranges were not written back again.
    assert_eq!(
        state.active_session().entry_line_ranges_writes(),
        after_first,
        "an unchanged frame must reuse the stored line ranges"
    );
}

#[rstest::rstest]
fn a_loading_session_shows_the_loading_indication() {
    // Given a chat log whose session is still loading.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.active_session_mut().push_entry(ChatEntry::user("hello"));
        s.session
            .begin_load(s.active_session().session_id().clone());
        s
    };
    let (mut terminal, area) = setup_term(40, 10);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new(
                &state,
                &slices,
                &overlay_views,
                jinn_config::empty_config_layer(),
            );
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the conversation's own text is not drawn — the indication replaces it.
    let drawn = row_text(terminal.backend().buffer(), area);
    assert!(
        !drawn.contains("hello"),
        "a loading session must not show its history"
    );
}

#[rstest::rstest]
fn a_loading_indication_animates_over_time() {
    // Given a chat log whose session is still loading.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.session
            .begin_load(s.active_session().session_id().clone());
        s
    };
    let (mut terminal, area) = setup_term(40, 10);

    // When frames are drawn over time.
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.push(draw_loading_frame(
            &mut element,
            &state,
            &mut terminal,
            area,
        ));
        std::thread::sleep(std::time::Duration::from_millis(120));
    }

    // Then the drawn glyphs are not all the same.
    //
    // The assertion is over the whole window rather than a single pair of
    // frames: the animation advances after each frame is painted, so the first
    // two frames legitimately paint the same glyph. What matters is that the
    // indication keeps moving instead of sitting still for the whole load.
    let first = &seen[0];
    assert!(
        seen.iter().any(|frame| frame != first),
        "a long load must animate, not sit on one static glyph, saw {seen:?}"
    );
    // And the label itself is always there, so the movement is around a
    // meaningful message rather than an empty widget.
    assert!(
        seen.iter().all(|frame| frame.contains("Loading session")),
        "every loading frame must say what it is doing, saw {seen:?}"
    );
}

#[rstest::rstest]
fn a_loading_indication_sits_on_the_bottom_row() {
    // Given a chat log whose session is still loading, in a tall pane.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.session
            .begin_load(s.active_session().session_id().clone());
        s
    };
    let (mut terminal, area) = setup_term(40, 11);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new(
                &state,
                &slices,
                &overlay_views,
                jinn_config::empty_config_layer(),
            );
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the indication sits on the pane's last row — the row directly above
    // the chat bar — rather than floating in the middle of the conversation.
    let rows: Vec<String> = (area.y..area.y + area.height)
        .map(|row| jinn_testutil::buffer_row(terminal.backend().buffer(), row, area.width))
        .collect();
    let painted = rows
        .iter()
        .position(|row| row.contains("Loading session"))
        .expect("the indication must be drawn");
    assert_eq!(
        painted, 10,
        "an 11-row pane must show the indication on its last row, got rows {rows:?}"
    );
}

#[rstest::rstest]
fn a_loading_indication_is_centered_horizontally() {
    // Given a chat log whose session is still loading, in a wide pane.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.session
            .begin_load(s.active_session().session_id().clone());
        s
    };
    let (mut terminal, area) = setup_term(60, 11);

    // When rendering.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new(
                &state,
                &slices,
                &overlay_views,
                jinn_config::empty_config_layer(),
            );
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the label starts near the pane's horizontal centre, not at its edge.
    let row = jinn_testutil::buffer_row(terminal.backend().buffer(), area.y + 10, area.width);
    let start = row
        .find("Loading session")
        .expect("the label must be drawn");
    assert!(
        start > 10,
        "a 60-column pane must not start the label at column {start}: {row:?}"
    );
    // And it stays clear of the right edge rather than being clipped.
    assert!(
        row.contains("Loading session..."),
        "the whole label must fit, got {row:?}"
    );
}

#[rstest::rstest]
fn a_loading_indication_tolerates_a_zero_height_pane() {
    // Given a chat log in a pane with no height, mid-resize.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.session
            .begin_load(s.active_session().session_id().clone());
        s
    };
    let (mut terminal, area) = setup_term(40, 3);
    let collapsed = Rect::new(area.x, area.y, area.width, 0);

    // When rendering into it.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new(
                &state,
                &slices,
                &overlay_views,
                jinn_config::empty_config_layer(),
            );
            element.render(frame, collapsed, &ctx);
        })
        .unwrap();

    // Then nothing is drawn and no panic escapes.
    assert!(
        !row_text(terminal.backend().buffer(), area).contains("Loading session"),
        "a pane with no rows has nowhere to draw the indication"
    );
}

#[rstest::rstest]
fn a_loading_indication_tolerates_a_zero_width_pane() {
    // Given a chat log in a pane with no width, mid-resize.
    let mut element = ChatLogElement::new();
    let state = {
        let mut s = normal_state();
        s.session
            .begin_load(s.active_session().session_id().clone());
        s
    };
    let (mut terminal, area) = setup_term(40, 3);
    let collapsed = Rect::new(area.x, area.y, 0, area.height);

    // When rendering into it.
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new(
                &state,
                &slices,
                &overlay_views,
                jinn_config::empty_config_layer(),
            );
            element.render(frame, collapsed, &ctx);
        })
        .unwrap();

    // Then nothing is drawn and no panic escapes.
    assert!(
        !row_text(terminal.backend().buffer(), area).contains("Loading session"),
        "a pane with no columns has nowhere to draw the indication"
    );
}

/// Draws one frame of a loading chat log and returns the text it painted.
fn draw_loading_frame(
    element: &mut ChatLogElement,
    state: &AppState,
    terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
    area: ratatui::layout::Rect,
) -> String {
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new(
                state,
                &slices,
                &overlay_views,
                jinn_config::empty_config_layer(),
            );
            element.render(frame, area, &ctx);
        })
        .unwrap();
    row_text(terminal.backend().buffer(), area)
}

/// The text of the buffer's rows, joined.
fn row_text(buffer: &ratatui::buffer::Buffer, area: ratatui::layout::Rect) -> String {
    (area.y..area.y + area.height)
        .map(|row| {
            (area.x..area.x + area.width)
                .map(|col| buffer.cell((col, row)).map_or("", |c| c.symbol()))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Measurement coverage
// ---------------------------------------------------------------------------

/// The width a chat log rendered into `width` columns lays its entries out at.
fn content_width_for(width: u16) -> u16 {
    width - GUTTER_WIDTH
}

/// State with `count` user entries in its active session, at `content_width`.
fn measured_state(count: usize, content_width: u16) -> AppState {
    let mut state = normal_state();
    for index in 0..count {
        state
            .active_session_mut()
            .push_entry(ChatEntry::user(format!("message {index}")));
    }
    state.active_session_mut().set_content_width(content_width);
    state
}

/// Whether a frame of the given width would find the session fully measured.
fn coverage_at(state: &AppState, content_width: u16) -> bool {
    let session_id = state.active_session().session_id().clone();
    state
        .frontend
        .line_cache_cell()
        .expect("catalog registered the line-cache cell")
        .update(|cache| {
            crate::kernel_element::is_session_measured(cache, state, &session_id, content_width)
        })
}

/// Renders one frame, filling the cache the way a real frame would.
fn measure_by_rendering(state: &AppState, width: u16, height: u16) {
    let mut element = ChatLogElement::new();
    let (mut terminal, area) = setup_term(width, height);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new(
                state,
                &slices,
                &overlay_views,
                jinn_config::empty_config_layer(),
            );
            element.render(frame, area, &ctx);
        })
        .unwrap();
}

#[rstest::rstest]
fn an_unmeasured_session_reports_not_covered() {
    // Given a session that has never been rendered.
    let state = measured_state(4, 60);

    // When coverage is checked at the width it would render at.
    let covered = coverage_at(&state, 60);

    // Then it is not covered.
    assert!(!covered, "a session with no counts needs a measurement");
}

#[rstest::rstest]
fn a_session_measured_at_a_width_reports_covered() {
    // Given a session a real frame has already measured.
    let state = measured_state(4, 60);
    measure_by_rendering(&state, 62, 10);

    // When coverage is checked at that same width.
    let covered = coverage_at(&state, content_width_for(62));

    // Then it is covered.
    assert!(
        covered,
        "a measured session needs no further measurement, cache holds {}",
        state
            .frontend
            .line_cache_cell()
            .expect("catalog registered the line-cache cell")
            .read()
            .len()
    );
}

#[rstest::rstest]
fn a_session_measured_at_one_width_is_not_covered_at_another() {
    // Given a session measured by a frame of one width.
    let state = measured_state(4, 60);
    measure_by_rendering(&state, 62, 10);

    // When coverage is checked at a different width.
    let covered = coverage_at(&state, content_width_for(62) + 1);

    // Then it is not covered, because those counts are wrong at this width.
    assert!(
        !covered,
        "counts measured at one width cannot serve another"
    );
}

#[rstest::rstest]
fn a_session_with_a_collapsed_ignored_block_reports_covered() {
    // Given a long session that renders as a collapsed block plus visible
    // entries, and that has been measured.
    let mut state = measured_state(0, 60);
    for index in 0..12 {
        state
            .active_session_mut()
            .push_entry(ChatEntry::system(format!("noise {index}")));
    }
    state
        .active_session_mut()
        .push_entry(ChatEntry::user("the visible question"));
    measure_by_rendering(&state, 62, 20);

    // When coverage is checked.
    let visual_items = state.active_session().visual_items_snapshot().len();
    let covered = coverage_at(&state, content_width_for(62));

    // Then it is covered.
    //
    // A collapsed block is one line and is never cached, so a coverage check
    // that probed it would report a miss no measurement could ever fix.
    assert!(
        visual_items > 0,
        "the session must render as visual items for this to mean anything"
    );
    assert!(
        covered,
        "a collapsed ignored block must not read as an unmeasured entry"
    );
}

#[rstest::rstest]
fn coverage_agrees_with_what_the_render_pass_does() {
    // Given a session holding a tool call and its result, which are what the
    // render variant keys on.
    let mut state = measured_state(0, 60);
    state
        .active_session_mut()
        .push_entry(ChatEntry::tool_call("id-1", "grep", "{}"));
    state
        .active_session_mut()
        .push_entry(ChatEntry::tool_result(
            "id-1",
            "grep",
            "found it",
            ToolResultStatus::Success,
        ));
    state
        .active_session_mut()
        .push_entry(ChatEntry::user("thanks"));

    // When a frame measures it and coverage is then checked.
    measure_by_rendering(&state, 62, 20);
    let covered = coverage_at(&state, content_width_for(62));

    // Then coverage says the next frame is a full cache hit.
    assert!(
        covered,
        "coverage must agree with the render pass, or every switch re-measures"
    );
}

/// Given a session that has rendered a frame, when the coverage probe reads
/// the session back, then it sees the collapse count that frame used.
#[rstest::rstest]
fn a_render_publishes_the_collapse_count_the_coverage_probe_reads_back() {
    // Given a session with a history.
    let mut state = normal_state();
    let session_id = state.session.active_session_id().clone();
    for text in ["first", "second"] {
        state
            .session
            .get_mut(&session_id)
            .unwrap()
            .push_entry(ChatEntry::user(text));
    }

    // When one frame is drawn.
    let mut element = ChatLogElement::new();
    let (mut terminal, area) = setup_term(100, 20);
    terminal
        .draw(|frame| {
            let slices = jinn_slices::Slices::new();
            let overlay_views = jinn_slices::OverlayViews::new();
            let ctx = RenderCtx::new(
                &state,
                &slices,
                &overlay_views,
                jinn_config::empty_config_layer(),
            );
            element.render(frame, area, &ctx);
        })
        .unwrap();

    // Then the session records the count that frame used, so the off-thread
    // probe builds the same visual items.
    assert_eq!(
        state.session.get(&session_id).unwrap().min_collapse_count(),
        Some(jinn_chat_log_view_msg::DEFAULT_MIN_COLLAPSE_COUNT),
        "the coverage probe reads this back; if the frame did not publish it, the probe builds different visual items than the frame did"
    );
}
