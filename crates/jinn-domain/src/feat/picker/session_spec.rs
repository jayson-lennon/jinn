//! The session picker's spec — behavior authored once in the builder.
//!
//! A tree browser over the session history: subagent sessions nested under
//! their parents, telescope rows (datetime, project column, title), archived
//! sessions dimmed. Entries load asynchronously through
//! `SessionPersistenceActor` (the SQLite read is genuinely async), so the
//! open hook only resets storage and emits the load message. Confirm begins
//! the load and dispatches the switch command.

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::RowCtx;
use jinn_picker::StatusCtx;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::common::app_state::AppState;
use crate::feat::session::picker_entry::SessionTreeEntry;
use crate::feat::session::protocol::load_session_picker_entries::LoadSessionPickerEntries;
use crate::feat::session::protocol::session_load_requested::SessionLoadRequested;
use crate::feat::ui::picker_states::PickerExt;

/// Builds the session picker's spec.
#[must_use]
pub fn session_spec() -> PickerSpec<SessionTreeEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::SESSION_ID))
        .title(" Sessions ")
        .widget(jinn_picker::PickerWidget::Tree)
        .row(session_row)
        .search(|entry| entry.title.clone())
        .on_open(open_sessions)
        .on_confirm(confirm_session_spec)
        .status(session_status)
}

/// The domain state behind an [`ActionCtx`]. The kernel's host lens always
/// lends `AppState`; this downcast is the spec's single sanctioned escape.
fn state_of<'a>(ctx: &'a mut ActionCtx<'_>) -> &'a mut AppState {
    ctx.state_any()
        .downcast_mut::<AppState>()
        .expect("domain host lends AppState")
}

/// The read-only domain state behind a [`StatusCtx`].
fn state_ref_of<'a>(ctx: &'a StatusCtx<'_>) -> &'a AppState {
    ctx.state_any_ref()
        .downcast_ref::<AppState>()
        .expect("domain host lends AppState")
}

// ── Rendering ────────────────────────────────────────────────────────────

/// Renders one picker row: the telescope row with the widget's tree
/// connector placed directly before the title text (never before the
/// datetime or project column).
pub fn session_row(entry: &SessionTreeEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    SessionTreeEntry::render_row_impl(
        entry,
        ctx.is_selected,
        ctx.match_ranges,
        ctx.tree_prefix,
        ctx.tree_style,
    )
}

/// The status line: a hint for creating a new session from inside the picker.
#[expect(clippy::unnecessary_wraps, reason = "hook signature is Option<Line>")]
fn session_status(ctx: &StatusCtx<'_>) -> Option<Line<'static>> {
    let state = state_ref_of(ctx);
    let orange = ratatui::style::Style::default().fg(state.frontend.theme.accent_action);
    Some(Line::from(vec![
        Span::styled("CTRL+N ".to_owned(), orange),
        Span::styled("to create a new session".to_owned(), {
            ratatui::style::Style::default().fg(state.frontend.theme.muted_text)
        }),
    ]))
}

// ── Lifecycle ────────────────────────────────────────────────────────────

/// Opening the session picker: fresh filter + selection, then request the
/// session tree from `SessionPersistenceActor` (the SQLite read is async —
/// the actor wraps the entries via `make_items` and writes storage).
fn open_sessions(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state.frontend.session_picker_mut().reset();
    PickerOutcome::empty().with_message(LoadSessionPickerEntries)
}

/// Enter on the session picker: begin loading the selected session and
/// dispatch the switch command.
fn confirm_session_spec(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let Some(session_id) = state_of(ctx)
        .frontend
        .session_picker()
        .selected_item()
        .map(|item| item.entry().session_id.clone())
    else {
        return PickerOutcome::empty();
    };
    let state = state_of(ctx);
    state.session.begin_load(session_id.clone());
    PickerOutcome::new_message(SessionLoadRequested { session_id }).close()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::feat::picker::PickerKind;
    use crate::feat::picker::intent::handle_open_picker;
    use crate::feat::session::chat_session::SessionState;

    #[rstest::rstest]
    fn open_resets_storage_and_emits_the_load_message() {
        // Given a default state.
        let mut state = AppState::default_with_scope_focus();

        // When opening the session picker through the real open path.
        let registry = crate::feat::picker::registry::build_picker_registry();
        let result = handle_open_picker(&mut state, PickerKind::Session, &registry);

        // Then the load request is dispatched to the actor.
        assert!(result.message_names.contains((&"jinn_domain::feat::session::protocol::load_session_picker_entries::LoadSessionPickerEntries")));
    }

    #[rstest::rstest]
    fn confirm_begins_load_emits_and_closes() {
        // Given an open session picker with one selected entry.
        let mut state = AppState::default_with_scope_focus();
        let theme = crate::feat::theme::default_theme();
        let session_entry = SessionTreeEntry {
            session_id: crate::protocol::SessionId::new(),
            id_str: "s1".to_owned(),
            title: "Root session".to_owned(),
            updated_at: jiff::Timestamp::now(),
            theme,
            session_state: SessionState::Archived,
            parent_id: None,
            parent_id_str: None,
            project: None,
            project_display: String::new(),
            project_width: 0,
        };
        {
            let registry = crate::feat::picker::registry::build_picker_registry();
            let items = registry
                .make_items(
                    crate::feat::picker::registry::SESSION_ID,
                    vec![session_entry],
                )
                .expect("session spec registered");
            state.frontend.session_picker_mut().set_items(items);
        }
        state
            .frontend
            .scope_push(crate::common::app_state::FocusScope::Picker {
                kind: PickerKind::Session,
            });
        let session_id = {
            state.frontend.session_picker().items()[0]
                .entry()
                .session_id
                .clone()
        };

        // When confirming through the real confirm path.
        let registry = crate::feat::picker::registry::build_picker_registry();
        let (result, _redispatch) =
            crate::feat::picker::intent::handle_picker_confirm(&mut state, &registry);

        // Then the switch command is dispatched and the picker closes.
        assert!(
            result
                .message_names
                .contains((&"jinn_domain::feat::session::protocol::session_load_requested::SessionLoadRequested")),
            "switch command dispatched: {:?}",
            result.message_names
        );
        assert!(state.frontend.picker_kind().is_none(), "picker closed");
        let _ = session_id;
    }
}
