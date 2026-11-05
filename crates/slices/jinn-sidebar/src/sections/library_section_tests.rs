//! Tests for the Library sidebar section.
//!
//! Rendering (header, dimmed archived members, active marker), navigation
//! with cursor restore, and the three section handlers — `<enter>` through
//! the shared activation, `a` archive with membership kept, `L` remove.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code"
)]

use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::render_test::render_section;
use jinn_session_state::ChatSessionState;
use jinn_session_store_msg::{LibraryState, SessionSummary, library_slot};

use super::library_section::{LibrarySection, handle_library_archive, handle_library_remove};
use crate::sections::section_trait::{EnterFrom, SidebarIntent, SidebarSection as _};

/// A member summary: title, archived, in the library.
fn member(title: &str, archived: bool) -> SessionSummary {
    SessionSummary {
        session_id: jinn_core_types::SessionId::new(),
        title: title.to_owned(),
        updated_at: jiff::Timestamp::now(),
        created_at: jiff::Timestamp::now(),
        session_state: if archived {
            jinn_session_store_msg::SessionState::Archived
        } else {
            jinn_session_store_msg::SessionState::Loaded
        },
        in_library: true,
        is_bot: false,
        parent_session: None,
        project: None,
    }
}

/// State with a library cell holding `members`, the library section focused,
/// and the cursor on row `selected`.
///
/// The cell is seeded through the state's own registry: `attach_slices` is a
/// one-shot set, and `default_with_scope_focus()` has already attached the
/// catalog's registry, so a second `Slices` would be silently dropped.
fn library_state(members: Vec<SessionSummary>, selected: usize) -> AppState {
    let state = AppState::default_with_scope_focus();
    let selected_id = {
        let library_cell = state
            .frontend
            .slices()
            .and_then(|slices| slices.reader::<LibraryState>(&library_slot()))
            .expect("the cell catalog registers the library slot");
        library_cell.update(|library| library.entries = members.clone());
        members.get(selected).map(|entry| entry.session_id.clone())
    };
    state
        .frontend
        .scope_push(jinn_sidebar_msg::SidebarSectionId::Library.focus_scope());
    state
        .frontend
        .update_sections(|s| s.library.selected_id = selected_id);
    state
}

/// The visible text of one buffer row.
fn row_text_of(buffer: &ratatui::buffer::Buffer, width: u16, y: u16) -> String {
    (0..width)
        .map(|x| {
            buffer
                .cell((x, y))
                .map_or(" ", ratatui::buffer::Cell::symbol)
        })
        .collect()
}

#[rstest::rstest]
fn a_member_in_memory_renders_its_live_title_over_the_stale_cell_summary() {
    // Given a library member whose cell summary still carries the old title
    // (the async refresh has not landed) while the in-memory session holds
    // the rename.
    let mut section = LibrarySection::default();
    let summary = member("stale title", false);
    let mut live = ChatSessionState::new();
    live.set_session_id(summary.session_id.clone());
    live.set_title("renamed live".to_owned());
    let state = {
        let mut state = library_state(vec![summary], 0);
        state.session.insert(live);
        state
    };

    let width = 40u16;
    let height = 8u16;
    let terminal = render_section(
        &state,
        &jinn_slices::Slices::new(),
        width,
        height,
        |frame, area, ctx| {
            section.render(frame, area, 0, ctx);
        },
    );
    let buffer = terminal.backend().buffer();

    // Then the row shows the live title, not the cell's stale one.
    let live_row = (0..height).find(|&y| row_text_of(buffer, width, y).contains("renamed live"));
    assert!(
        live_row.is_some(),
        "the in-memory title must be drawn the frame after a rename"
    );
    let stale_row = (0..height).find(|&y| row_text_of(buffer, width, y).contains("stale title"));
    assert!(
        stale_row.is_none(),
        "the stale summary title must not render while a live one exists"
    );
}

#[rstest::rstest]
fn an_archived_member_renders_its_cell_title_when_not_in_memory() {
    // Given an archived member: not in memory, so the cell is the only
    // source of its title.
    let mut section = LibrarySection::default();
    let state = library_state(vec![member("archived work", true)], 0);

    let width = 40u16;
    let height = 8u16;
    let terminal = render_section(
        &state,
        &jinn_slices::Slices::new(),
        width,
        height,
        |frame, area, ctx| {
            section.render(frame, area, 0, ctx);
        },
    );
    let buffer = terminal.backend().buffer();

    // Then the cell's title renders.
    let row = (0..height).find(|&y| row_text_of(buffer, width, y).contains("archived work"));
    assert!(
        row.is_some(),
        "an out-of-memory member falls back to its cell summary title"
    );
}

#[rstest::rstest]
fn a_selected_library_row_bands_the_full_width() {
    // Given the library focused with the cursor on its only member.
    let mut section = LibrarySection::default();
    let state = library_state(vec![member("shelved work", false)], 0);
    let theme = state.frontend.theme.clone();

    let width = 40u16;
    let height = 8u16;
    let terminal = render_section(
        &state,
        &jinn_slices::Slices::new(),
        width,
        height,
        |frame, area, ctx| {
            section.render(frame, area, 0, ctx);
        },
    );
    let buffer = terminal.backend().buffer();

    // Then the member's row carries the selection background...
    let member_y = (0..height)
        .find(|&y| row_text_of(buffer, width, y).contains("shelved work"))
        .expect("member row rendered");
    let banded: Vec<u16> = (0..width)
        .filter(|&x| {
            buffer
                .cell((x, member_y))
                .is_some_and(|cell| cell.bg == theme.selection_fg)
        })
        .collect();
    assert!(
        !banded.is_empty(),
        "the selected library row must carry the selection band"
    );
    // ...across the full width, including cells past the last character.
    assert_eq!(
        banded.last().copied(),
        Some(width.saturating_sub(1)),
        "the band must reach the row's last cell"
    );
    // And the band's text is the sidebar background — the title span carries
    // no hard foreground that would defeat it.
    let text_x = banded[0];
    let text_cell = buffer.cell((text_x, member_y)).expect("a text cell");
    assert_eq!(
        text_cell.fg, theme.gutter_bg,
        "band text must be the sidebar background, not a span foreground"
    );
}

#[rstest::rstest]
fn library_section_renders_title_and_members() {
    let mut section = LibrarySection::default();
    let state = library_state(vec![member("shelved work", false)], 0);

    // When rendering.
    let terminal = render_section(
        &state,
        &jinn_slices::Slices::new(),
        30,
        5,
        |frame, area, ctx| {
            section.render(frame, area, 0, ctx);
        },
    );

    // Then the header names the section and the member is listed by title.
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(text.contains("Library"), "header missing: {text:?}");
    assert!(
        text.contains("shelved work"),
        "member row missing: {text:?}"
    );
}

#[rstest::rstest]
fn library_section_renders_archived_member_dimmed() {
    // Given a library with two archived members and the cursor on the first —
    // so the second renders unselected, its state color visible (a selected
    // row is the band, which overrides every state color).
    let mut section = LibrarySection::default();
    let first = {
        let mut summary = member("old shelved", true);
        summary.title = "old shelved".to_owned();
        summary
    };
    let second = {
        let mut summary = member("older shelved", true);
        summary.title = "older shelved".to_owned();
        summary
    };
    let state = library_state(vec![first, second], 0);

    // When rendering.
    let terminal = render_section(
        &state,
        &jinn_slices::Slices::new(),
        30,
        6,
        |frame, area, ctx| {
            section.render(frame, area, 0, ctx);
        },
    );

    // Then the unselected archived member renders in the muted color, not
    // the primary.
    let buffer = terminal.backend().buffer();
    let row_text = |y: u16| {
        (0..30)
            .filter_map(|x| buffer.cell((x, y)))
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
    };
    let member_y = (0..6)
        .find(|&y| row_text(y).contains("older shelved"))
        .unwrap_or_else(|| panic!("archived member row missing"));
    let styled = (0..30)
        .filter_map(|x| buffer.cell((x, member_y)))
        .find(|cell| cell.symbol().trim() == "o")
        .map(ratatui::buffer::Cell::style)
        .unwrap_or_default();
    assert_eq!(
        styled.fg,
        Some(state.frontend.theme.muted_text),
        "an archived member must render dimmed"
    );
}

#[rstest::rstest]
fn library_navigation_moves_down_to_the_last_member() {
    // Given a library with two members and the cursor on the first.
    let mut state = library_state(vec![member("first", false), member("second", false)], 0);

    // When moving down.
    let moved = super::library_section::navigate(&SidebarIntent::MoveDown, &mut state);

    // Then the cursor lands on the second member.
    assert!(matches!(
        moved,
        crate::sections::section_trait::SectionNavResult::Moved
    ));
    let second_id = super::library_section::entries(&state)[1]
        .session_id
        .clone();
    assert_eq!(
        state
            .frontend
            .with_sections(|s| s.library.selected_id.clone(), || None),
        Some(second_id)
    );
}

#[rstest::rstest]
fn library_navigation_exhausts_at_the_top() {
    // Given a library with two members and the cursor on the first.
    let mut state = library_state(vec![member("first", false), member("second", false)], 0);
    let first_id = super::library_section::entries(&state)[0]
        .session_id
        .clone();

    // When moving up past the top.
    let exhausted = super::library_section::navigate(&SidebarIntent::MoveUp, &mut state);

    // Then the section exhausts and leaves the cursor alone, so the sidebar
    // can move focus to the neighbor section instead.
    assert!(matches!(
        exhausted,
        crate::sections::section_trait::SectionNavResult::Exhausted
    ));
    assert_eq!(
        state
            .frontend
            .with_sections(|s| s.library.selected_id.clone(), || None),
        Some(first_id)
    );
}

#[rstest::rstest]
fn library_receive_cursor_from_bottom_selects_last_member() {
    // Given a library with two members and no cursor.
    let mut state = library_state(vec![member("first", false), member("second", false)], 0);
    state
        .frontend
        .update_sections(|s| s.library.selected_id = None);

    // When entering the section from below.
    let _result = super::library_section::receive_cursor(&mut state, EnterFrom::Bottom);

    // Then the cursor lands on the last member — the row a removal will
    // restore to via `last_cursor_row`.
    let last_id = super::library_section::entries(&state)[1]
        .session_id
        .clone();
    assert_eq!(
        state
            .frontend
            .with_sections(|s| s.library.selected_id.clone(), || None),
        Some(last_id)
    );
}

#[rstest::rstest]
fn entering_library_requests_a_refresh_even_when_the_cell_is_empty() {
    // Given a library section focused over an *empty* cell — which is not
    // evidence of an empty store: the cell may simply be stale.
    let mut state = library_state(vec![], 0);
    state
        .frontend
        .update_sections(|s| s.library.selected_id = None);

    // When entering the section from above.
    let result = super::library_section::receive_cursor(&mut state, EnterFrom::Top);

    // Then the refresh request is published anyway, and no cursor was set —
    // an empty list has no row to name.
    assert!(
        result
            .message_names
            .iter()
            .any(|name| name.ends_with("LoadLibraryEntries")),
        "entering the library must ask the store actor to fill the cell, \
         published: {:?}",
        result.message_names
    );
    assert_eq!(
        state
            .frontend
            .with_sections(|s| s.library.selected_id.clone(), || None),
        None
    );
}

#[rstest::rstest]
fn entering_library_with_members_requests_a_refresh() {
    // Given a library with one member and no cursor.
    let mut state = library_state(vec![member("only", false)], 0);
    state
        .frontend
        .update_sections(|s| s.library.selected_id = None);

    // When entering the section from above.
    let result = super::library_section::receive_cursor(&mut state, EnterFrom::Top);

    // Then the refresh is requested.
    assert!(
        result
            .message_names
            .iter()
            .any(|name| name.ends_with("LoadLibraryEntries")),
        "entering the library must ask the store actor to fill the cell, \
         published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
fn entering_library_places_the_cursor_on_the_first_member() {
    // Given a library with one member and no cursor.
    let mut state = library_state(vec![member("only", false)], 0);
    state
        .frontend
        .update_sections(|s| s.library.selected_id = None);

    // When entering the section from above.
    let _result = super::library_section::receive_cursor(&mut state, EnterFrom::Top);

    // Then the cursor lands on the first member.
    let only_id = super::library_section::entries(&state)[0]
        .session_id
        .clone();
    assert_eq!(
        state
            .frontend
            .with_sections(|s| s.library.selected_id.clone(), || None),
        Some(only_id)
    );
}

#[rstest::rstest]
fn library_enter_on_archived_member_publishes_load() {
    // Given a focused library whose cursor names a member that is *not* in
    // memory (the archived case).
    let mut state = library_state(vec![member("archived member", true)], 0);

    // When activating it through the shared activation the `<enter>` row
    // delegates to.
    let result = crate::sections::sessions::handle_session_activate(&mut state);

    // Then the standard load command is published — whose handler
    // unarchives as a side effect.
    assert!(
        result
            .message_names
            .iter()
            .any(|name| name.ends_with("SessionLoadRequested")),
        "activating an archived member must go through the load path, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
fn library_activate_insert_lands_in_input_over_a_normal_base() {
    // Given a focused library whose cursor names a member that is in
    // memory — activation switches immediately, where an archived member
    // would wait for the store actor's load.
    let mut state = library_state(vec![member("member", false)], 0);
    let member_id = super::library_section::entries(&state)[0]
        .session_id
        .clone();
    {
        let mut live = ChatSessionState::new();
        live.set_session_id(member_id.clone());
        state.session.insert(live);
    }

    // When activating into insert mode — the handler the `i` row binds.
    crate::sections::sessions::handle_session_activate_insert(&mut state);

    // Then the scope stack is Input over Normal — the same landing the
    // sessions and bots sections' `i` gives, so ESC returns to Normal.
    assert_eq!(state.frontend.scope(), jinn_slices::FocusScope::Input);
    assert_eq!(
        state.frontend.scope_parent(),
        Some(jinn_slices::FocusScope::Normal)
    );

    // And the active session is the member under the cursor.
    assert_eq!(state.session.active_session_id(), &member_id);
}

#[rstest::rstest]
fn library_archive_publishes_archive_command() {
    // Given a focused library whose cursor names a live, idle member.
    let mut state = library_state(vec![member("live member", false)], 0);
    let target_id = super::library_section::entries(&state)[0]
        .session_id
        .clone();
    let mut live = ChatSessionState::new();
    live.set_session_id(target_id.clone());
    state.session.insert(live);

    // When archiving it.
    let result = handle_library_archive(&mut state);

    // Then the archive command is published — the actor's archival keeps
    // the membership flag, so the member stays listed.
    assert!(
        result
            .message_names
            .iter()
            .any(|name| name.ends_with("ArchiveSession")),
        "archive must publish ArchiveSession, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
fn library_archive_on_archived_member_is_noop() {
    // Given a focused library whose cursor names a member not in memory.
    let mut state = library_state(vec![member("archived member", true)], 0);

    // When archiving it.
    let result = handle_library_archive(&mut state);

    // Then nothing is published — re-archiving would be a no-op anyway.
    assert!(
        result.message_names.is_empty(),
        "archiving an archived member must be a no-op, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
fn library_remove_publishes_membership_clear() {
    // Given a focused library whose cursor names an archived member that is
    // not in memory — removal must work by id alone.
    let mut state = library_state(vec![member("archived member", true)], 0);

    // When removing it from the library.
    let result = handle_library_remove(&mut state);

    // Then the membership-clear command is published for that id.
    assert!(
        result
            .message_names
            .iter()
            .any(|name| name.ends_with("SetSessionLibrary")),
        "remove must publish SetSessionLibrary, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
fn removing_a_member_lands_the_cursor_on_the_nearest_survivor() {
    // Given a focused library with two members and the cursor on the first.
    let mut state = library_state(vec![member("first", false), member("second", false)], 0);
    let first_id = super::library_section::entries(&state)[0]
        .session_id
        .clone();
    let second_id = super::library_section::entries(&state)[1]
        .session_id
        .clone();

    // When removing the first.
    let _result = handle_library_remove(&mut state);

    // Then the cursor names the second member — the row now sitting where
    // the removed one was, not nothing.
    assert_eq!(
        state
            .frontend
            .with_sections(|s| s.library.selected_id.clone(), || None),
        Some(second_id),
        "removing {first_id} must land the cursor on the nearest surviving member"
    );
}

#[rstest::rstest]
fn removing_the_last_member_clears_the_cursor() {
    // Given a focused library whose only member is under the cursor.
    let mut state = library_state(vec![member("sole", false)], 0);

    // When removing it.
    let _result = handle_library_remove(&mut state);

    // Then the cursor is unset — there is no row left to name.
    assert_eq!(
        state
            .frontend
            .with_sections(|s| s.library.selected_id.clone(), || None),
        None,
        "removing the last member must clear the cursor"
    );
}

#[rstest::rstest]
fn removing_the_last_row_lands_the_cursor_on_its_neighbor() {
    // Given a focused library with two members and the cursor on the second.
    let mut state = library_state(vec![member("first", false), member("second", false)], 1);
    let first_id = super::library_section::entries(&state)[0]
        .session_id
        .clone();

    // When removing the second.
    let _result = handle_library_remove(&mut state);

    // Then the cursor clamps to the new last row — the first member.
    assert_eq!(
        state
            .frontend
            .with_sections(|s| s.library.selected_id.clone(), || None),
        Some(first_id),
        "removing the last row must clamp the cursor to the survivor"
    );
}

#[rstest::rstest]
#[test]
fn go_key_reruns_bot_from_library_scope() {
    // Given a focused library whose cursor names an idle bot member.
    let mut state = library_state(vec![member("bot member", false)], 0);
    let bot_id = super::library_section::entries(&state)[0]
        .session_id
        .clone();
    {
        let parent = ChatSessionState::new();
        let mut bot = ChatSessionState::new_bot(&parent, true);
        bot.set_session_id(bot_id.clone());
        bot.set_title("bot member".to_owned());
        bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
        bot.set_bot_is_prepping(false);
        bot.set_seed_template("verify: <prior_report>".to_owned());
        state.session.insert(bot);
    }

    // When `G` re-runs it through the shared handler the library row binds.
    let result = crate::sections::sessions::bot_actions::handle_rerun_bot(&mut state);

    // Then the dispatch is published — the same handler the sessions and
    // bots scopes bind, resolved through the library cursor.
    assert!(
        result
            .message_names
            .iter()
            .any(|name| name.ends_with("EnqueueUserMessage")),
        "G in the library must re-run the bot, published: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
fn streaming_member_wears_the_working_indicator() {
    // Given a library whose unselected member is in memory and streaming.
    let mut section = LibrarySection::default();
    let mut state = library_state(vec![member("working member", false)], 0);
    let member_id = super::library_section::entries(&state)[0]
        .session_id
        .clone();
    {
        let mut live = ChatSessionState::new();
        live.set_session_id(member_id.clone());
        live.begin_streaming();
        state.session.insert(live);
    }

    // When rendering.
    let terminal = render_section(
        &state,
        &jinn_slices::Slices::new(),
        30,
        5,
        |frame, area, ctx| {
            section.render(frame, area, 0, ctx);
        },
    );

    // Then the row's leading indicator cell carries the streaming color —
    // the same cell and color the sessions rows use.
    let buffer = terminal.backend().buffer();
    let member_y = (0..5)
        .find(|&y| {
            (0..30)
                .filter_map(|x| buffer.cell((x, y)))
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .contains("working member")
        })
        .unwrap_or_else(|| panic!("member row missing"));
    let indicator = buffer.cell((2, member_y)).expect("indicator cell");
    assert_eq!(
        indicator.style().fg,
        Some(state.frontend.theme.streaming),
        "a streaming member must show the working indicator in the streaming color"
    );
}

#[rstest::rstest]
fn archived_member_has_a_blank_indicator_cell() {
    // Given a library whose unselected member is archived — not in memory.
    let mut section = LibrarySection::default();
    let first = member("old shelved", true);
    let second = {
        let mut summary = member("older shelved", true);
        summary.title = "older shelved".to_owned();
        summary
    };
    let state = library_state(vec![first, second], 0);

    // When rendering.
    let terminal = render_section(
        &state,
        &jinn_slices::Slices::new(),
        30,
        6,
        |frame, area, ctx| {
            section.render(frame, area, 0, ctx);
        },
    );

    // Then the unselected archived member's indicator cell is blank — a
    // dormant member has no working state to show.
    let buffer = terminal.backend().buffer();
    let member_y = (0..6)
        .find(|&y| {
            (0..30)
                .filter_map(|x| buffer.cell((x, y)))
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .contains("older shelved")
        })
        .unwrap_or_else(|| panic!("archived member row missing"));
    let indicator = buffer.cell((2, member_y)).expect("indicator cell");
    assert_eq!(
        indicator.symbol().trim(),
        "",
        "an archived member must have a blank indicator cell"
    );
}

#[rstest::rstest]
fn error_tailed_member_wears_the_red_error_block() {
    // Given a library with two members — the first under the cursor, so the
    // second renders unselected (a selected row is the band, which overrides
    // every state color) — and the second's history ending in an error.
    let mut section = LibrarySection::default();
    let mut state = library_state(
        vec![member("fine member", false), member("failed member", false)],
        0,
    );
    let member_id = super::library_section::entries(&state)[1]
        .session_id
        .clone();
    {
        let mut live = ChatSessionState::new();
        live.set_session_id(member_id.clone());
        live.push_entry(jinn_kernel::protocol::ChatEntry::error(
            "stream cancelled".to_owned(),
        ));
        state.session.insert(live);
    }

    // When rendering.
    let terminal = render_section(
        &state,
        &jinn_slices::Slices::new(),
        30,
        6,
        |frame, area, ctx| {
            section.render(frame, area, 0, ctx);
        },
    );

    // Then the unselected member's title wears the red error block — the
    // same inversion the sessions rows paint, with the sidebar background as
    // the row's own foreground.
    let theme = &state.frontend.theme;
    let buffer = terminal.backend().buffer();
    let member_y = (0..6)
        .find(|&y| {
            (0..30)
                .filter_map(|x| buffer.cell((x, y)))
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .contains("failed member")
        })
        .unwrap_or_else(|| panic!("member row missing"));
    let styled = (0..30)
        .filter_map(|x| buffer.cell((x, member_y)))
        .find(|cell| cell.symbol().trim() == "f")
        .map(ratatui::buffer::Cell::style)
        .unwrap_or_default();
    assert_eq!(
        styled.bg,
        Some(ratatui::style::Color::Red),
        "an error-tailed member must wear the red error block"
    );
    assert_eq!(
        styled.fg,
        Some(theme.gutter_bg),
        "the error block's text is the sidebar background"
    );
}

#[rstest::rstest]
fn a_bot_member_renders_in_the_bots_color() {
    // Given a library with three live members — the cursor on the first, so
    // the two asserted rows (an ordinary session and a bot) render
    // unselected with their state colors visible.
    let mut section = LibrarySection::default();
    let mut bot = member("watcher bot", false);
    bot.is_bot = true;
    let state = library_state(
        vec![
            member("cursor row", false),
            member("plain work", false),
            bot,
        ],
        0,
    );

    // When rendering.
    let terminal = render_section(
        &state,
        &jinn_slices::Slices::new(),
        30,
        8,
        |frame, area, ctx| {
            section.render(frame, area, 0, ctx);
        },
    );

    // Then the unselected ordinary member renders in the primary text color,
    // while the bot member wears the bots color — the same tint the sessions
    // tree gives a bot row.
    let theme = &state.frontend.theme;
    let buffer = terminal.backend().buffer();
    let row_text = |y: u16| {
        (0..30)
            .filter_map(|x| buffer.cell((x, y)))
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
    };
    let plain_y = (0..8)
        .find(|&y| row_text(y).contains("plain work"))
        .unwrap_or_else(|| panic!("plain member row missing"));
    let plain_style = (0..30)
        .filter_map(|x| buffer.cell((x, plain_y)))
        .find(|cell| cell.symbol().trim() == "p")
        .map(ratatui::buffer::Cell::style)
        .unwrap_or_default();
    assert_eq!(
        plain_style.fg,
        Some(theme.primary_text),
        "an ordinary member must render in the primary text color"
    );
    let bot_y = (0..8)
        .find(|&y| row_text(y).contains("watcher bot"))
        .unwrap_or_else(|| panic!("bot member row missing"));
    let bot_style = (0..30)
        .filter_map(|x| buffer.cell((x, bot_y)))
        .find(|cell| cell.symbol().trim() == "w")
        .map(ratatui::buffer::Cell::style)
        .unwrap_or_default();
    assert_eq!(
        bot_style.fg,
        Some(theme.bot_fg),
        "a bot member must wear the bots color"
    );
}

#[rstest::rstest]
fn an_archived_bot_member_dims_to_the_subagent_color() {
    // Given a library with three archived members — the cursor on the first,
    // so the two asserted rows (an ordinary session and a bot) render
    // unselected with their dormant colors visible.
    let mut section = LibrarySection::default();
    let mut bot = member("sleeping bot", true);
    bot.is_bot = true;
    let state = library_state(
        vec![member("cursor row", true), member("old plain", true), bot],
        0,
    );

    // When rendering.
    let terminal = render_section(
        &state,
        &jinn_slices::Slices::new(),
        30,
        8,
        |frame, area, ctx| {
            section.render(frame, area, 0, ctx);
        },
    );

    // Then the archived bot renders in the subagent color — the dormant-role
    // color the sessions tree uses where a live row would use muted — while
    // the archived ordinary member stays muted.
    let theme = &state.frontend.theme;
    let buffer = terminal.backend().buffer();
    let row_text = |y: u16| {
        (0..30)
            .filter_map(|x| buffer.cell((x, y)))
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
    };
    let plain_y = (0..8)
        .find(|&y| row_text(y).contains("old plain"))
        .unwrap_or_else(|| panic!("archived plain row missing"));
    let plain_style = (0..30)
        .filter_map(|x| buffer.cell((x, plain_y)))
        .find(|cell| cell.symbol().trim() == "o")
        .map(ratatui::buffer::Cell::style)
        .unwrap_or_default();
    assert_eq!(
        plain_style.fg,
        Some(theme.muted_text),
        "an archived ordinary member must render dimmed"
    );
    let bot_y = (0..8)
        .find(|&y| row_text(y).contains("sleeping bot"))
        .unwrap_or_else(|| panic!("archived bot row missing"));
    let bot_style = (0..30)
        .filter_map(|x| buffer.cell((x, bot_y)))
        .find(|cell| cell.symbol().trim() == "s")
        .map(ratatui::buffer::Cell::style)
        .unwrap_or_default();
    assert_eq!(
        bot_style.fg,
        Some(theme.subagent_fg),
        "an archived bot member must wear the subagent color"
    );
}
