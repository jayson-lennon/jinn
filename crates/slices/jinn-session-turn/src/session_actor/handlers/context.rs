//! Context-related handlers - pinning, caching, and persona management.
//!
//! Handles entry pinning (PinChatEntry/UnpinChatEntry), prompt template
//! caching (PromptTemplatesLoaded), persona selection (PersonasLoaded), and
//! persona catalog refresh.

use jinn_core_types::DEFAULT_PERSONA_NAME;
use jinn_kernel::PromptTemplatesLoaded;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_persona_msg::PersonasLoaded;
use jinn_session_history_msg::ChatEntryPinChanged;
use jinn_session_history_msg::{PinChatEntry, UnpinChatEntry};

use super::super::SessionPersistenceActor;

/// Compute sorted pinned-entry IDs from a session, matching
/// `AppState::sorted_pinned_ids` but operating on a session directly so the
/// pins handler can run inside a [`SessionPinsView`] without `&AppState`.
///
/// The ids are the sidebar's display rows, so the unpin cursor clamps over
/// exactly the rows the user sees.
fn sorted_pinned_ids_from_session(
    session: &jinn_session_state::ChatSessionState,
) -> Vec<jinn_core_types::ChatEntryId> {
    use jinn_core_types::ChatEntryId;
    session
        .pinned_display_rows()
        .into_iter()
        .map(|row| row.entry_id)
        .collect::<Vec<ChatEntryId>>()
}

impl SessionPersistenceActor {
    /// PinChatEntry: pin entry in session.
    ///
    /// Pinning is an interaction: the session is marked interacted so the
    /// pin (and the entry it anchors) reaches the store — a pin on a
    /// brand-new, never-sent-to session would otherwise be silently dropped
    /// by the `is_persistable` guard.
    pub(in crate::session_actor) async fn handle_pin_chat_entry(&self, payload: &PinChatEntry) {
        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(&payload.session_id);
            session.pin_entry(&payload.entry_id, payload.position);
            session.mark_interacted();
        });
        self.publish(ChatEntryPinChanged {
            session_id: payload.session_id.clone(),
        })
        .await;
    }

    /// UnpinChatEntry: unpin entry in session.
    ///
    /// Like pinning, unpinning is an interaction and marks the session
    /// interacted so the removal persists.
    pub(in crate::session_actor) async fn handle_unpin_chat_entry(&self, payload: &UnpinChatEntry) {
        {
            self.state.with_session_pins(|view| {
                let is_active = view.session.map().active_session_id() == &payload.session_id;
                let old_index = if is_active {
                    view.frontend.with_sections(
                        |s| {
                            s.pins.selection_index(&sorted_pinned_ids_from_session(
                                view.session.map().active_session(),
                            ))
                        },
                        || 0,
                    )
                } else {
                    0
                };

                let session = view.session.map().get_or_create(&payload.session_id);
                session.unpin_entry(&payload.entry_id);
                session.mark_interacted();

                if is_active {
                    let new_sorted =
                        sorted_pinned_ids_from_session(view.session.map().active_session());
                    view.frontend
                        .update_sections(|s| s.pins.clamp_to_nearest(&new_sorted, old_index));
                }
            });
        }
        self.publish(ChatEntryPinChanged {
            session_id: payload.session_id.clone(),
        })
        .await;
    }

    /// No-op receiver for [`PromptTemplatesLoaded`].
    #[expect(
        clippy::unused_self,
        reason = "trait contract requires #[allow(clippy::unused_self)]self method"
    )]
    ///
    /// The session-init slice's discovery worker writes each session's
    /// discovered prompt set directly into that session's ephemeral state
    /// before emitting the event,
    /// so there is no global mirror to update. The handler exists only to keep
    /// the event dispatch arm explicit (and to make future per-session-side
    /// reactions easy to add).
    pub(in crate::session_actor) fn on_prompt_templates_loaded(
        &self,
        event: &PromptTemplatesLoaded,
    ) {
        tracing::trace!(
            session_id = %event.session_id,
            count = event.templates.len(),
            "prompt templates loaded for session (no global mirror)",
        );
    }

    /// Replaces the persona catalog and normalizes every live session's
    /// persona against it.
    ///
    /// The active session's profile persona is the only persona authority,
    /// so a landing catalog carries no selection to apply. What it does
    /// carry is the authority on which names exist: any session whose
    /// stored persona is absent from the scan is rewritten (default
    /// persona first, first-available backstop), keeping the invariant
    /// that no session holds a persona that does not exist. The rewrites
    /// ride each session's next stamped save — in-memory correctness is
    /// what the sidebar and context assembly read.
    pub(in crate::session_actor) fn on_personas_loaded(&self, payload: &PersonasLoaded) {
        if payload.error.is_some() {
            tracing::warn!(
                error = ?payload.error,
                "persona scan reported an error"
            );
            return;
        }

        // The persona catalog lives in the persona slice's cell.
        let Some(cell) = self
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
        else {
            tracing::warn!("persona slice not activated; dropping PersonasLoaded catalog");
            return;
        };
        cell.update(|personas| personas.replace_entries(payload.personas.clone()));

        // Every live session whose persona stopped resolving is rewritten.
        let unknown: Vec<jinn_core_types::SessionId> = {
            let state = self.state.read();
            state
                .session
                .iter()
                .filter(|(_, session)| cell.read().persona(session.persona_name()).is_none())
                .map(|(id, _)| id.clone())
                .collect()
        };
        if unknown.is_empty() {
            return;
        }
        self.state.with_session(|view| {
            for session_id in unknown {
                let Some(session) = view.session.map().get_mut(&session_id) else {
                    continue;
                };
                let normalized = cell
                    .read()
                    .normalize_name(session.persona_name(), DEFAULT_PERSONA_NAME);
                session.set_persona_name(normalized);
            }
        });
    }
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

    use super::super::super::helpers::test_actor_with_store_recording;
    use super::*;
    use jinn_core_types::SessionId;
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::services::BusAudit;
    use jinn_kernel::common::state::State;
    use jinn_kernel::protocol::{ChatEntryId, PinPosition};
    use jinn_slices::Persona;

    fn make_persona(name: &str) -> Persona {
        Persona {
            name: name.to_owned(),
            description: String::new(),
            body: String::new(),
        }
    }

    async fn create_actor() -> (
        super::super::super::SessionPersistenceActor,
        State,
        BusAudit,
    ) {
        let state = State::new(AppState::default_with_scope_focus());
        let (actor, audit) = super::super::super::helpers::test_actor_recording().await;
        let actor = super::super::super::SessionPersistenceActor {
            state: state.clone(),
            ..actor
        };
        (actor, state, audit)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_personas_loaded_replaces_the_catalog() {
        // Given a session actor whose personas cell holds a stale catalog.
        let (actor, _state, _audit) = create_actor().await;
        actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded")
            .update(|p| p.replace_entries(vec![make_persona("stale")]));
        let payload = PersonasLoaded {
            personas: vec![make_persona("fresh")],
            error: None,
        };

        // When receiving PersonasLoaded.
        actor.on_personas_loaded(&payload);

        // Then the cell holds exactly the scanned catalog.
        let cell = actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded");
        let names: Vec<String> = cell.read().entries.iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, vec!["fresh".to_owned()]);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn personas_loaded_normalizes_unknown_session_persona() {
        // Given a session actor whose active session carries a persona the
        // scan no longer finds (deleted from disk while jinn was closed).
        let (actor, state, _audit) = create_actor().await;
        state
            .write()
            .active_session_mut()
            .set_persona_name("learning-tutor".to_owned());
        let payload = PersonasLoaded {
            personas: vec![make_persona("coding-assistant")],
            error: None,
        };

        // When receiving PersonasLoaded.
        actor.on_personas_loaded(&payload);

        // Then the session is rewritten to the default persona.
        assert_eq!(
            state.read().active_session().persona_name(),
            "coding-assistant"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn personas_loaded_leaves_known_session_personas_alone() {
        // Given a session actor whose active session carries a persona the
        // scan does find.
        let (actor, state, _audit) = create_actor().await;
        state
            .write()
            .active_session_mut()
            .set_persona_name("learning-tutor".to_owned());
        let payload = PersonasLoaded {
            personas: vec![
                make_persona("coding-assistant"),
                make_persona("learning-tutor"),
            ],
            error: None,
        };

        // When receiving PersonasLoaded.
        actor.on_personas_loaded(&payload);

        // Then the session keeps its persona.
        assert_eq!(
            state.read().active_session().persona_name(),
            "learning-tutor"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_personas_loaded_skips_a_failed_scan() {
        // Given a session actor whose active session carries an unknown persona.
        let (actor, state, _audit) = create_actor().await;
        state
            .write()
            .active_session_mut()
            .set_persona_name("learning-tutor".to_owned());
        let payload = PersonasLoaded {
            personas: vec![],
            error: Some("scan failed".to_owned()),
        };

        // When receiving PersonasLoaded with an error.
        actor.on_personas_loaded(&payload);

        // Then the session is untouched — a failed scan is not a catalog.
        assert_eq!(
            state.read().active_session().persona_name(),
            "learning-tutor"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_pin_chat_entry_pins_and_emits() {
        // Given a session with a user entry.
        let (actor, state, audit) = create_actor().await;
        let entry_id = {
            let mut guard = state.write();
            let session = guard.active_session_mut();
            let entry = jinn_core_types::ChatEntry::user("hello");
            let id = entry.id.clone();
            session.push_entry(entry);
            id
        };
        let session_id = state.read().session.active_session_id().clone();

        // When pinning the entry.
        actor
            .handle_pin_chat_entry(&PinChatEntry {
                session_id: session_id.clone(),
                entry_id: entry_id.clone(),
                position: PinPosition::Top,
            })
            .await;

        // Then the entry is pinned.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        let entry = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("entry");
        assert!(entry.is_pinned(), "expected entry to be pinned");
        drop(guard);

        // And ChatEntryPinChanged event was emitted.
        assert!(
            audit.contains_name("ChatEntryPinChanged"),
            "expected ChatEntryPinChanged event"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_unpin_chat_entry_unpins_and_emits() {
        // Given a session with a pinned entry.
        let (actor, state, audit) = create_actor().await;
        let entry_id = {
            let mut guard = state.write();
            let session = guard.active_session_mut();
            let mut entry = jinn_core_types::ChatEntry::user("hello");
            entry.pin_position = Some(PinPosition::Top);
            let id = entry.id.clone();
            session.push_entry(entry);
            id
        };
        let session_id = state.read().session.active_session_id().clone();

        // When unpinning the entry.
        actor
            .handle_unpin_chat_entry(&UnpinChatEntry {
                session_id: session_id.clone(),
                entry_id: entry_id.clone(),
            })
            .await;

        // Then the entry is no longer pinned.
        let guard = state.read();
        let session = guard.session.get(&session_id).expect("session");
        let entry = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("entry");
        assert!(!entry.is_pinned(), "expected entry to be unpinned");
        drop(guard);

        // And ChatEntryPinChanged event was emitted.
        assert!(
            audit.contains_name("ChatEntryPinChanged"),
            "expected ChatEntryPinChanged event"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn pinning_entry_persists_previously_unsaved_session() {
        // Given a non-interacted (not-yet-persistable) session with an entry.
        let (actor, store, _audit) = test_actor_with_store_recording(vec![]).await;
        let session_id = actor.state.read().session.active_session_id().clone();
        let entry_id = {
            let mut guard = actor.state.write();
            let session = guard.active_session_mut();
            let entry = jinn_core_types::ChatEntry::user("hello");
            let id = entry.id.clone();
            session.push_entry(entry);
            id
        };
        assert!(
            store.last_saved_session(&session_id).is_none(),
            "test setup: nothing saved yet"
        );

        // When pinning the entry, then the actor's ChatEntryPinChanged
        // reaction (persist) runs - the same save the bus dispatch triggers.
        actor
            .handle_pin_chat_entry(&PinChatEntry {
                session_id: session_id.clone(),
                entry_id,
                position: PinPosition::Top,
            })
            .await;
        actor.save_active_session(&session_id).await;

        // Then the session reaches the store (the pin persists).
        assert!(
            store.last_saved_session(&session_id).is_some(),
            "pinning a chat entry must persist the session"
        );
    }

    /// Pushes `n` pinned entries (all `Top`, so display order = insertion order)
    /// into the active session and returns their IDs in insertion order.
    fn push_pinned_entries(state: &State, n: usize) -> Vec<ChatEntryId> {
        let mut guard = state.write();
        let session = guard.active_session_mut();
        (0..n)
            .map(|i| {
                let mut entry = jinn_core_types::ChatEntry::user(format!("entry {i}"));
                entry.pin_position = Some(PinPosition::Top);
                let id = entry.id.clone();
                session.push_entry(entry);
                id
            })
            .collect()
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unpin_keeps_cursor_position_when_first_removed() {
        // Given 3 pinned entries [A, B, C] with A selected.
        let (actor, state, _audit) = create_actor().await;
        let ids = push_pinned_entries(&state, 3);
        let session_id = state.read().session.active_session_id().clone();
        state.write().frontend.update_sections(|s| {
            s.pins.select_by_id(ids[0].clone());
        });

        // When unpinning A.
        actor
            .handle_unpin_chat_entry(&UnpinChatEntry {
                session_id,
                entry_id: ids[0].clone(),
            })
            .await;

        // Then the cursor lands on B (now at index 0).
        assert_eq!(
            state
                .read()
                .frontend
                .with_sections(|s| s.pins.selected_id().cloned(), || None),
            Some(ids[1].clone())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unpin_keeps_cursor_position_when_middle_removed() {
        // Given 3 pinned entries [A, B, C] with B selected.
        let (actor, state, _audit) = create_actor().await;
        let ids = push_pinned_entries(&state, 3);
        let session_id = state.read().session.active_session_id().clone();
        state.write().frontend.update_sections(|s| {
            s.pins.select_by_id(ids[1].clone());
        });

        // When unpinning B.
        actor
            .handle_unpin_chat_entry(&UnpinChatEntry {
                session_id,
                entry_id: ids[1].clone(),
            })
            .await;

        // Then the cursor lands on C (now at index 1).
        assert_eq!(
            state
                .read()
                .frontend
                .with_sections(|s| s.pins.selected_id().cloned(), || None),
            Some(ids[2].clone())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unpin_keeps_cursor_when_a_different_entry_is_removed() {
        // Given 3 pinned entries [A, B, C] with B selected.
        let (actor, state, _audit) = create_actor().await;
        let ids = push_pinned_entries(&state, 3);
        let session_id = state.read().session.active_session_id().clone();
        let selected = ids[1].clone();
        state.write().frontend.update_sections(|s| {
            s.pins.select_by_id(selected.clone());
        });

        // When unpinning A (a different, non-selected entry).
        actor
            .handle_unpin_chat_entry(&UnpinChatEntry {
                session_id,
                entry_id: ids[0].clone(),
            })
            .await;

        // Then the cursor stays on B (its ID is still present).
        assert_eq!(
            state
                .read()
                .frontend
                .with_sections(|s| s.pins.selected_id().cloned(), || None),
            Some(selected)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unpin_clamps_cursor_to_new_last_when_last_removed() {
        // Given 3 pinned entries [A, B, C] with C selected.
        let (actor, state, _audit) = create_actor().await;
        let ids = push_pinned_entries(&state, 3);
        let session_id = state.read().session.active_session_id().clone();
        state.write().frontend.update_sections(|s| {
            s.pins.select_by_id(ids[2].clone());
        });

        // When unpinning C.
        actor
            .handle_unpin_chat_entry(&UnpinChatEntry {
                session_id,
                entry_id: ids[2].clone(),
            })
            .await;

        // Then the cursor clamps to B (the new last entry).
        assert_eq!(
            state
                .read()
                .frontend
                .with_sections(|s| s.pins.selected_id().cloned(), || None),
            Some(ids[1].clone())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unpin_clears_cursor_when_only_pin_removed() {
        // Given 1 pinned entry [A] with A selected.
        let (actor, state, _audit) = create_actor().await;
        let ids = push_pinned_entries(&state, 1);
        let session_id = state.read().session.active_session_id().clone();
        state.write().frontend.update_sections(|s| {
            s.pins.select_by_id(ids[0].clone());
        });

        // When unpinning A.
        actor
            .handle_unpin_chat_entry(&UnpinChatEntry {
                session_id,
                entry_id: ids[0].clone(),
            })
            .await;

        // Then the cursor is cleared.
        assert!(
            state
                .read()
                .frontend
                .with_sections(|s| s.pins.selected_id().is_none(), || true)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unpin_for_non_active_session_leaves_cursor_unchanged() {
        // Given 3 pinned entries [A, B, C] with B selected, and an unrelated session.
        let (actor, state, _audit) = create_actor().await;
        let ids = push_pinned_entries(&state, 3);
        let selected = ids[1].clone();
        state.write().frontend.update_sections(|s| {
            s.pins.select_by_id(selected.clone());
        });

        // When unpinning B under a non-active session id.
        actor
            .handle_unpin_chat_entry(&UnpinChatEntry {
                session_id: SessionId::new(),
                entry_id: ids[1].clone(),
            })
            .await;

        // Then the cursor is unchanged.
        assert_eq!(
            state
                .read()
                .frontend
                .with_sections(|s| s.pins.selected_id().cloned(), || None),
            Some(selected)
        );
    }
}
