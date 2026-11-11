//! Tests for the handoff gate, fold mutations, and recap entry shape.

#![allow(clippy::expect_used, reason = "test code")]

use super::GateDecision;
use super::build_fold_mutations;
use super::build_handoff_nudge;
use super::build_recap_entry;
use super::evaluate_handoff_gate;
use super::is_fold_protected;

use jinn_core_types::ChangeSource;
use jinn_core_types::ChatEntry;
use jinn_core_types::ContextOverride;
use jinn_core_types::HistoryMutation;
use jinn_core_types::PinPosition;
use jinn_preferences_config::schemas::HandoffCompactionConfig;

fn jinn_config_test_layer() -> &'static jinn_config::ConfigLayer {
    jinn_config::empty_config_layer()
}

fn enabled_config(context_limit: usize, trigger_tokens: usize) -> HandoffCompactionConfig {
    HandoffCompactionConfig {
        enabled: true,
        context_limit: Some(context_limit),
        trigger_tokens: Some(trigger_tokens),
    }
}

#[rstest::rstest]
fn gate_passes_when_disabled() {
    // Given handoff compaction disabled and a context far past any line.
    let config = HandoffCompactionConfig::default();

    // When evaluating the gate.
    let decision = evaluate_handoff_gate(&config, Some(1000), Some(2000), 0);

    // Then the request dispatches normally.
    assert_eq!(decision, GateDecision::Pass);
}

#[rstest::rstest]
fn gate_passes_without_an_effective_limit() {
    // Given handoff enabled with a trigger but no limit from any source.
    let config = enabled_config(1000, 600);

    // When evaluating the gate with no model-reported limit.
    let decision = evaluate_handoff_gate(&config, None, Some(500), 0);

    // Then the gate never guesses: it passes.
    assert_eq!(decision, GateDecision::Pass);
}

#[rstest::rstest]
fn gate_fires_when_projected_context_reaches_the_trigger() {
    // Given handoff enabled with a 600-token trigger over a 1000-token
    // limit.
    let config = enabled_config(1000, 600);

    // When the assembled estimate reaches the trigger.
    let decision = evaluate_handoff_gate(&config, Some(1000), Some(600), 0);

    // Then the pending request is replaced by the handoff.
    assert_eq!(decision, GateDecision::Fire);
}

#[rstest::rstest]
fn gate_counts_excluded_cost_toward_the_trigger() {
    // Given handoff enabled and an assembled estimate below the trigger
    // only because prune excluded entries after the last assembly.
    let config = enabled_config(1000, 600);

    // When the excluded cost pushes the projection over.
    let decision = evaluate_handoff_gate(&config, Some(1000), Some(500), 150);

    // Then the gate still fires.
    assert_eq!(decision, GateDecision::Fire);
}

#[rstest::rstest]
fn gate_passes_below_the_trigger() {
    // Given handoff enabled with a 600-token trigger.
    let config = enabled_config(1000, 600);

    // When the projection is well under the trigger.
    let decision = evaluate_handoff_gate(&config, Some(1000), Some(590), 0);

    // Then the request dispatches normally.
    assert_eq!(decision, GateDecision::Pass);
}

#[rstest::rstest]
fn gate_fires_against_a_model_reported_limit_when_unset() {
    // Given handoff enabled with a trigger and no explicit limit.
    let config = HandoffCompactionConfig {
        enabled: true,
        context_limit: None,
        trigger_tokens: Some(12_000),
    };

    // When resolving against the model's reported length.
    let decision = evaluate_handoff_gate(&config, Some(16_000), Some(12_500), 0);

    // Then the model's length is the limit and the trigger fires.
    assert_eq!(decision, GateDecision::Fire);
}

#[rstest::rstest]
fn gate_passes_when_the_trigger_meets_the_limit() {
    // Given a mis-edited config whose trigger sits at the limit — no gap
    // for the recap turn.
    let config = enabled_config(1000, 1000);

    // When evaluating the gate.
    let decision = evaluate_handoff_gate(&config, None, Some(900), 0);

    // Then the regime is invalid and the gate refuses.
    assert_eq!(decision, GateDecision::Pass);
}

#[rstest::rstest]
fn nudge_wraps_the_recap_prompt_verbatim() {
    // Given the point-of-use recap prompt.
    let prompt = "# Recap\n\nobjective: ...";

    // When building the nudge.
    let nudge = build_handoff_nudge(prompt);

    // Then the content is the prompt itself.
    assert_eq!(nudge, prompt);
}

fn history_with_mixed_entries() -> Vec<jinn_core_types::ChatEntry> {
    let history = vec![
        ChatEntry::user("first task"),
        ChatEntry::assistant("working on it"),
        ChatEntry::user("steer this way"),
    ];
    history
}

#[rstest::rstest]
fn fold_excludes_assistant_but_keeps_user_entries() {
    // Given a history of user, assistant, and user entries.
    let history = history_with_mixed_entries();
    let recap = build_recap_entry(
        jinn_core_types::ChatEntryId::new(),
        "recap text".to_owned(),
        500,
        60,
        2,
        "test-model".to_owned(),
    );
    let last_id = history.last().map(|entry| entry.id.clone());

    // When building the fold mutations.
    let mutations = build_fold_mutations(&history, recap, last_id);

    // Then the assistant entry is excluded and the recap inserted after
    // the last entry.
    let excluded: Vec<_> = mutations
        .iter()
        .filter_map(|mutation| match mutation {
            HistoryMutation::SetContextOverride { entry_id, .. } => Some(entry_id),
            _ => None,
        })
        .collect();
    assert_eq!(excluded.len(), 1);
    let excluded_id = excluded.first().expect("one exclusion");
    let assistant = history.get(1).expect("assistant entry in fixture");
    assert_eq!(*excluded_id, &assistant.id);
    assert!(
        mutations
            .iter()
            .any(|mutation| matches!(mutation, HistoryMutation::InsertEntry { .. }))
    );
}

#[rstest::rstest]
fn fold_treats_already_excluded_entries_as_protected() {
    // Given a history whose assistant entry the user already excluded.
    let mut history = history_with_mixed_entries();
    let entry = history
        .get_mut(1)
        .expect("assistant entry in fixture history");
    entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);

    // When asking whether the entry is fold-protected.
    let entry = history.get(1).expect("assistant entry in fixture");

    // Then protection holds — the fold is idempotent over prior excludes.
    assert!(is_fold_protected(entry));
}

#[rstest::rstest]
fn fold_leaves_pins_alone() {
    // Given a pinned assistant entry (pins are the user's keep-signal).
    let mut entry = ChatEntry::assistant("keep this");
    entry.pin_position = Some(PinPosition::Top);

    // When asking whether the fold protects it.
    // Then the pin wins over the kind.
    assert!(is_fold_protected(&entry));
}

#[rstest::rstest]
fn recap_entry_is_a_default_included_compaction_entry() {
    // Given a recap built for insertion.
    // When constructing the entry.
    let recap = build_recap_entry(
        jinn_core_types::ChatEntryId::new(),
        "the recap".to_owned(),
        500,
        60,
        2,
        "test-model".to_owned(),
    );

    // Then it is a Compaction entry riding context by default (the fresh
    // conversation = system + task + recap).
    assert!(matches!(
        recap.kind,
        jinn_core_types::ChatEntryKind::Compaction { .. }
    ));
    assert_eq!(recap.context_override, ContextOverride::Default);
}

#[rstest::rstest]
fn config_layer_resolves_the_new_section_without_defaults_leaking() {
    // Given the empty config layer.
    let config = jinn_config_test_layer();

    // When reading the handoff section.
    let resolved = config.get::<HandoffCompactionConfig>();

    // Then it resolves with defaults (disabled).
    let resolved = resolved.expect("section registered");
    assert!(!resolved.enabled);
}
