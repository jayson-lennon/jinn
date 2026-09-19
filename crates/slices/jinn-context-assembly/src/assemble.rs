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

use jinn_core_types::ToolDefinition;
use jinn_domain::feat::context::env_context::{
    context_files_section, cwd_section, date_section, persona_section,
};
use jinn_domain::feat::context::protocol::inputs::AssemblyInputs;
use jinn_domain::feat::context::strategy::token_estimator::TokenCounter;
use jinn_domain::feat::context::tool_prompt::build_tool_context_block;
use jinn_skills::format_skills_for_prompt;
use jinn_domain::protocol::{ChatEntry, LlmMessage, PinPosition, entries_to_messages};
use jinn_slices::AssembledPrompt;
use jinn_slices::SystemPrompt;

///
/// Reads all context (skills, persona, context files, tools, history) from
/// [`AppState`] in one read-lock scope, produces messages and counts tokens.
///
/// # Assembly pipeline
///
/// 1. Read skills, persona, context files, tools, history from state.
/// 2. Split history into TOP/BOTTOM pins and working history.
/// 3. Compose the system prompt from per-section builders.
/// 4. Convert history (pins and working) to messages via [`entries_to_messages`].
/// 5. Re-inject pins in correct positions.
/// 6. Count tokens in the system prompt and all assembled messages.
/// 7. Return [`AssembledPrompt`].
///
/// # Panics
///
/// Panics if the given `session_id` does not exist in the session map.
#[must_use]
/// Assembles a complete LLM prompt from caller-provided inputs, in a
/// single pure pass.
///
/// Reads nothing from any shared state: every field comes from
/// [`AssemblyInputs`]. Splits pinned entries, builds the system prompt,
/// converts history to messages, and counts tokens.
pub fn assemble(inputs: &AssemblyInputs, counter: &dyn TokenCounter) -> AssembledPrompt {
    let AssemblyInputs {
        session_id,
        cwd,
        persona,
        history,
        tools,
        disabled_tools,
        provider_name,
        skills,
        disabled_skills,
        loaded_skills,
        context_files,
    } = inputs;

    let persona = persona.as_ref();

    let mut tool_defs: Vec<ToolDefinition> = tools.clone();

    // Filter out disabled tools and server tools that don't match the active provider.
    tool_defs.retain(|def| {
        !disabled_tools.contains(&def.name) && def.available_for_provider(provider_name)
    });

    let filtered_map: BTreeMap<String, ToolDefinition> = tool_defs
        .iter()
        .cloned()
        .map(|def| (def.name.clone(), def))
        .collect();
    let tool_block = build_tool_context_block(&filtered_map);

    let filtered: Vec<_> = skills
        .iter()
        .filter(|s| !disabled_skills.contains(&s.name))
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

    // Count tokens: the system prompt plus every conversation message.
    let estimated_tokens = count_messages(&system_prompt, &final_messages, counter);

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
fn count_messages(
    system_prompt: &SystemPrompt,
    messages: &[LlmMessage],
    counter: &dyn TokenCounter,
) -> u32 {
    let system_tokens = system_prompt.as_deref().map_or(0, |c| counter.count(c));
    messages
        .iter()
        .map(|msg| match msg {
            LlmMessage::User { content, .. }
            | LlmMessage::Assistant { content, .. }
            | LlmMessage::Tool { content, .. } => counter.count(content),
        })
        .sum::<usize>()
        .wrapping_add(system_tokens) as u32
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
    use jinn_core_types::ServerToolType;
    use jinn_core_types::model_selection::ModelSelection;
    use jinn_core_types::tool_types::ToolDefinition;
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::common::state::State;
    use jinn_domain::feat::context::env_context::ContextFile;
    use jinn_domain::feat::context::strategy::token_estimator::TiktokenCounter;
    use jinn_domain::protocol::ToolResultStatus;
    use jinn_skills::Skill;
    use jinn_domain::protocol::{ChatEntry, SessionId};
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
        let inputs = jinn_domain::feat::context::snapshot::build_assembly_inputs(state, session_id);
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
            source: jinn_skills::SkillSource::Global,
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

    pub(crate) fn state_with_history(entries: Vec<ChatEntry>) -> (State, SessionId) {
        let state = State::new(AppState::default_with_scope_focus());
        let session_id = {
            let mut guard = state.write_test_no_cap();
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
                let mut guard = state.write_test_no_cap();
                let session = guard.active_session_mut();
                for entry in [
                    ChatEntry::user("run it"),
                    ChatEntry::assistant(""),
                    ChatEntry::tool_call("call-1", "bash", "{}"),
                    ChatEntry::tool_result(
                        "call-1",
                        "bash",
                        "ok",
                        jinn_domain::protocol::ToolResultStatus::Success,
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
                jinn_domain::protocol::ToolResultStatus::Success,
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
                let mut guard = state.write_test_no_cap();
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
                jinn_domain::protocol::ToolResultStatus::Success,
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
            let mut guard = state.write_test_no_cap();
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
            jinn_domain::protocol::ToolResultStatus::Success,
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
        use jinn_domain::protocol::PinPosition;
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

        // Manual count: system prompt plus every message.
        let system_tokens = result
            .system_prompt
            .as_deref()
            .map_or(0, |c| counter.count(c));
        let manual: usize = result
            .messages
            .iter()
            .map(|m| match m {
                LlmMessage::User { content, .. }
                | LlmMessage::Assistant { content, .. }
                | LlmMessage::Tool { content, .. } => counter.count(content),
            })
            .sum::<usize>()
            + system_tokens;
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
        let mut guard = state.write_test_no_cap();
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
        assert!(result.tool_definitions.is_empty());
        let system = result.system_prompt.to_string();
        assert!(
            !system.contains("Web search (OpenRouter)"),
            "system prompt should not contain web search snippet, got: {system}"
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
            let mut guard = state.write_test_no_cap();
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
            let mut guard = state.write_test_no_cap();
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
    fn assemble_prompt_excludes_disabled_tools_from_tool_definitions() {
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
                r.global.insert("write".to_owned(), make_tool("write"));
            });
            // Disable bash and write.
            let mut disabled = std::collections::HashSet::new();
            disabled.insert("bash".to_owned());
            disabled.insert("write".to_owned());
            {
                let mut guard = state.write_test_no_cap();
                guard
                    .session
                    .get_mut(&session_id)
                    .expect("session exists")
                    .set_disabled_tools(disabled);
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
            "disabled write should be excluded, got: {tool_names:?}"
        );
        assert!(
            tool_names.contains(&"read"),
            "enabled read should be included, got: {tool_names:?}"
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
            let child = jinn_domain::feat::session::chat_session::ChatSessionState::new_child(
                &parent_id, true,
            );
            child_id = child.session_id().clone();
            let mut guard = state.write_test_no_cap();
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
    fn assemble_prompt_excludes_disabled_tools_from_tool_context_block() {
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
            // Disable bash.
            let mut disabled = std::collections::HashSet::new();
            disabled.insert("bash".to_owned());
            let mut guard = state.write_test_no_cap();
            guard
                .session
                .get_mut(&session_id)
                .expect("session exists")
                .set_disabled_tools(disabled);
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the system prompt tool block excludes disabled tools.
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
    fn assemble_prompt_excludes_disabled_skills_from_skills_block() {
        // Given a session with skills and some disabled.
        let (state, session_id) = state_with_history(vec![ChatEntry::user("use skills")]);
        {
            let mut guard = state.write_test_no_cap();
            guard.active_session_mut().set_discovered_skills(vec![
                make_skill("phased-task-loop"),
                make_skill("web-coder"),
                make_skill("scream"),
            ]);
            // Disable web-coder.
            guard
                .session
                .get_mut(&session_id)
                .expect("session exists")
                .set_disabled_skills(std::collections::HashSet::from(["web-coder".to_owned()]));
        }

        // When assembling the prompt.
        let guard = state.read();
        let result = assemble_prompt(&guard, &session_id, &counter());

        // Then the system prompt skills block excludes disabled skills.
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
            "disabled skill should be excluded from skills block, got: {system}"
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
                p.entries.push(jinn_persona_msg::Persona {
                    name: "custom".to_owned(),
                    description: "Custom persona".to_owned(),
                    body: "You are a custom persona.".to_owned(),
                });
            });
            let mut guard = state.write_test_no_cap();
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
                p.entries.push(jinn_persona_msg::Persona {
                    name: "coding-assistant".to_owned(),
                    description: "Default".to_owned(),
                    body: "You are a coding assistant.".to_owned(),
                });
            });
            let mut guard = state.write_test_no_cap();
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

    #[rstest::rstest]
    #[test]
    fn assemble_prompt_system_sections_appear_in_declared_order() {
        // Given a state where every system section has content.
        let (state, session_id) = state_with_history(vec![]);
        {
            let cell = {
                state
                    .read()
                    .persona_selection()
                    .expect("persona cell attached")
            };
            let () = cell.update(|p| {
                p.entries.push(jinn_persona_msg::Persona {
                    name: "custom".to_owned(),
                    description: "Custom persona".to_owned(),
                    body: "ORDER-MARK-PERSONA".to_owned(),
                });
            });
            let mut guard = state.write_test_no_cap();
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
            {
                let cell = {
                    state
                        .read()
                        .tool_registry()
                        .expect("registry cell attached")
                };
                cell.update(|r| {
                    r.global
                        .insert("ordermark".to_owned(), make_tool("ordermark"));
                });
            }
        }

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
}
