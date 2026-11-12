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

fn enabled_config(workset_trigger_tokens: usize) -> HandoffCompactionConfig {
    HandoffCompactionConfig {
        enabled: true,
        workset_trigger_tokens: Some(workset_trigger_tokens),
    }
}

#[rstest::rstest]
fn gate_passes_when_disabled() {
    // Given handoff compaction disabled and a work size far past any line.
    let config = HandoffCompactionConfig::default();

    // When evaluating the gate.
    let decision = evaluate_handoff_gate(&config, Some(1000));

    // Then the request dispatches normally.
    assert_eq!(decision, GateDecision::Pass);
}

#[rstest::rstest]
fn gate_passes_without_a_cached_size_pair() {
    // Given handoff enabled with a trigger but no honest size pair cached
    // (a compaction just invalidated the cache, say).
    let config = enabled_config(600);

    // When evaluating the gate with no work size.
    let decision = evaluate_handoff_gate(&config, None);

    // Then the gate never guesses: it passes.
    assert_eq!(decision, GateDecision::Pass);
}

#[rstest::rstest]
fn gate_fires_on_work_not_wire() {
    // Given a 600-token work trigger.
    let config = enabled_config(600);

    // When the work size (assembled minus the constant floor) reaches the
    // trigger — regardless of how large the floor made the wire total.
    let decision = evaluate_handoff_gate(&config, Some(600));

    // Then the pending request is replaced by the handoff.
    assert_eq!(decision, GateDecision::Fire);
}

#[rstest::rstest]
fn gate_passes_when_a_large_system_floor_holds_the_work_small() {
    // Given a 60_000-token system floor and a 60_000-token work trigger.
    let config = enabled_config(60_000);

    // When the wire total is huge but the work above the floor is small
    // (the floor can no longer sit at the fire line).
    let decision = evaluate_handoff_gate(&config, Some(10_000));

    // Then the request dispatches normally.
    assert_eq!(decision, GateDecision::Pass);
}

#[rstest::rstest]
fn gate_passes_below_the_trigger() {
    // Given a 600-token work trigger.
    let config = enabled_config(600);

    // When the work is well under the trigger.
    let decision = evaluate_handoff_gate(&config, Some(590));

    // Then the request dispatches normally.
    assert_eq!(decision, GateDecision::Pass);
}

#[rstest::rstest]
fn gate_passes_without_a_configured_trigger() {
    // Given an enabled config whose fire line was never set (a config
    // from before the field existed, or a deliberately unconfigured one).
    let config = HandoffCompactionConfig {
        enabled: true,
        workset_trigger_tokens: None,
    };

    // When evaluating the gate.
    let decision = evaluate_handoff_gate(&config, Some(60_000));

    // Then the gate never fires an unbounded handoff.
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
