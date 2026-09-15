//! The chat-log-view slice — per-session chat log display state.
//!
//! Owns one cell ([`chat_log_views_slot`]) holding
//! [`jinn_slices::ChatLogViews`]: each session's scroll intent and render
//! caches, cursor selection, expand/ignore sets, saved pins position, and
//! ignore-sweep. The kernel's exempt IntentHandler writes through
//! `ChatSession`'s semantic methods (a facade over the cell), and the chat
//! log renderer publishes its per-frame caches through the same methods.
//! There is no actor and no route row: the writers are the exempt sync
//! handler and the render pass, exactly as the migration docs prescribe.

pub mod state;

pub use jinn_slices::chat_log_views_slot;
pub use state::ChatLogViewUi;

use jinn_slices::SliceHost;

/// Activates the slice: mints the chat-log-views cell. No routes, no
/// actors, no view.
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
        .register_cell(chat_log_views_slot(), jinn_slices::ChatLogViews::new())
        .expect("chat-log-view slot is registered exactly once at wiring");
}

#[cfg(test)]
mod activation_tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use jinn_slices::SliceHost;

    #[rstest::rstest]
    #[tokio::test]
    async fn activate_registers_the_chat_log_views_cell() {
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
            .reader::<jinn_slices::ChatLogViews>(&crate::chat_log_views_slot())
            .expect("activation must register the chat-log-views cell");
        let session_id = jinn_domain::SessionId::new();
        cell.update(|views| {
            views.entry(session_id.clone()).or_default().scroll_offset = Some(3);
        });
        assert_eq!(
            cell.read().get(&session_id).and_then(|v| v.scroll_offset),
            Some(3),
            "the cell must round-trip a per-session entry"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn per_session_entries_are_isolated() {
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
            .reader::<jinn_slices::ChatLogViews>(&crate::chat_log_views_slot())
            .expect("activation must register the chat-log-views cell");
        let session_a = jinn_domain::SessionId::new();
        let session_b = jinn_domain::SessionId::new();

        // When writing a distinct scroll offset per session.
        cell.update(|views| {
            views.entry(session_a.clone()).or_default().scroll_offset = Some(3);
            views.entry(session_b.clone()).or_default().scroll_offset = Some(7);
        });

        // Then neither session observes the other's offset.
        let views = cell.read();
        assert_eq!(views.get(&session_a).and_then(|v| v.scroll_offset), Some(3));
        assert_eq!(views.get(&session_b).and_then(|v| v.scroll_offset), Some(7));
    }
}
