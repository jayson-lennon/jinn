//! Prompt assembly — the pure core of the context-assembly service.
//!
//! [`assemble`] turns caller-provided [`AssemblyInputs`] into an
//! [`AssembledPrompt`] in a single pass: splits pinned entries, builds
//! the system prompt from per-section builders, converts history to
//! messages, and counts tokens. Reads no shared state.
//!
//! The section builders live in the kernel (`env_context`,
//! `tool_prompt`, `skills::format`) and are consumed through the
//! slice's justified kernel dependency.

use std::collections::BTreeMap;

use jinn_context::env_context::{
    context_files_section, cwd_section, date_section, persona_section,
};
use jinn_context_assembly_msg::AssemblyInputs;
use jinn_core_types::LlmMessage;
use jinn_core_types::ToolDefinition;
use jinn_kernel::protocol::{ChatEntry, PinPosition, entries_to_messages};
use jinn_llm_support::token_estimator::{IMAGE_ATTACHMENT_TOKENS, TokenCounter};
use jinn_llm_support::tool_prompt::build_tool_context_block;
use jinn_skills::format_skills_for_prompt;
use jinn_slices::AssembledPrompt;
use jinn_slices::SystemPrompt;

/// Assembles a complete LLM prompt from caller-provided inputs, in a
/// single pure pass.
///
/// Reads nothing from any shared state: every field comes from
/// [`AssemblyInputs`]. A caller that wants state-derived inputs builds them
/// first — see [`crate::inputs::build_assembly_inputs`] — so a missing
/// session surfaces there, where it can be handled, rather than here.
///
/// # Assembly pipeline
///
/// 1. Split history into TOP/BOTTOM pins and working history.
/// 2. Compose the system prompt from per-section builders.
/// 3. Convert history (pins and working) to messages via [`entries_to_messages`].
/// 4. Re-inject pins in correct positions.
/// 5. Count tokens in the system prompt and all assembled messages.
/// 6. Return [`AssembledPrompt`].
///
/// This function does not panic; it cannot fail.
///
/// [`entries_to_messages`]: jinn_kernel::protocol::entries_to_messages
pub fn assemble(inputs: &AssemblyInputs, counter: &dyn TokenCounter) -> AssembledPrompt {
    let AssemblyInputs {
        session_id,
        cwd,
        persona,
        history,
        tools,
        tool_filter,
        provider_name,
        skills,
        skill_filter,
        loaded_skills,
        context_files,
    } = inputs;

    let persona = persona.as_ref();

    let mut tool_defs: Vec<ToolDefinition> = tools.clone();

    // Two independent gates, one retain: the session's filter, and the
    // provider gate for server tools. Kept as a single conjunction so the
    // context block is built from exactly the set these definitions came
    // from — folding them into one predicate would need the provider name
    // here and at dispatch, where it is not always in hand.
    tool_defs.retain(|def| {
        tool_filter
            .as_ref()
            .is_none_or(|filter| filter.permits(&def.name))
            && def.available_for_provider(provider_name)
    });

    let filtered_map: BTreeMap<String, ToolDefinition> = tool_defs
        .iter()
        .cloned()
        .map(|def| (def.name.clone(), def))
        .collect();
    let tool_block = build_tool_context_block(&filtered_map);

    let filtered: Vec<_> = skills
        .iter()
        .filter(|s| {
            skill_filter
                .as_ref()
                .is_none_or(|filter| filter.permits(&s.name))
        })
        .cloned()
        .collect();
    let skills_block = format_skills_for_prompt(&filtered, loaded_skills);

    // Compose environment sections. Builders returning an empty section are
    // omitted entirely.
    let env_sections = {
        vec![
            persona_section(persona),
            context_files_section(context_files),
        ]
        .into_iter()
        .filter(|section| !section.is_empty())
        .collect::<Vec<_>>()
    };

    // Split and normalize history before converting any partition. Tool loops
    // are selected as units so a pinned result cannot be separated from its call.
    let (top_pins, bottom_pins, working_history) = split_history(history);

    // Convert pin entries to messages.
    let top_messages = entries_to_messages(&top_pins);
    let bottom_messages = entries_to_messages(&bottom_pins);

    // Compose the system prompt in fixed section order. Empty sections are
    // omitted entirely; the rest are joined with a blank line between them.
    // Date and cwd are unconditional - their builders always render content.
    let system_prompt = {
        let mut system_parts: Vec<String> = Vec::new();
        system_parts.extend(env_sections);
        if let Some(block) = tool_block {
            system_parts.push(block);
        }
        if !skills_block.is_empty() {
            system_parts.push(skills_block);
        }
        system_parts.push(date_section());
        system_parts.push(cwd_section(cwd));
        SystemPrompt::new(system_parts.join("\n\n"))
    };

    // Convert working history to messages. Bottom pins are inserted before
    // the final message-run, at the message level: after conversion, the
    // tail run is contiguous User/Assistant/Tool messages with no open tool
    // batch (the tripwire guarantees that), so inserting there can never
    // split an assistant from its results.
    let working_messages = entries_to_messages(&working_history);

    // Build final message list: TOP pins first (in history order, nothing
    // reserved), then working history.
    let mut final_messages = Vec::new();
    final_messages.extend(top_messages);
    final_messages.extend(working_messages);

    // Insert BOTTOM pins just before the last message, walking back over a
    // trailing tool run so the pins land before the loop's assistant.
    insert_bottom_pins(&mut final_messages, bottom_messages);

    // Count tokens: the system prompt, every conversation message, and the
    // tool schemas actually shipped in the request. Providers bill the
    // tools array alongside the messages, so leaving it out under-reports
    // the request the same way the minimap's per-entry counts do not.
    let estimated_tokens = {
        let message_tokens = count_messages(&system_prompt, &final_messages, counter);
        let schema_tokens = count_tool_schema_tokens(&tool_defs, counter);
        message_tokens.saturating_add(u32::try_from(schema_tokens).unwrap_or(u32::MAX))
    };

    AssembledPrompt {
        session_id: session_id.clone(),
        system_prompt,
        messages: final_messages,
        tool_definitions: tool_defs,
        estimated_tokens,
    }
}

/// Splits history entries into TOP pins, BOTTOM pins, and working history.
///
/// Entry-level filtering (per `is_in_context()`); tool-loop atomicity is
/// enforced at write time by the history editor, which expands mutations to
/// whole loops, so no read-side group logic is needed here.
fn split_history(history: &[ChatEntry]) -> (Vec<ChatEntry>, Vec<ChatEntry>, Vec<ChatEntry>) {
    let top_pins: Vec<ChatEntry> = history
        .iter()
        .filter(|e| e.pin_position() == Some(PinPosition::Top))
        .cloned()
        .collect();

    let bottom_pins: Vec<ChatEntry> = history
        .iter()
        .filter(|e| e.pin_position() == Some(PinPosition::Bottom))
        .cloned()
        .collect();

    let working_history: Vec<ChatEntry> = history
        .iter()
        .filter(|e| {
            (e.pin_position().is_none() || e.pin_position() == Some(PinPosition::Relative))
                && e.is_in_context()
        })
        .cloned()
        .collect();

    (top_pins, bottom_pins, working_history)
}

/// Inserts bottom-pin messages before the final logical unit of the message
/// list.
///
/// Runs at the message level after conversion: the insertion index walks
/// back over a trailing tool run (the results of one assistant batch) so the
/// pins land before the loop's declaring assistant, never between it and its
/// results.
#[expect(
    clippy::indexing_slicing,
    reason = "index is bounded by the non-empty check above"
)]
fn insert_bottom_pins(final_messages: &mut Vec<LlmMessage>, bottom_messages: Vec<LlmMessage>) {
    if bottom_messages.is_empty() || final_messages.is_empty() {
        final_messages.extend(bottom_messages);
        return;
    }
    let mut insert_at = final_messages.len() - 1;
    while insert_at > 0 && matches!(final_messages[insert_at], LlmMessage::Tool { .. }) {
        insert_at -= 1;
    }
    final_messages.splice(insert_at..insert_at, bottom_messages);
}

/// Counts tokens across the system prompt and all messages.
///
/// Mirrors the per-entry estimator's conventions (`estimate_entry_content_tokens`)
/// so the assembled estimate always covers the sum of per-entry counts shown in
/// the minimap: tool-call `name + arguments`, tool-result `name + content`, and
/// the flat per-image attachment cost are all counted here.
fn count_messages(
    system_prompt: &SystemPrompt,
    messages: &[LlmMessage],
    counter: &dyn TokenCounter,
) -> u32 {
    let system_tokens = system_prompt.as_deref().map_or(0, |c| counter.count(c));
    messages
        .iter()
        .map(|msg| match msg {
            LlmMessage::User {
                content,
                attachments,
            } => counter.count(content) + image_attachment_tokens(attachments),
            LlmMessage::Assistant {
                content,
                tool_calls,
            } => {
                let calls: usize = tool_calls
                    .iter()
                    .flatten()
                    .map(|call| counter.count(&call.name) + counter.count(&call.arguments))
                    .sum();
                counter.count(content) + calls
            }
            LlmMessage::Tool { name, content, .. } => counter.count(name) + counter.count(content),
        })
        .sum::<usize>()
        .wrapping_add(system_tokens) as u32
}

/// Flat per-image cost shared with the per-entry estimator, so the assembled
/// estimate and the minimap's per-entry counts apply the same image price.
fn image_attachment_tokens(attachments: &[jinn_provider::Attachment]) -> usize {
    attachments
        .iter()
        .filter(|a| a.is_image())
        .count()
        .saturating_mul(IMAGE_ATTACHMENT_TOKENS)
}

/// Counts the tokens providers bill for a tool definition in the request's
/// tools array: the serialized `{name, description, parameters}` projection.
///
/// `prompt_snippet` and `prompt_guidelines` are deliberately excluded — they
/// ride only in the system prompt's tool block (see [`build_tool_context_block`]),
/// whose tokens are already counted via `system_prompt`. Serializing the whole
/// [`ToolDefinition`] would bill them twice.
fn count_tool_schema_tokens(tools: &[ToolDefinition], counter: &dyn TokenCounter) -> usize {
    tools
        .iter()
        .map(|def| {
            let schema = serde_json::json!({
                "name": def.name,
                "description": def.description,
                "parameters": def.parameters,
            })
            .to_string();
            counter.count(&schema)
        })
        .sum()
}

/// Runs [`assemble`] over inputs in wire form, for callers that cannot
/// name this pipeline's domain types across a compilation boundary —
/// the unit-test bridge in `jinn-kernel` enters here (see
/// `assembly_test_bridge` there); the crate compiles once, so its
/// deserialization of the payload lands on the same types [`assemble`]
/// reads.
///
/// # Errors
///
/// Returns an error when `inputs` does not deserialize into
/// [`AssemblyInputs`].
pub fn assemble_erased(inputs: serde_json::Value) -> Result<AssembledPrompt, serde_json::Error> {
    let inputs: AssemblyInputs = serde_json::from_value(inputs)?;
    let counter = jinn_llm_support::token_estimator::TiktokenCounter::o200k_base();
    Ok(assemble(&inputs, &counter))
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
    use jinn_context::env_context::ContextFile;
    use jinn_core_types::ServerToolType;
    use jinn_core_types::SessionId;
    use jinn_core_types::model_selection::ModelSelection;
    use jinn_core_types::tool_types::ToolDefinition;
    use jinn_core_types::{FilterMode, NameFilter};
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::state::State;
    use jinn_kernel::protocol::ChatEntry;
    use jinn_kernel::protocol::ToolResultStatus;
    use jinn_llm_support::token_estimator::TiktokenCounter;
    use jinn_skills_msg::Skill;
    use jinn_tools_msg::TASK_TOOL_NAME;

    /// Test bridge: build inputs from an AppState the way production
    /// callers do (via the kernel snapshot builder) and run the pure
    /// assemble. Keeps the historic stateful test style while the core
    /// under test stays pure.
    fn assemble_prompt(
        state: &AppState,
        session_id: &SessionId,
        counter: &TiktokenCounter,
    ) -> jinn_slices::AssembledPrompt {
        let inputs = crate::inputs::build_assembly_inputs(state, session_id);
        assemble(&inputs, counter)
    }

    pub(crate) fn counter() -> TiktokenCounter {
        TiktokenCounter::o200k_base()
    }

    fn make_skill(name: &str) -> Skill {
        Skill {
            name: name.to_owned(),
            description: format!("{name} skill"),
            body: String::new(),
            file_path: std::path::PathBuf::from(format!("/skills/{name}/SKILL.md")),
            base_dir: std::path::PathBuf::from(format!("/skills/{name}")),
            source: jinn_skills_msg::SkillSource::Global,
        }
    }

    pub(crate) fn make_tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object"}),
            prompt_snippet: Some(format!("{name} does things")),
            prompt_guidelines: vec![],
            server_tool_type: None,
        }
    }

    /// Counts tokens of `messages` alone (no system prompt, no tools) so
    /// message-level tests can assert exact deltas.
    fn count_messages_only(messages: &[LlmMessage]) -> u32 {
        let counter = counter();
        let system = SystemPrompt::new(String::new());
        count_messages(&system, messages, &counter)
    }

    /// Bridges [`TiktokenCounter`] to the per-entry estimator trait so tests
    /// can recompute minimap-style per-entry sums with the same tokenizer.
    struct CounterAsEstimator<'a>(&'a TiktokenCounter);

    impl jinn_llm_support::token_estimator::TokenEstimator for CounterAsEstimator<'_> {
        fn estimate(&self, text: &str) -> usize {
            self.0.count(text)
        }

        fn name(&self) -> &'static str {
            "counter_as_estimator"
        }
    }

    pub(crate) fn state_with_history(entries: Vec<ChatEntry>) -> (State, SessionId) {
        let state = State::new(AppState::default_with_scope_focus());
        let session_id = {
            let mut guard = state.write();
            let session = guard.active_session_mut();
            for entry in entries {
                session.push_entry(entry);
            }
            guard.session.active_session_id().clone()
        };
        (state, session_id)
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_keeps_pinned_tool_result_with_its_call() {
        // Given a tool loop bottom-pinned via the editor (which pins the whole
        // loop, the only legitimate producer of such state) and a later user turn.
        let (state, session_id) = {
            let state = State::new(AppState::default_with_scope_focus());
            let session_id = {
                let mut guard = state.write();
                let session = guard.active_session_mut();
                for entry in [
                    ChatEntry::user("run it"),
                    ChatEntry::assistant(""),
                    ChatEntry::tool_call("call-1", "bash", "{}"),
                    ChatEntry::tool_result(
                        "call-1",
                        "bash",
                        "ok",
                        jinn_kernel::protocol::ToolResultStatus::Success,
                    ),
                    ChatEntry::user("continue"),
                ] {
                    session.push_entry(entry);
                }
                let result_id = session.history()[3].id.clone();
                session.edit_history().pin(&result_id, PinPosition::Bottom);
                guard.session.active_session_id().clone()
            };
            (state, session_id)
        };

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the assistant call and tool result remain adjacent in valid order.
        let tool_index = result
            .messages
            .iter()
            .position(|message| matches!(message, LlmMessage::Tool { tool_call_id, .. } if tool_call_id == "call-1"))
            .expect("tool result should be present");
        assert!(matches!(
            result.messages.get(tool_index.wrapping_sub(1)),
            Some(LlmMessage::Assistant { tool_calls: Some(calls), .. }) if calls.iter().any(|call| call.id == "call-1")
        ));
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_drops_malformed_persisted_tool_history() {
        // Given persisted history containing an orphan result beside valid user history.
        let (state, session_id) = state_with_history(vec![
            ChatEntry::user("before"),
            ChatEntry::tool_result(
                "orphan",
                "bash",
                "bad",
                jinn_kernel::protocol::ToolResultStatus::Success,
            ),
            ChatEntry::user("after"),
        ]);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the malformed result is absent while neighboring user messages remain.
        let contents: Vec<&str> = result
            .messages
            .iter()
            .filter_map(|message| match message {
                LlmMessage::User { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert!(contents.contains(&"before"));
        assert!(contents.contains(&"after"));
        assert!(!result.messages.iter().any(|message| matches!(message, LlmMessage::Tool { tool_call_id, .. } if tool_call_id == "orphan")));
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_top_pin_keeps_tool_loop_atomic() {
        // Given a tool loop top-pinned via the editor (which pins the whole
        // loop) and a later user turn.
        let (state, session_id) = {
            let state = State::new(AppState::default_with_scope_focus());
            let session_id = {
                let mut guard = state.write();
                let session = guard.active_session_mut();
                for entry in [
                    ChatEntry::user("before"),
                    ChatEntry::assistant(""),
                    ChatEntry::tool_call("top-call", "echo", "{}"),
                    ChatEntry::tool_result("top-call", "echo", "ok", ToolResultStatus::Success),
                    ChatEntry::user("after"),
                ] {
                    session.push_entry(entry);
                }
                let call_id = session.history()[2].id.clone();
                session.edit_history().pin(&call_id, PinPosition::Top);
                guard.session.active_session_id().clone()
            };
            (state, session_id)
        };

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the complete tool loop is emitted in valid order.
        let assistant_index = result.messages.iter().position(|message| {
            matches!(message, LlmMessage::Assistant { tool_calls: Some(calls), .. } if calls.iter().any(|call| call.id == "top-call"))
        }).expect("tool assistant should be present");
        assert!(
            matches!(result.messages.get(assistant_index + 1), Some(LlmMessage::Tool { tool_call_id, .. }) if tool_call_id == "top-call")
        );
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_relative_pin_keeps_tool_loop_in_working_order() {
        // Given a relative-pinned tool result in a complete loop.
        let entries = vec![
            ChatEntry::user("before"),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("relative-call", "echo", "{}"),
            ChatEntry::tool_result("relative-call", "echo", "ok", ToolResultStatus::Success)
                .with_pin(PinPosition::Relative),
            ChatEntry::user("after"),
        ];
        let (state, session_id) = state_with_history(entries);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the user, tool loop, and later user remain in original order.
        let contents: Vec<&str> = result
            .messages
            .iter()
            .filter_map(|message| match message {
                LlmMessage::User { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(contents, vec!["before", "after"]);
        assert!(result.messages.windows(2).any(|window| matches!((&window[0], &window[1]), (LlmMessage::Assistant { tool_calls: Some(calls), .. }, LlmMessage::Tool { tool_call_id, .. }) if calls.iter().any(|call| call.id == "relative-call") && tool_call_id == "relative-call")));
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_parallel_batch_results_in_completion_order_emit_valid_sequence() {
        // Given a two-call batch whose results landed in completion order
        // (call-2 finished first). The v0.108.4 positional-zip converter
        // dropped the whole loop for this history.
        let entries = vec![
            ChatEntry::user("inspect both"),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("call-1", "read", r#"{"path":"a"}"#),
            ChatEntry::tool_call("call-2", "read", r#"{"path":"b"}"#),
            ChatEntry::tool_result("call-2", "read", "b", ToolResultStatus::Success),
            ChatEntry::tool_result("call-1", "read", "a", ToolResultStatus::Success),
        ];
        let (state, session_id) = state_with_history(entries);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then both calls are declared and both results are emitted (the
        // tripwire matches ids as a set, not positionally).
        let declared: Vec<&str> = result
            .messages
            .iter()
            .filter_map(|m| match m {
                LlmMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } => Some(calls.iter().map(|c| c.id.as_str()).collect::<Vec<_>>()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(declared, vec!["call-1", "call-2"]);
        let resolved: Vec<&str> = result
            .messages
            .iter()
            .filter_map(|m| match m {
                LlmMessage::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(resolved, vec!["call-2", "call-1"]);
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_legacy_split_loop_is_stripped_by_tripwire() {
        // Given a legacy persisted half-loop: an orphan tool result without
        // its call (the pre-editor corruption class). The write-time editor
        // cannot produce this; the tripwire must strip it.
        let entries = vec![
            ChatEntry::user("before"),
            ChatEntry::tool_result(
                "orphan",
                "bash",
                "stray",
                jinn_kernel::protocol::ToolResultStatus::Success,
            ),
            ChatEntry::user("after"),
        ];
        let (state, session_id) = state_with_history(entries);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the orphan tool message is dropped and the neighbors remain.
        let contents: Vec<&str> = result
            .messages
            .iter()
            .filter_map(|message| match message {
                LlmMessage::User { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(contents, vec!["before", "after"]);
        assert!(!result.messages.iter().any(
            |m| matches!(m, LlmMessage::Tool { tool_call_id, .. } if tool_call_id == "orphan")
        ));
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_bottom_pins_never_split_tool_loop() {
        // Given a bottom pin and a trailing complete tool loop.
        let entries = vec![
            ChatEntry::user("context"),
            ChatEntry::assistant("pin me").with_pin(PinPosition::Bottom),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("tail-call", "echo", "{}"),
            ChatEntry::tool_result("tail-call", "echo", "ok", ToolResultStatus::Success),
        ];
        let (state, session_id) = state_with_history(entries);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the bottom pin is placed before the trailing loop's assistant,
        // never between the assistant and its result.
        let tool_index = result
            .messages
            .iter()
            .position(|m| matches!(m, LlmMessage::Tool { tool_call_id, .. } if tool_call_id == "tail-call"))
            .expect("tool result present");
        assert!(matches!(
            result.messages.get(tool_index - 1),
            Some(LlmMessage::Assistant { tool_calls: Some(calls), .. })
                if calls.iter().any(|c| c.id == "tail-call")
        ));
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_with_empty_history_produces_only_system() {
        // Given a state with skills but no history.
        let (state, session_id) = state_with_history(vec![]);
        {
            let mut guard = state.write();
            guard
                .active_session_mut()
                .set_discovered_skills(vec![make_skill("test-skill")]);
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the system prompt contains the skill.
        let system = result.system_prompt.to_string();
        assert!(
            system.contains("test-skill"),
            "system prompt should contain skill, got: {system}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_with_no_skills_or_tools_still_has_env_context() {
        // Given a state with no skills, persona, tools, or context files, and no history.
        let (state, session_id) = state_with_history(vec![]);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the system prompt still has date and CWD from env context.
        let system = result.system_prompt.to_string();
        assert!(system.contains("Current date:"));
        assert!(system.contains("Current working directory:"));
        // And empty sections leave no gaps in the join.
        assert!(!system.contains("\n\n\n"), "empty section gap: {system:?}");
        assert!(!system.starts_with('\n'), "leading separator: {system:?}");
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_places_bottom_pins_before_last_message() {
        // Given a session with a user message and a bottom-pinned entry.
        let user = ChatEntry::user("hello");
        let mut pinned = ChatEntry::assistant("pinned assistant");
        pinned.pin_position = Some(PinPosition::Bottom);

        let (state, session_id) = state_with_history(vec![user, pinned]);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the bottom pin appears just before the last message.
        assert!(result.messages.len() >= 2, "need at least 2 messages");
        let second_last = &result.messages[result.messages.len() - 2];
        match second_last {
            LlmMessage::Assistant { content, .. } => {
                assert_eq!(content, "pinned assistant");
            }
            other => panic!("expected Assistant as second-to-last, got {other:?}"),
        }
        let last = result.messages.last().expect("has last");
        match last {
            LlmMessage::User { content, .. } => {
                assert_eq!(content, "hello");
            }
            other => panic!("expected User as last, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_drained_steering_sits_at_tail_preserving_tool_pairing() {
        // Given a tool_call/tool_result pair followed by a drained steering entry.
        let assistant = ChatEntry::assistant("using tool");
        let tool_result = ChatEntry::tool_result(
            "call-1",
            "bash",
            "ok",
            jinn_kernel::protocol::ToolResultStatus::Success,
        );
        let steer = ChatEntry::user_expanded("stay at the foo part", "stay at the foo part");
        let (state, session_id) = state_with_history(vec![
            ChatEntry::user("initial"),
            assistant,
            tool_result,
            steer,
        ]);

        // When assembling.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the steering entry sits at the tail of the messages.
        let last = result.messages.last().expect("has last message");
        match last {
            LlmMessage::User { content, .. } => {
                assert_eq!(content, "stay at the foo part");
            }
            other => panic!("expected User (steering) at tail, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_steering_and_bottom_pin_coexist_at_respective_positions() {
        // Given a user-pinned entry and a tail steering entry.
        use jinn_kernel::protocol::PinPosition;
        let pinned = ChatEntry::user("pinned constraint").with_pin(PinPosition::Bottom);
        let middle = ChatEntry::user("middle");
        let assistant = ChatEntry::assistant("response");
        let steer = ChatEntry::user_expanded("steer msg", "steer msg");
        let entries = vec![pinned, middle, assistant, steer];
        let (state, session_id) = state_with_history(entries);

        // When assembling.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then both the pinned message and the steering message appear in the assembled prompt.
        let body = result
            .messages
            .iter()
            .map(|m| match m {
                LlmMessage::User { content, .. } => content.as_str(),
                _ => "",
            })
            .collect::<Vec<_>>();
        assert!(
            body.iter().any(|s| s.contains("pinned constraint")),
            "pinned message must appear in prompt: {body:?}"
        );
        assert!(
            body.iter().any(|s| s.contains("steer msg")),
            "steering message must appear in prompt: {body:?}"
        );
        // Steering entry remains at the tail.
        assert_eq!(body.last().copied(), Some("steer msg"));
    }
    #[rstest::rstest]
    #[test]
    fn assemble_prompt_excludes_thinking_entries() {
        // Given a session with a thinking entry and a user message.
        let thinking = ChatEntry::thinking("internal thoughts");
        let user = ChatEntry::user("hello");
        let (state, session_id) = state_with_history(vec![thinking, user]);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then no message contains the thinking text.
        let system = result.system_prompt.to_string();
        assert!(
            !system.contains("internal thoughts"),
            "thinking entry should be excluded from system prompt"
        );
        for msg in &result.messages {
            match msg {
                LlmMessage::User { content, .. }
                | LlmMessage::Assistant { content, .. }
                | LlmMessage::Tool { content, .. } => {
                    assert!(
                        !content.contains("internal thoughts"),
                        "thinking entry should be excluded"
                    );
                }
            }
        }
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_token_count_is_accurate() {
        // Given a session with a user message.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("hello world")]);

        // When assembling the prompt.
        let counter = counter();
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter);

        // Then token count is > 0 and matches manual count.
        assert!(
            result.estimated_tokens() > 0,
            "token count should be positive"
        );

        // Manual count for this fixture: system prompt plus message content.
        // (No tools are registered, so the schema term is 0; the message has
        // no tool calls or images, so content is the whole message cost.)
        let system_tokens = result
            .system_prompt
            .as_deref()
            .map_or(0, |c| counter.count(c));
        let message_tokens: usize = result
            .messages
            .iter()
            .map(|m| match m {
                LlmMessage::User { content, .. }
                | LlmMessage::Assistant { content, .. }
                | LlmMessage::Tool { content, .. } => counter.count(content),
            })
            .sum::<usize>();
        let schema_tokens = count_tool_schema_tokens(&result.tool_definitions, &counter);
        let manual = system_tokens + message_tokens + schema_tokens;
        assert_eq!(result.estimated_tokens(), manual as u32);
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_includes_tools() {
        // Given a state with tool definitions.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("use tools")]);
        {
            let cell = {
                state
                    .read()
                    .tool_registry()
                    .expect("registry cell attached")
            };
            cell.update(|r| {
                r.global.insert("bash".to_owned(), make_tool("bash"));
            });
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then tool definitions are included.
        assert_eq!(result.tool_definitions.len(), 1);
        assert_eq!(result.tool_definitions[0].name, "bash");
    }

    fn make_web_search_tool() -> ToolDefinition {
        ToolDefinition {
            name: "openrouter:web_search".to_owned(),
            description: "Search the web".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
            prompt_snippet: Some("Web search (OpenRouter)".to_owned()),
            prompt_guidelines: vec![],
            server_tool_type: Some(ServerToolType::OpenrouterWebSearch),
        }
    }

    fn set_active_model(state: &State, model: &str) {
        let mut guard = state.write();
        guard
            .active_session_mut()
            .set_model(ModelSelection::Single(model.to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_includes_web_search_for_openrouter_model() {
        // Given a state on an openrouter model with web search registered.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("search it")]);
        set_active_model(&state, "openrouter/openai/gpt-oss-120b");
        {
            let cell = {
                state
                    .read()
                    .tool_registry()
                    .expect("registry cell attached")
            };
            cell.update(|r| {
                r.global
                    .insert("openrouter:web_search".to_owned(), make_web_search_tool());
            });
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then web search is in the tool definitions AND the system prompt block.
        assert_eq!(result.tool_definitions.len(), 1);
        assert_eq!(result.tool_definitions[0].name, "openrouter:web_search");
        let system = result.system_prompt.to_string();
        assert!(
            system.contains("Web search (OpenRouter)"),
            "system prompt should contain web search snippet, got: {system}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_excludes_web_search_for_non_openrouter_model() {
        // Given a state on a non-openrouter model with web search registered.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("search it")]);
        set_active_model(&state, "zai/glm-4.6");
        {
            let cell = {
                state
                    .read()
                    .tool_registry()
                    .expect("registry cell attached")
            };
            cell.update(|r| {
                r.global
                    .insert("openrouter:web_search".to_owned(), make_web_search_tool());
            });
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then web search is absent from tool definitions AND the system prompt block.
        assert_eq!(
            result.tool_definitions,
            [] as [jinn_core_types::ToolDefinition; 0]
        );
        let system = result.system_prompt.to_string();
        assert!(
            !system.contains("Web search (OpenRouter)"),
            "system prompt should not contain web search snippet, got: {system}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn an_allow_filter_cannot_re_admit_a_tool_the_provider_gate_withholds() {
        // Given a non-openrouter model with web search registered, and an
        // allow filter naming it.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("search it")]);
        set_active_model(&state, "zai/glm-4.6");
        {
            let cell = state
                .read()
                .tool_registry()
                .expect("registry cell attached");
            cell.update(|r| {
                r.global
                    .insert("openrouter:web_search".to_owned(), make_web_search_tool());
            });
            let mut guard = state.write();
            guard
                .session
                .get_mut(&session_id)
                .expect("session exists")
                .set_tool_filter(Some(NameFilter {
                    mode: FilterMode::Allow,
                    names: ["openrouter:web_search".to_owned()].into_iter().collect(),
                }));
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the tool is still absent. The filter and the provider gate are
        // independent: an allow filter widens what the session *may* use, not
        // what the provider *can* run. Folding them into one predicate would
        // let an allow list hand back a tool the model cannot call.
        assert!(
            result.tool_definitions.is_empty(),
            "the provider gate must still withhold the tool; got: {:?}",
            result
                .tool_definitions
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_keeps_function_tools_for_non_openrouter_model() {
        // Given a state on a non-openrouter model with a function tool registered.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("do work")]);
        set_active_model(&state, "zai/glm-4.6");
        {
            let cell = {
                state
                    .read()
                    .tool_registry()
                    .expect("registry cell attached")
            };
            cell.update(|r| {
                r.global.insert("bash".to_owned(), make_tool("bash"));
            });
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the function tool is still present despite the non-openrouter model.
        assert_eq!(result.tool_definitions.len(), 1);
        assert_eq!(result.tool_definitions[0].name, "bash");
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_includes_context_files_in_system_message() {
        // Given a state with cached context files.
        let (state, session_id) = state_with_history(vec![]);
        {
            let mut guard = state.write();
            guard
                .active_session_mut()
                .set_discovered_context_files(vec![ContextFile {
                    path: std::path::PathBuf::from("/project/AGENTS.md"),
                    content: "Use Rust.".to_owned(),
                }]);
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the system prompt contains the context file content.
        let system = result.system_prompt.to_string();
        assert!(system.contains("Use Rust."));
        assert!(system.contains("Project Context"));
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_includes_skills_and_global_tools() {
        // Given a state with skills and tools.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("hello")]);
        {
            let cell = {
                state
                    .read()
                    .tool_registry()
                    .expect("registry cell attached")
            };
            cell.update(|r| {
                r.global.insert("bash".to_owned(), make_tool("bash"));
            });
            let mut guard = state.write();
            guard
                .active_session_mut()
                .set_discovered_skills(vec![make_skill("test-skill")]);
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the system prompt contains the skill.
        let system = result.system_prompt.to_string();
        assert!(system.contains("test-skill"));
        // And the global tool is the only tool definition.
        assert_eq!(result.tool_definitions.len(), 1);
        assert_eq!(result.tool_definitions[0].name, "bash");
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_excludes_filtered_tools_from_tool_definitions() {
        // Given a session with tools, some withheld by a deny filter.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("use tools")]);
        {
            let cell = {
                state
                    .read()
                    .tool_registry()
                    .expect("registry cell attached")
            };
            cell.update(|r| {
                r.global.insert("bash".to_owned(), make_tool("bash"));
                r.global.insert("read".to_owned(), make_tool("read"));
                r.global.insert("write".to_owned(), make_tool("write"));
            });
            // Withhold bash and write.
            {
                let mut guard = state.write();
                guard
                    .session
                    .get_mut(&session_id)
                    .expect("session exists")
                    .set_tool_filter(Some(NameFilter::deny([
                        "bash".to_owned(),
                        "write".to_owned(),
                    ])));
            }
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then only enabled tools appear in tool definitions.
        let tool_names: Vec<&str> = result
            .tool_definitions
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert!(
            !tool_names.contains(&"bash"),
            "disabled bash should be excluded, got: {tool_names:?}"
        );
        assert!(
            !tool_names.contains(&"write"),
            "filtered write should be excluded, got: {tool_names:?}"
        );
        assert!(
            tool_names.contains(&"read"),
            "unfiltered read should be included, got: {tool_names:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn parent_linked_session_keeps_task_tool_when_not_disabled() {
        // Given a state whose context advertises the task tool, and a child
        // session linked to a parent with nothing disabled.
        let state = State::new(AppState::default_with_scope_focus());
        let parent_id = SessionId::new();
        let child_id;
        {
            let cell = {
                state
                    .read()
                    .tool_registry()
                    .expect("registry cell attached")
            };
            cell.update(|r| {
                r.global
                    .insert(TASK_TOOL_NAME.to_owned(), make_tool(TASK_TOOL_NAME));
            });
            let child = jinn_session_state::ChatSessionState::new_child(&parent_id, true);
            child_id = child.session_id().clone();
            let mut guard = state.write();
            guard.session.insert(child);
        }

        // When assembling the prompt for the child.
        let snapshot = state.read();
        let result = assemble_prompt(&snapshot, &child_id, &counter());

        // Then the tool definitions include task.
        assert!(
            result
                .tool_definitions
                .iter()
                .any(|def| def.name == TASK_TOOL_NAME),
            "a parent-linked session without the tool disabled keeps the task tool, got: {:?}",
            result
                .tool_definitions
                .iter()
                .map(|d| d.name.clone())
                .collect::<Vec<_>>()
        );
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_excludes_filtered_tools_from_tool_context_block() {
        // Given a session with tools and some disabled.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("use tools")]);
        {
            let cell = {
                state
                    .read()
                    .tool_registry()
                    .expect("registry cell attached")
            };
            cell.update(|r| {
                r.global.insert("bash".to_owned(), make_tool("bash"));
                r.global.insert("read".to_owned(), make_tool("read"));
            });
            // Withhold bash.
            let mut guard = state.write();
            guard
                .session
                .get_mut(&session_id)
                .expect("session exists")
                .set_tool_filter(Some(NameFilter::deny(["bash".to_owned()])));
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the system prompt tool block excludes the filtered tool.
        let system = result.system_prompt.to_string();
        assert!(
            system.contains("read does things"),
            "enabled tool should be in tool context block, got: {system}"
        );
        assert!(
            !system.contains("bash does things"),
            "disabled tool should be excluded from tool context block, got: {system}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_excludes_filtered_skills_from_skills_block() {
        // Given a session with skills and some disabled.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("use skills")]);
        {
            let mut guard = state.write();
            guard.active_session_mut().set_discovered_skills(vec![
                make_skill("phased-task-loop"),
                make_skill("web-coder"),
                make_skill("scream"),
            ]);
            // Withhold web-coder.
            guard
                .session
                .get_mut(&session_id)
                .expect("session exists")
                .set_skill_filter(Some(NameFilter::deny(["web-coder".to_owned()])));
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the system prompt skills block excludes the filtered skill.
        let system = result.system_prompt.to_string();
        assert!(
            system.contains("<name>phased-task-loop</name>"),
            "enabled skill should be in skills block, got: {system}"
        );
        assert!(
            system.contains("<name>scream</name>"),
            "enabled skill should be in skills block, got: {system}"
        );
        assert!(
            !system.contains("<name>web-coder</name>"),
            "filtered skill should be excluded from skills block, got: {system}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn an_allow_filter_excludes_every_unlisted_tool_from_the_prompt() {
        // Given a session permitted exactly one tool.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("use tools")]);
        {
            let cell = state
                .read()
                .tool_registry()
                .expect("registry cell attached");
            cell.update(|r| {
                r.global.insert("read".to_owned(), make_tool("read"));
                r.global.insert("write".to_owned(), make_tool("write"));
                r.global.insert(
                    "mcp__github__create_pr".to_owned(),
                    make_tool("mcp__github__create_pr"),
                );
            });
            let mut guard = state.write();
            guard
                .session
                .get_mut(&session_id)
                .expect("session exists")
                .set_tool_filter(Some(NameFilter {
                    mode: FilterMode::Allow,
                    names: ["read".to_owned()].into_iter().collect(),
                }));
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then only the listed tool is offered. The MCP tool is the case a
        // blocklist could not reach: there is no entry to remove, only a list
        // it fails to appear on.
        let tool_names: Vec<&str> = result
            .tool_definitions
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert!(tool_names.contains(&"read"), "got: {tool_names:?}");
        assert!(!tool_names.contains(&"write"), "got: {tool_names:?}");
        assert!(
            !tool_names.contains(&"mcp__github__create_pr"),
            "an unlisted MCP tool must be withheld; got: {tool_names:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_session_with_no_filter_offers_every_tool() {
        // Given a session whose filter names nothing.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("use tools")]);
        {
            let cell = state
                .read()
                .tool_registry()
                .expect("registry cell attached");
            cell.update(|r| {
                r.global.insert("read".to_owned(), make_tool("read"));
                r.global.insert("write".to_owned(), make_tool("write"));
            });
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then every tool is offered, exactly as before this type existed.
        let tool_names: Vec<&str> = result
            .tool_definitions
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(
            tool_names.len(),
            2,
            "an unconfigured filter must withhold nothing; got: {tool_names:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_uses_matching_persona() {
        // Given a session with persona "custom" and a persona list containing "custom".
        let (state, session_id) = state_with_history(vec![ChatEntry::user("hello")]);
        {
            let cell = {
                state
                    .read()
                    .persona_selection()
                    .expect("persona cell attached")
            };
            let () = cell.update(|p| {
                p.entries.push(jinn_slices::Persona {
                    name: "custom".to_owned(),
                    description: "Custom persona".to_owned(),
                    body: "You are a custom persona.".to_owned(),
                });
            });
            let mut guard = state.write();
            guard
                .session
                .get_mut(&session_id)
                .expect("session exists")
                .set_persona_name("custom".to_owned());
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the system prompt contains the custom persona body.
        let system = result.system_prompt.to_string();
        assert!(
            system.contains("You are a custom persona."),
            "should contain custom persona body, got: {system}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_falls_back_to_coding_assistant_persona() {
        // Given a session with persona name "nonexistent" but "coding-assistant" is available.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("hello")]);
        {
            let cell = {
                state
                    .read()
                    .persona_selection()
                    .expect("persona cell attached")
            };
            let () = cell.update(|p| {
                p.entries.push(jinn_slices::Persona {
                    name: "coding-assistant".to_owned(),
                    description: "Default".to_owned(),
                    body: "You are a coding assistant.".to_owned(),
                });
            });
            let mut guard = state.write();
            guard
                .session
                .get_mut(&session_id)
                .expect("session exists")
                .set_persona_name("nonexistent".to_owned());
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the system prompt contains the coding-assistant fallback.
        let system = result.system_prompt.to_string();
        assert!(
            system.contains("You are a coding assistant."),
            "should contain coding-assistant fallback, got: {system}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_emits_pinned_system_entry_as_user_message() {
        // Given a session with a top-pinned System entry.
        let mut sys_entry = ChatEntry::system("Custom system instructions");
        sys_entry.pin_position = Some(PinPosition::Top);
        let user = ChatEntry::user("hello");
        let (state, session_id) = state_with_history(vec![sys_entry, user]);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the pinned system entry rides as a [System]-prefixed User message.
        let first = &result.messages[0];
        match first {
            LlmMessage::User { content, .. } => {
                assert_eq!(content, "[System] Custom system instructions");
            }
            other => panic!("expected User message, got {other:?}"),
        }
        // And the system prompt does NOT absorb it.
        let system = result.system_prompt.to_string();
        assert!(!system.contains("Custom system instructions"));
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_pins_occupy_array_front_in_history_order() {
        // Given a session with a top-pinned System entry and a top-pinned User entry.
        let mut sys_entry = ChatEntry::system("System stuff");
        sys_entry.pin_position = Some(PinPosition::Top);
        let mut user_pin = ChatEntry::user("pinned user");
        user_pin.pin_position = Some(PinPosition::Top);
        let (state, session_id) = state_with_history(vec![sys_entry, user_pin]);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the pins occupy the array front in history order - nothing reserved.
        match (&result.messages[0], &result.messages[1]) {
            (
                LlmMessage::User { content: first, .. },
                LlmMessage::User {
                    content: second, ..
                },
            ) => {
                assert_eq!(first, "[System] System stuff");
                assert_eq!(second, "pinned user");
            }
            other => panic!("expected two pin messages at the front, got {other:?}"),
        }
    }

    /// A state populated so every system-prompt section carries its marker.
    fn state_with_every_system_section() -> (State, SessionId) {
        let (state, session_id) = state_with_history(vec![]);
        let cell = state
            .read()
            .persona_selection()
            .expect("persona cell attached");
        let () = cell.update(|p| {
            p.entries.push(jinn_slices::Persona {
                name: "custom".to_owned(),
                description: "Custom persona".to_owned(),
                body: "ORDER-MARK-PERSONA".to_owned(),
            });
        });
        let mut guard = state.write();
        guard
            .session
            .get_mut(&session_id)
            .expect("session exists")
            .set_persona_name("custom".to_owned());
        guard
            .active_session_mut()
            .set_discovered_context_files(vec![ContextFile {
                path: std::path::PathBuf::from("/project/AGENTS.md"),
                content: "ORDER-MARK-FILES".to_owned(),
            }]);
        guard
            .active_session_mut()
            .set_discovered_skills(vec![make_skill("ordermark-skill")]);
        drop(guard);
        let cell = state
            .read()
            .tool_registry()
            .expect("registry cell attached");
        cell.update(|r| {
            r.global
                .insert("ordermark".to_owned(), make_tool("ordermark"));
        });
        (state, session_id)
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_system_sections_appear_in_declared_order() {
        // Given a state where every system section has content.
        let (state, session_id) = state_with_every_system_section();

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then each section's marker appears after the previous section's.
        let system = result.system_prompt.to_string();
        let positions = [
            system.find("ORDER-MARK-PERSONA"),
            system.find("ORDER-MARK-FILES"),
            system.find("ordermark does things"),
            system.find("<name>ordermark-skill</name>"),
            system.find("Current date:"),
            system.find("Current working directory:"),
        ];
        assert!(
            positions.iter().all(Option::is_some),
            "all six sections must be present, positions: {positions:?}"
        );
        let mut prev = 0;
        for (index, pos) in positions.iter().enumerate() {
            let pos = pos.expect("checked above");
            assert!(
                pos >= prev,
                "section {index} out of declared order: {positions:?}"
            );
            prev = pos;
        }
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_bottom_pins_placed_before_last_message() {
        // Given a session with multiple bottom-pinned entries and a user message.
        let mut bottom1 = ChatEntry::assistant("bottom-1");
        bottom1.pin_position = Some(PinPosition::Bottom);
        let mut bottom2 = ChatEntry::assistant("bottom-2");
        bottom2.pin_position = Some(PinPosition::Bottom);
        let user = ChatEntry::user("the last msg");
        let (state, session_id) = state_with_history(vec![bottom1, bottom2, user]);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the last message is the user message (bottom pins are inserted before it).
        let last = result.messages.last().expect("has messages");
        if let LlmMessage::User { content, .. } = last {
            assert_eq!(content, "the last msg");
        } else {
            panic!("expected User as last message, got {last:?}");
        }

        // And both bottom pins appear before the last message.
        let n = result.messages.len();
        let assistant_contents: Vec<&str> = result.messages[..n - 1]
            .iter()
            .filter_map(|m| match m {
                LlmMessage::Assistant { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            assistant_contents.contains(&"bottom-1"),
            "bottom-1 should appear before last msg"
        );
        assert!(
            assistant_contents.contains(&"bottom-2"),
            "bottom-2 should appear before last msg"
        );
    }

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_top_pin_not_in_working_history() {
        // Given a session with a top-pinned entry that is NOT a system entry.
        let mut top_user = ChatEntry::user("top pinned user");
        top_user.pin_position = Some(PinPosition::Top);
        let working_user = ChatEntry::user("working user");
        let (state, session_id) = state_with_history(vec![top_user, working_user]);

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the top-pinned user message appears exactly once (not duplicated in working history).
        let user_msg_count = result
            .messages
            .iter()
            .filter(|m| {
                if let LlmMessage::User { content, .. } = m {
                    content == "top pinned user"
                } else {
                    false
                }
            })
            .count();
        assert_eq!(
            user_msg_count, 1,
            "top pinned user should appear exactly once, appeared {user_msg_count}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn count_messages_counts_assistant_tool_call_arguments() {
        // Given two equal-content assistant messages, one carrying a tool call.
        let plain = vec![LlmMessage::Assistant {
            content: "thinking".to_owned(),
            tool_calls: None,
        }];
        let with_call = vec![LlmMessage::Assistant {
            content: "thinking".to_owned(),
            tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                id: "call-1".to_owned(),
                name: "bash".to_owned(),
                arguments: r#"{"command":"ls -la /tmp"}"#.to_owned(),
            }]),
        }];

        // When counting tokens for each message list.
        let baseline = count_messages_only(&plain);
        let counted = count_messages_only(&with_call);

        // Then the tool call adds the token count of name + arguments, the
        // same convention the per-entry estimator uses.
        let counter = counter();
        let expected_delta = {
            let name = counter.count("bash");
            let args = counter.count(r#"{"command":"ls -la /tmp"}"#);
            name + args
        };
        assert_eq!(
            usize::try_from(counted - baseline).unwrap_or(0),
            expected_delta,
            "tool call arguments must be counted (name + arguments), \
             got delta {baseline} -> {counted}, expected +{expected_delta}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn count_messages_counts_tool_result_name() {
        // Given two equal-content tool messages, one with a non-empty tool name.
        let plain = vec![LlmMessage::Tool {
            tool_call_id: "call-1".to_owned(),
            name: String::new(),
            content: "some tool output".to_owned(),
        }];
        let named = vec![LlmMessage::Tool {
            tool_call_id: "call-1".to_owned(),
            name: "bash".to_owned(),
            content: "some tool output".to_owned(),
        }];

        // When counting tokens for each message list.
        let baseline = count_messages_only(&plain);
        let counted = count_messages_only(&named);

        // Then the tool name adds its token count, matching the per-entry
        // estimator's name + content convention for ToolResult entries.
        let expected_delta = counter().count("bash");
        assert_eq!(
            usize::try_from(counted - baseline).unwrap_or(0),
            expected_delta,
            "tool result name must be counted alongside content"
        );
    }

    #[rstest::rstest]
    #[test]
    fn count_messages_adds_image_attachment_cost() {
        // Given two equal-content user messages, one carrying an image attachment.
        let plain = vec![LlmMessage::User {
            content: "describe this".to_owned(),
            attachments: vec![],
        }];
        let with_image = vec![LlmMessage::User {
            content: "describe this".to_owned(),
            attachments: vec![jinn_provider::Attachment::image(
                "image/png".to_owned(),
                vec![1, 2, 3],
            )],
        }];

        // When counting tokens for each message list.
        let baseline = count_messages_only(&plain);
        let counted = count_messages_only(&with_image);

        // Then the image adds the flat per-image cost used by the per-entry
        // estimator (765), so both sides of the minimap invariant agree.
        assert_eq!(
            counted - baseline,
            u32::try_from(jinn_llm_support::token_estimator::IMAGE_ATTACHMENT_TOKENS).unwrap_or(0),
            "image attachment must add the flat per-image cost"
        );
    }

    #[rstest::rstest]
    #[test]
    fn count_messages_adds_flat_cost_per_image_not_per_message() {
        // Given a user message carrying two image attachments.
        let with_images = vec![LlmMessage::User {
            content: "describe these".to_owned(),
            attachments: vec![
                jinn_provider::Attachment::image("image/png".to_owned(), vec![1]),
                jinn_provider::Attachment::image("image/jpeg".to_owned(), vec![2, 3]),
            ],
        }];

        // When counting tokens for the message.
        let counted = count_messages_only(&with_images);
        let text_only = counter().count("describe these");

        // Then the flat cost is added once per image.
        let image_flat =
            u32::try_from(jinn_llm_support::token_estimator::IMAGE_ATTACHMENT_TOKENS).unwrap_or(0);
        assert_eq!(
            counted - text_only as u32,
            image_flat * 2,
            "two image attachments must add the flat cost twice"
        );
    }

    /// The provider-facing projection of a tool definition: exactly the
    /// name, description, and parameters an API receives in its tools array.
    fn schema_projection_of(def: &ToolDefinition) -> String {
        serde_json::json!({
            "name": def.name,
            "description": def.description,
            "parameters": def.parameters,
        })
        .to_string()
    }

    /// The summed token count of every message's content in an assembly.
    fn message_content_tokens(result: &AssembledPrompt, counter: &TiktokenCounter) -> usize {
        result
            .messages
            .iter()
            .map(|m| match m {
                LlmMessage::User { content, .. }
                | LlmMessage::Assistant { content, .. }
                | LlmMessage::Tool { content, .. } => counter.count(content),
            })
            .sum()
    }

    #[rstest::rstest]
    #[test]
    fn assemble_estimate_includes_tool_schema_tokens() {
        // Given a state with one tool whose schema (name/description/parameters)
        // carries substantial text, plus large snippet/guidelines that ride in
        // the system prompt.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("use tools")]);
        let cell = state
            .read()
            .tool_registry()
            .expect("registry cell attached");
        let heavy_schema = || ToolDefinition {
            name: "schema-heavy".to_owned(),
            description: "A".repeat(200),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "BBBB".repeat(20)}
                }
            }),
            prompt_snippet: Some("SNIPPET-ONLY-IN-SYSTEM-PROMPT".to_owned()),
            prompt_guidelines: vec!["GUIDELINE-ONLY-IN-SYSTEM-PROMPT".to_owned()],
            server_tool_type: None,
        };
        cell.update(|r| {
            r.global.insert("schema-heavy".to_owned(), heavy_schema());
        });

        // When assembling the prompt.
        let counter = counter();
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter);

        // Then the estimate at least covers the schema projection: the count
        // of serializing name, description, and parameters (what providers
        // actually receive in the tools array).
        let schema_tokens = counter.count(&schema_projection_of(&result.tool_definitions[0]));
        let system_tokens = counter.count(&result.system_prompt.to_string());
        let message_tokens = message_content_tokens(&result, &counter);
        let floor = system_tokens + message_tokens + schema_tokens;
        assert!(
            result.estimated_tokens() >= floor as u32,
            "estimate {} must cover system ({system_tokens}) + messages \
             ({message_tokens}) + schema ({schema_tokens})",
            result.estimated_tokens()
        );

        // And the estimate stays close to that floor: the snippet and
        // guidelines are NOT double-counted on top of their system-prompt
        // occurrence (only the ~few-token whitespace/JSON overhead above it).
        let headroom = result.estimated_tokens() - floor as u32;
        assert!(
            headroom < 40,
            "estimate exceeds schema floor by {headroom}; \
             snippet/guidelines must not be double-counted"
        );
    }

    /// The minimap's basis: the sum of per-entry content token estimates
    /// over in-context entries only (persisted `token_count` is that same
    /// per-entry estimate, gated by `is_in_context`).
    fn in_context_minimap_sum(
        state: &State,
        session_id: &SessionId,
        counter: &TiktokenCounter,
    ) -> usize {
        let estimator = CounterAsEstimator(counter);
        let guard = state.read();
        guard
            .session
            .get(session_id)
            .expect("session exists")
            .history()
            .iter()
            .filter(|entry| entry.is_in_context())
            .map(|entry| {
                jinn_llm_support::token_estimator::estimate_entry_content_tokens(&estimator, entry)
            })
            .sum()
    }

    #[rstest::rstest]
    #[test]
    fn assembled_estimate_covers_in_context_minimap_sum() {
        // Given a history exercising every in-context shape: user text, an
        // image attachment, an assistant tool loop, a pinned entry, plus
        // out-of-context entries the minimap must skip.
        let mut image_user = ChatEntry::user("describe this screenshot");
        if let jinn_kernel::protocol::ChatEntryKind::User { attachments, .. } = &mut image_user.kind
        {
            attachments.push(jinn_provider::Attachment::image(
                "image/png".to_owned(),
                vec![1, 2, 3],
            ));
        }
        let entries = vec![
            ChatEntry::user("run the build"),
            image_user,
            ChatEntry::assistant("running it"),
            ChatEntry::tool_call("call-1", "bash", r#"{"command":"cargo build"}"#),
            ChatEntry::tool_result(
                "call-1",
                "bash",
                "compiled fine",
                jinn_kernel::protocol::ToolResultStatus::Success,
            ),
            ChatEntry::user("always remember this").with_pin(PinPosition::Top),
            // Out of context: excluded from both the prompt and the minimap.
            ChatEntry::thinking("private reasoning"),
            ChatEntry::transient("Welcome to jinn!"),
        ];
        let (state, session_id) = state_with_history(entries);
        let cell = state
            .read()
            .tool_registry()
            .expect("registry cell attached");
        cell.update(|r| {
            r.global.insert("bash".to_owned(), make_tool("bash"));
        });

        // When assembling the prompt and summing per-entry token counts over
        // in-context entries (the minimap's basis: persisted token_count is
        // the per-entry content estimate, summed with is_in_context gating).
        let counter = counter();
        let minimap_sum = in_context_minimap_sum(&state, &session_id, &counter);
        let result = {
            let guard = state.read();
            assemble_prompt(&guard, &session_id, &counter)
        };

        // Then the assembled estimate covers the minimap's in-context total
        // (plus system prompt and tool schemas), so the status bar can never
        // read below the minimap arrows.
        assert!(
            result.estimated_tokens() as usize >= minimap_sum,
            "assembled estimate {} must cover minimap in-context sum {minimap_sum}",
            result.estimated_tokens()
        );
    }
}
