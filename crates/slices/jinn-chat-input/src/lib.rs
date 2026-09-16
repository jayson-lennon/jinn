//! The chat-input slice — the per-session draft in the box at the bottom of
//! the chat screen.
//!
//! Owns one cell ([`chat_inputs_slot`]) holding
//! [`jinn_chat_input_msg::ChatInputs`]: each session's input buffer, cursor, wrap
//! cache, sticky Queue/Steer submission mode, and autocomplete session. The
//! kernel's exempt IntentHandler performs the edits through `ChatSession`'s
//! closure accessors (a facade over the cell), the render pass snapshots the
//! draft through the same accessors, and the session actor pours drained
//! queue text back into the box on stream error/cancel. There is no actor
//! and no route row: the writers are the exempt sync handler, the render
//! pass, and the session actor, exactly as the migration docs prescribe.

pub use jinn_chat_input_msg::chat_inputs_slot;

use jinn_slices::SliceHost;

/// Activates the slice: mints the chat-inputs cell. No routes, no actors,
/// no view.
///
/// # Panics
///
/// Panics if the slot is already registered — double activation is a
/// wiring bug.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(host: &mut SliceHost<'_, jinn_slices::RenderFacts>) {
    let _cell = host
        .register_cell(chat_inputs_slot(), jinn_chat_input_msg::ChatInputs::new())
        .expect("chat-input slot is registered exactly once at wiring");
}

#[cfg(test)]
mod activation_tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use jinn_slices::SliceHost;

    #[rstest::rstest]
    #[tokio::test]
    async fn activate_registers_the_chat_inputs_cell() {
        // Given a host over an empty slice registry.
        let slices = jinn_slices::Slices::new();
        let mut viewport = jinn_domain::common::slices::view::Viewport::new();
        let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
        let key_routes = jinn_slices::KeyRoutes::new();
        let services = jinn_domain::Services::new_fake().await;
        let mut host = SliceHost::new(
            &slices,
            &mut viewport,
            &overlay_views,
            &key_routes,
            &services.trouper_system,
        );

        // When activating the slice.
        crate::activate(&mut host);

        // Then the cell resolves and round-trips a per-session write.
        let cell = slices
            .reader::<jinn_chat_input_msg::ChatInputs>(&crate::chat_inputs_slot())
            .expect("activation must register the chat-inputs cell");
        let session_id = jinn_domain::SessionId::new();
        cell.update(|inputs| {
            inputs
                .entry(session_id.clone())
                .or_default()
                .insert_text("draft");
        });
        assert_eq!(
            cell.read()
                .get(&session_id)
                .map(jinn_chat_input_msg::ChatInputBoxState::text),
            Some("draft"),
            "the cell must round-trip a per-session entry"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn per_session_inputs_are_isolated() {
        // Given an activated slice with two sessions in the cell.
        let slices = jinn_slices::Slices::new();
        let mut viewport = jinn_domain::common::slices::view::Viewport::new();
        let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
        let key_routes = jinn_slices::KeyRoutes::new();
        let services = jinn_domain::Services::new_fake().await;
        let mut host = SliceHost::new(
            &slices,
            &mut viewport,
            &overlay_views,
            &key_routes,
            &services.trouper_system,
        );
        crate::activate(&mut host);
        let cell = slices
            .reader::<jinn_chat_input_msg::ChatInputs>(&crate::chat_inputs_slot())
            .expect("activation must register the chat-inputs cell");
        let session_a = jinn_domain::SessionId::new();
        let session_b = jinn_domain::SessionId::new();

        // When writing a distinct draft per session.
        cell.update(|inputs| {
            inputs
                .entry(session_a.clone())
                .or_default()
                .insert_text("alpha");
            inputs
                .entry(session_b.clone())
                .or_default()
                .insert_text("beta");
        });

        // Then neither session observes the other's draft.
        let inputs = cell.read();
        assert_eq!(
            inputs
                .get(&session_a)
                .map(jinn_chat_input_msg::ChatInputBoxState::text),
            Some("alpha")
        );
        assert_eq!(
            inputs
                .get(&session_b)
                .map(jinn_chat_input_msg::ChatInputBoxState::text),
            Some("beta")
        );
    }
}
