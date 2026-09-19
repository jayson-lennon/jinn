//! Context-related handlers - pinning, caching, and persona management.
//!
//! Handles entry pinning (PinChatEntry/UnpinChatEntry), tool definition caching
//! (ToolsRegistered), prompt template caching (PromptTemplatesLoaded),
//! persona selection (PersonasLoaded), and persona picker population
//! (LoadPersonaPickerEntries).
//!
//! Relocated from `PromptAssemblyActor` - these concerns are session-related
//! mutations of `AppState`, not part of prompt assembly.

use crate::common::actor_deps::BusPublish;
use crate::feat::context::protocol::command::LoadPersonaPickerEntries;
use crate::feat::persona::PersonaEntry;
use crate::feat::provider::protocol::event::PromptTemplatesLoaded;
use crate::feat::session::profile::DEFAULT_PERSONA_NAME;
use jinn_session_history_msg::ChatEntryPinChanged;
use jinn_session_history_msg::{PinChatEntry, UnpinChatEntry};
use jinn_tools_msg::{ToolsRegistered, ToolsUnregistered};

use super::super::SessionPersistenceActor;

/// Compute sorted pinned-entry IDs from a session, matching
/// `AppState::sorted_pinned_ids` but operating on a session directly so the
/// pins handler can run inside a [`SessionPinsView`] without `&AppState`.
fn sorted_pinned_ids_from_session(
    session: &crate::feat::session::chat_session::ChatSessionState,
) -> Vec<crate::protocol::ChatEntryId> {
    use crate::common::app_state::pin_sort_key;
    use crate::protocol::ChatEntryId;
    let mut pinned = session.pinned_entries();
    pinned.sort_by_key(|entry| pin_sort_key(entry.pin_position));
    pinned
        .iter()
        .map(|e| e.id.clone())
        .collect::<Vec<ChatEntryId>>()
}

impl SessionPersistenceActor {
    /// PinChatEntry: pin entry in session.
    pub(in crate::feat::session::session_actor) async fn handle_pin_chat_entry(
        &self,
        payload: &PinChatEntry,
    ) {
        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(&payload.session_id);
            session.pin_entry(&payload.entry_id, payload.position);
        });
        self.publish(ChatEntryPinChanged {
            session_id: payload.session_id.clone(),
        })
        .await;
    }

    /// UnpinChatEntry: unpin entry in session.
    pub(in crate::feat::session::session_actor) async fn handle_unpin_chat_entry(
        &self,
        payload: &UnpinChatEntry,
    ) {
        {
            self.state
                .with_session_pins(&self.cap, &self.frontend_cap, |view| {
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

    /// ToolsRegistered: cache tool definitions in global or per-session map.
    pub(in crate::feat::session::session_actor) fn on_tools_registered(
        &self,
        evt: &ToolsRegistered,
    ) {
        let Some(cell) = self
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
        else {
            tracing::warn!("tools registry cell missing; dropping ToolsRegistered");
            return;
        };

        match &evt.session_id {
            // Global tools (builtins) -> shared map.
            None => {
                cell.update(|registry| {
                    for def in &evt.definitions {
                        registry.global.insert(def.name.clone(), def.clone());
                    }
                });
            }
            // Session-scoped tools -> per-session map.
            Some(target_id) => {
                cell.update(|registry| {
                    let session_map = registry.session.entry(target_id.clone()).or_default();
                    for def in &evt.definitions {
                        session_map.insert(def.name.clone(), def.clone());
                    }
                });
            }
        }
    }

    /// ToolsUnregistered: prune a provider's session-scoped tools from the
    /// context cache so the LLM stops seeing them (e.g. an MCP server was
    /// disabled, or its actor tore down on close/restart).
    pub(in crate::feat::session::session_actor) fn on_tools_unregistered(
        &self,
        evt: &ToolsUnregistered,
    ) {
        // Tool names are "<provider><tool>" — the provider string already
        // carries its trailing "__" separator (e.g. `mcp__stub__echo`), so a
        // plain provider-prefix match never over-matches `stub_extended`.
        let prefix = evt.provider.clone();
        if let Some(cell) = self
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
        {
            cell.update(|registry| {
                let Some(map) = registry.session.get_mut(&evt.session_id) else {
                    return;
                };
                map.retain(|name, _| !name.starts_with(&prefix));
                if map.is_empty() {
                    registry.session.remove(&evt.session_id);
                }
            });
        }
    }

    /// SessionClosed cleanup: drop the closed session's entry from the
    /// context tool cache so it does not leak across the app's lifetime
    /// (the orchestrator prunes its own routing map separately).
    pub(in crate::feat::session::session_actor) fn on_session_closed_cleanup(
        &self,
        session_id: &crate::protocol::SessionId,
    ) {
        if let Some(cell) = self
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
        {
            cell.update(|registry| {
                registry.session.remove(session_id);
            });
        }
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
    pub(in crate::feat::session::session_actor) fn on_prompt_templates_loaded(
        &self,
        event: &PromptTemplatesLoaded,
    ) {
        tracing::trace!(
            session_id = %event.session_id,
            count = event.templates.len(),
            "prompt templates loaded for session (no global mirror)",
        );
    }

    /// Stores loaded personas in state and selects the active persona.
    ///
    /// Priority:
    /// 1. Honor state.persona_name if set and found in list.
    /// 2. Keep current active_persona if it still exists in the new list.
    /// 3. Fallback to `"coding-assistant"` by name.
    /// 4. If coding-assistant not found, pick first available.
    pub(in crate::feat::session::session_actor) fn on_personas_loaded(
        &self,
        payload: &crate::feat::context::protocol::event::PersonasLoaded,
    ) {
        if payload.error.is_some() {
            tracing::warn!(
                error = ?payload.error,
                "persona scan reported an error"
            );
            return;
        }

        // Read frontend.app_state.persona_name (if set) before writing.
        let seeded_persona_name = self.state.read().frontend.app_state.persona_name.clone();

        // The persona catalog lives in the persona slice's cell now; the
        // actor only carries authority to write the selection it was
        // seeded with.
        let Some(cell) = self
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
        else {
            tracing::warn!("persona slice not activated; dropping PersonasLoaded catalog");
            return;
        };
        cell.update(|personas| {
            personas.seeded_replace(
                payload.personas.clone(),
                seeded_persona_name.as_deref(),
                DEFAULT_PERSONA_NAME,
            );
        });
    }

    /// Loads persona picker entries into `AppState`.
    pub(in crate::feat::session::session_actor) fn handle_load_persona_picker_entries(
        &self,
        _payload: &LoadPersonaPickerEntries,
    ) {
        let state = self.state.read();
        let selection = self
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .map(|cell| cell.read().clone());
        drop(state);
        let (_active_name, mut entries): (Option<String>, Vec<PersonaEntry>) = match selection {
            Some(selection) => {
                let active_name = selection.active.clone();
                let theme = self.state.read().frontend.theme.clone();
                let entries = selection
                    .entries
                    .iter()
                    .map(|p| PersonaEntry {
                        name: p.name.clone(),
                        description: p.description.clone(),
                        is_active: active_name.as_ref() == Some(&p.name),
                        theme: theme.clone(),
                    })
                    .collect();
                (active_name, entries)
            }
            None => (None, Vec::new()),
        };

        entries.sort_by_key(|e| e.name.to_lowercase());

        self.state
            .with_persona_picker(&self.frontend_cap, |picker| {
                picker.set_items(entries);
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

    use super::*;
    use crate::common::app_state::AppState;
    use crate::common::services::BusAudit;
    use crate::common::state::State;
    use crate::feat::context::protocol::event::PersonasLoaded;
    use crate::feat::persona::Persona;
    use crate::feat::ui::picker_states::PickerExt;
    use crate::protocol::{ChatEntryId, PinPosition, SessionId};
    use jinn_core_types::tool_types::ToolDefinition;

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
    async fn on_tools_registered_keeps_regular_tools_in_global_map() {
        // Given a session actor.
        let (actor, _state, _audit) = create_actor().await;

        // Build a ToolsRegistered with a builtin-shaped tool definition.
        let definitions = vec![ToolDefinition {
            name: "bash".to_owned(),
            description: "Run a shell command".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: None,
        }];
        let payload = ToolsRegistered {
            provider: "builtin".to_owned(),
            definitions,
            session_id: None,
        };

        // When processing the event.
        actor.on_tools_registered(&payload);

        // Then regular tools are in the global map.
        let registry = actor
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
            .expect("tools registry cell seeded");
        assert!(
            registry.read().global.contains_key("bash"),
            "bash should be in global tool map"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tools_registered_ignores_attached_tools_for_different_session() {
        // Given a session actor.
        let (actor, _state, _audit) = create_actor().await;
        // Build a ToolsRegistered targeting a different session.
        let other_session_id = SessionId::new();
        let payload = ToolsRegistered {
            provider: "tester:alpha".to_owned(),
            definitions: vec![ToolDefinition {
                name: "judgment_passed".to_owned(),
                description: "Pass".to_owned(),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                parameters: serde_json::json!({"type": "object", "properties": {}}),
                server_tool_type: None,
            }],
            session_id: Some(other_session_id.clone()),
        };

        // When processing the event.
        actor.on_tools_registered(&payload);

        // Then the tool is NOT in any global map.
        let registry = actor
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
            .expect("tools registry cell seeded");
        let registry_guard = registry.read();
        assert!(
            !registry_guard.global.contains_key("judgment_passed"),
            "attached tool for different session should not be in global map"
        );
        // And NOT in the target session's map either (it was stored by session_id key).
        // Since the tool WAS stored in session_tool_definitions[other_session_id],
        // it should be there, not in global.
        let session_tools = registry_guard.session.get(&other_session_id);
        // The tool IS stored under the correct session key (that's the new behavior).
        assert!(
            session_tools.is_some_and(|m| m.contains_key("judgment_passed")),
            "attached tool should be stored under its target session key"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tools_registered_stores_attached_tools_for_own_session() {
        // Given a session actor.
        let (actor, state, _audit) = create_actor().await;
        let session_id = state.read().session.active_session_id().clone();

        // Build a ToolsRegistered targeting this session.
        let payload = ToolsRegistered {
            provider: "tester:alpha".to_owned(),
            definitions: vec![ToolDefinition {
                name: "judgment_passed".to_owned(),
                description: "Pass".to_owned(),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                parameters: serde_json::json!({"type": "object", "properties": {}}),
                server_tool_type: None,
            }],
            session_id: Some(session_id.clone()),
        };

        // When processing the event.
        actor.on_tools_registered(&payload);

        // Then the tool IS stored in the session-specific map.
        let registry = actor
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
            .expect("tools registry cell seeded");
        let registry_guard = registry.read();
        let session_tools = registry_guard.session.get(&session_id);
        assert!(
            session_tools.is_some_and(|m| m.contains_key("judgment_passed")),
            "attached tool for own session should be stored in session map"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tools_unregistered_prunes_the_providers_tools_from_the_context_cache() {
        // Given a session actor with two MCP providers' tools cached for one
        // session (stub's echo + other's tool).
        let (actor, _state, _audit) = create_actor().await;
        let session_id = SessionId::new();
        for (provider, tool_name) in [
            ("mcp__stub__", "mcp__stub__echo"),
            ("mcp__other__", "mcp__other__tool"),
        ] {
            actor.on_tools_registered(&ToolsRegistered {
                provider: provider.to_owned(),
                definitions: vec![ToolDefinition {
                    name: tool_name.to_owned(),
                    description: String::new(),
                    prompt_snippet: None,
                    prompt_guidelines: vec![],
                    parameters: serde_json::json!({"type": "object", "properties": {}}),
                    server_tool_type: None,
                }],
                session_id: Some(session_id.clone()),
            });
        }

        // When the stub provider unregisters its tools.
        actor.on_tools_unregistered(&ToolsUnregistered {
            provider: "mcp__stub__".to_owned(),
            session_id: session_id.clone(),
        });

        // Then only the other provider's tool remains cached.
        let registry = actor
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
            .expect("tools registry cell seeded");
        let registry_guard = registry.read();
        let session_tools = registry_guard
            .session
            .get(&session_id)
            .expect("session map must survive while another provider's tools remain");
        assert!(
            !session_tools.contains_key("mcp__stub__echo"),
            "stub's tool must be pruned"
        );
        assert!(
            session_tools.contains_key("mcp__other__tool"),
            "other provider's tool must survive"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tools_unregistered_drops_the_session_map_when_it_empties() {
        // Given a session actor with one provider's tool cached.
        let (actor, _state, _audit) = create_actor().await;
        let session_id = SessionId::new();
        actor.on_tools_registered(&ToolsRegistered {
            provider: "mcp__stub__".to_owned(),
            definitions: vec![ToolDefinition {
                name: "mcp__stub__echo".to_owned(),
                description: String::new(),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                parameters: serde_json::json!({"type": "object", "properties": {}}),
                server_tool_type: None,
            }],
            session_id: Some(session_id.clone()),
        });

        // When that provider unregisters.
        actor.on_tools_unregistered(&ToolsUnregistered {
            provider: "mcp__stub__".to_owned(),
            session_id: session_id.clone(),
        });

        // Then the emptied session map is removed entirely.
        let registry = actor
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
            .expect("tools registry cell seeded");
        assert!(
            !registry.read().session.contains_key(&session_id),
            "emptied session map must be dropped"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tools_unregistered_spares_similarly_named_servers() {
        // Given a session actor with tools from "stub" and "stub_extended".
        let (actor, _state, _audit) = create_actor().await;
        let session_id = SessionId::new();
        for (provider, tool_name) in [
            ("mcp__stub__", "mcp__stub__echo"),
            ("mcp__stub_extended__", "mcp__stub_extended__echo"),
        ] {
            actor.on_tools_registered(&ToolsRegistered {
                provider: provider.to_owned(),
                definitions: vec![ToolDefinition {
                    name: tool_name.to_owned(),
                    description: String::new(),
                    prompt_snippet: None,
                    prompt_guidelines: vec![],
                    parameters: serde_json::json!({"type": "object", "properties": {}}),
                    server_tool_type: None,
                }],
                session_id: Some(session_id.clone()),
            });
        }

        // When only "stub" unregisters.
        actor.on_tools_unregistered(&ToolsUnregistered {
            provider: "mcp__stub__".to_owned(),
            session_id: session_id.clone(),
        });

        // Then "stub_extended"'s tool survives (prefix match includes the
        // trailing "__" separator).
        let registry = actor
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
            .expect("tools registry cell seeded");
        let registry_guard = registry.read();
        let session_tools = registry_guard
            .session
            .get(&session_id)
            .expect("session map must survive");
        assert!(
            session_tools.contains_key("mcp__stub_extended__echo"),
            "similarly-named server's tool must survive"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_closed_removes_the_sessions_context_tool_cache() {
        // Given a session actor with a session-scoped tool cached.
        let (actor, _state, _audit) = create_actor().await;
        let session_id = SessionId::new();
        actor.on_tools_registered(&ToolsRegistered {
            provider: "mcp__stub__".to_owned(),
            definitions: vec![ToolDefinition {
                name: "mcp__stub__echo".to_owned(),
                description: String::new(),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                parameters: serde_json::json!({"type": "object", "properties": {}}),
                server_tool_type: None,
            }],
            session_id: Some(session_id.clone()),
        });

        // When the session closes (the SessionClosed cleanup path).
        actor.on_session_closed_cleanup(&session_id);

        // Then the session's context-cache entry is gone.
        let registry = actor
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
            .expect("tools registry cell seeded");
        assert!(
            !registry.read().session.contains_key(&session_id),
            "closed session's context tool cache must be removed"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tools_registered_stores_global_tools_unconditionally() {
        // Given a session actor.
        let (actor, _state, _audit) = create_actor().await;
        // Given a session actor.

        // Build a global ToolsRegistered (session_id: None).
        let payload = ToolsRegistered {
            provider: "tester:beta".to_owned(),
            definitions: vec![ToolDefinition {
                name: "global_helper".to_owned(),
                description: "Help".to_owned(),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                parameters: serde_json::json!({"type": "object", "properties": {}}),
                server_tool_type: None,
            }],
            session_id: None,
        };

        // When processing the event.
        actor.on_tools_registered(&payload);

        // Then the global tool is stored.
        let registry = actor
            .services
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
            .expect("tools registry cell seeded");
        assert!(
            registry.read().global.contains_key("global_helper"),
            "global tool should be stored unconditionally"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_personas_loaded_selects_coding_assistant_when_none_active() {
        // Given a session actor with no active persona.
        let (actor, _state, _audit) = create_actor().await;
        let personas = vec![
            make_persona("learning-tutor"),
            make_persona("coding-assistant"),
        ];
        let payload = PersonasLoaded {
            personas,
            error: None,
        };

        // When receiving PersonasLoaded.
        actor.on_personas_loaded(&payload);

        // Then coding-assistant is selected by name, not position.
        let selection = actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded");
        let selection_guard = selection.read();
        assert_eq!(selection_guard.active.as_deref(), Some("coding-assistant"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_personas_loaded_keeps_existing_active_persona() {
        // Given a session actor with active persona "learning-tutor".
        let (actor, _state, _audit) = create_actor().await;
        actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded")
            .update(|p| p.active = Some("learning-tutor".to_owned()));
        let personas = vec![
            make_persona("coding-assistant"),
            make_persona("learning-tutor"),
        ];
        let payload = PersonasLoaded {
            personas,
            error: None,
        };

        // When receiving PersonasLoaded.
        actor.on_personas_loaded(&payload);

        // Then learning-tutor is kept (still exists in list).
        let selection = actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded");
        assert_eq!(selection.read().active.as_deref(), Some("learning-tutor"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_personas_loaded_falls_back_when_active_missing() {
        // Given a session actor where active persona "foo" was deleted from disk.
        let (actor, _state, _audit) = create_actor().await;
        actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded")
            .update(|p| p.active = Some("foo".to_owned()));
        let personas = vec![make_persona("coding-assistant")];
        let payload = PersonasLoaded {
            personas,
            error: None,
        };

        // When receiving PersonasLoaded.
        actor.on_personas_loaded(&payload);

        // Then falls back to coding-assistant.
        let selection = actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded");
        let selection_guard = selection.read();
        assert_eq!(selection_guard.active.as_deref(), Some("coding-assistant"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_personas_loaded_uses_first_when_coding_assistant_missing() {
        // Given a session actor with no coding-assistant in the scanned list.
        let (actor, _state, _audit) = create_actor().await;
        let personas = vec![make_persona("learning-tutor")];
        let payload = PersonasLoaded {
            personas,
            error: None,
        };

        // When receiving PersonasLoaded.
        actor.on_personas_loaded(&payload);

        // Then first available is selected.
        let selection = actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded");
        assert_eq!(selection.read().active.as_deref(), Some("learning-tutor"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_personas_loaded_clears_active_when_list_empty() {
        // Given a session actor with some active persona.
        let (actor, _state, _audit) = create_actor().await;
        actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded")
            .update(|p| p.active = Some("foo".to_owned()));
        let payload = PersonasLoaded {
            personas: vec![],
            error: None,
        };

        // When receiving PersonasLoaded with empty list.
        actor.on_personas_loaded(&payload);

        // Then active_persona is None.
        let selection = actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded");
        let selection_guard = selection.read();
        assert!(selection_guard.active.is_none());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_personas_loaded_resolves_seeded_persona_name() {
        // Given a session actor with a persisted persona_name in frontend.app_state
        // (the value that on_environment_loaded seeds from state.toml at startup).
        let (actor, state, _audit) = create_actor().await;
        {
            let mut guard = state.write_test_no_cap();
            guard.frontend.app_state.persona_name = Some("general".to_owned());
        }
        let payload = PersonasLoaded {
            personas: vec![make_persona("coding-assistant"), make_persona("general")],
            error: None,
        };

        // When receiving PersonasLoaded.
        actor.on_personas_loaded(&payload);

        // Then the seeded persona_name wins over the coding-assistant default.
        let selection = actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded");
        assert_eq!(selection.read().active.as_deref(), Some("general"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_pin_chat_entry_pins_and_emits() {
        // Given a session with a user entry.
        let (actor, state, audit) = create_actor().await;
        let entry_id = {
            let mut guard = state.write_test_no_cap();
            let session = guard.active_session_mut();
            let entry = crate::protocol::ChatEntry::user("hello");
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
            let mut guard = state.write_test_no_cap();
            let session = guard.active_session_mut();
            let mut entry = crate::protocol::ChatEntry::user("hello");
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

    /// Pushes `n` pinned entries (all `Top`, so display order = insertion order)
    /// into the active session and returns their IDs in insertion order.
    fn push_pinned_entries(state: &State, n: usize) -> Vec<ChatEntryId> {
        let mut guard = state.write_test_no_cap();
        let session = guard.active_session_mut();
        (0..n)
            .map(|i| {
                let mut entry = crate::protocol::ChatEntry::user(format!("entry {i}"));
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
        state.write_test_no_cap().frontend.update_sections(|s| {
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
        state.write_test_no_cap().frontend.update_sections(|s| {
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
        state.write_test_no_cap().frontend.update_sections(|s| {
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
        state.write_test_no_cap().frontend.update_sections(|s| {
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
        state.write_test_no_cap().frontend.update_sections(|s| {
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
        state.write_test_no_cap().frontend.update_sections(|s| {
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

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_load_persona_picker_entries_populates_picker() {
        // Given a session actor with personas loaded.
        let (actor, state, _audit) = create_actor().await;
        actor
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("personas cell seeded")
            .update(|p| {
                p.entries = vec![
                    make_persona("coding-assistant"),
                    make_persona("learning-tutor"),
                ];
                p.active = Some("learning-tutor".to_owned());
            });

        // When loading persona picker entries.
        actor.handle_load_persona_picker_entries(&LoadPersonaPickerEntries);

        // Then the picker has entries with correct active state.
        let guard = state.read();
        let items = guard.frontend.persona_picker().items();
        assert_eq!(items.len(), 2, "expected 2 persona entries");
        let active = items
            .iter()
            .find(|e| e.entry().is_active)
            .expect("an active entry");
        assert_eq!(active.entry().name, "learning-tutor");
    }
}
