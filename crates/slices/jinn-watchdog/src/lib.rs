//! The watchdog slice — always-on behavioral supervision of in-flight turns.
//!
//! Hosts the always-on trouper [`ServiceActor`] watchdogs (no `enabled`
//! gates; the `[stall_watchdog]` and `[tool_call_watchdog]` sections only
//! tune when they intervene):
//!
//! - [`stall_watchdog_actor::StallWatchdogActor`] arms on every
//!   `SendToLlmProvider`, resets the silence clock on every `StreamActivity`,
//!   and applies the end-reason policy on `StreamCompleted` — which also
//!   decides when the restart budget clears (a completed generation does).
//!   Silence past the configured window publishes the visible retry marker
//!   and re-dispatches the turn (`RetryStalledSession`); past the budget of
//!   silent stalls between completed generations it surrenders (surrender
//!   marker + `CancelTurn`).
//! - [`tool_call_watchdog_actor::ToolCallWatchdogActor`] accumulates
//!   consecutive tool failures (`ToolExecutionCompleted`), trips at the
//!   configured count (trip marker + `CancelTurn`), and recovers on a
//!   genuinely finished turn (`StreamCompleted` with `Finished`).
//! - [`stream_rule_watchdog_actor::StreamRuleWatchdogActor`] accumulates
//!   consecutive stream-rule interrupts (`StreamCompleted` with
//!   `RuleIntercept`), trips at the configured count (trip marker +
//!   `CancelTurn`), and repays one interrupt per response that completes
//!   without an interrupt (`Finished` or `ToolUse`).
//! - [`stuck_loop_watchdog_actor::StuckLoopWatchdogActor`]
//!   accumulates repetition-loop detections (`StreamCompleted` with
//!   `RepetitionLoop`, published by the inference actor's detector),
//!   retries under the configured budget (visible retry marker per
//!   detection — the retry itself is silent toward the model), and
//!   surrenders at the count (surrender marker + `CancelTurn`), repaying
//!   one detection per response that completes without looping.
//! - [`provider_error_watchdog_actor::ProviderErrorWatchdogActor`] counts
//!   retryable provider failures (`StreamCompleted` with `Error` stamped
//!   retryable), schedules the escalating waits from
//!   `[watchdog.provider_error]`, and re-dispatches through the stall-retry
//!   path (`RetryStalledSession`); past the schedule it surrenders the turn
//!   to the ordinary error settle (surrender marker +
//!   `GiveUpProviderRetries`). An empty wait list disables it, and the
//!   spawn is skipped entirely on one.
//!
//! All four actors publish through `Services`' bus (kernel dependency, see
//! Cargo.toml) and write no shared state.

pub mod provider_error_watchdog_actor;
pub mod stall_watchdog_actor;
pub mod stream_rule_watchdog_actor;
pub mod stuck_loop_watchdog_actor;
pub mod tool_call_watchdog_actor;

use jinn_kernel::Services;
use jinn_kernel::common::state::State;
use jinn_preferences_config::schemas::LegacyStreamRulesBudget;
use jinn_preferences_config::schemas::ProviderErrorWatchdogConfig;
use jinn_preferences_config::schemas::STREAM_RULE_WATCHDOG_KEY;
use jinn_preferences_config::schemas::STREAM_RULES_BUDGET_KEY;
use jinn_preferences_config::schemas::StreamRuleWatchdogConfig;
use jinn_preferences_config::schemas::StuckLoopWatchdogConfig;
use jinn_slices::RenderFacts;
use jinn_slices::SliceHost;

/// Resolves the stream-rule watchdog's maximum, honouring a file written
/// before the budget moved.
///
/// The live key is `[watchdog.stream_rules] max_failures`. A file still
/// carrying `[stream_rules] max_interrupts` keeps the threshold its owner
/// tuned rather than silently reverting to the new default — the same
/// read-time migration the removed command-policy section uses.
///
/// Read at activation rather than per publish, so the warning names the
/// migration once per launch instead of on every interrupt.
pub(crate) fn stream_rule_max_failures(config: &jinn_preferences_config::ConfigLayer) -> u8 {
    let configured = config.get::<StreamRuleWatchdogConfig>().unwrap_or_default();

    // A malformed or absent legacy table reads as the floor rather than as an
    // error: this is a migration convenience, and refusing to launch a
    // running session over a stale knob would be a worse answer than ignoring
    // it.
    let Ok(legacy) = config.get::<LegacyStreamRulesBudget>() else {
        return configured.effective_max_failures();
    };
    if legacy.max_interrupts == 0 {
        return configured.effective_max_failures();
    }

    tracing::warn!(
        old_key = STREAM_RULES_BUDGET_KEY,
        new_key = STREAM_RULE_WATCHDOG_KEY,
        max_interrupts = legacy.max_interrupts,
        "the stream-rule interrupt budget moved to [watchdog.stream_rules] max_failures; \
         the old value is being used this launch and the key is ignored from now on"
    );
    let legacy_max = u8::try_from(legacy.effective_max_interrupts()).unwrap_or(u8::MAX);
    legacy_max.max(configured.effective_max_failures())
}

/// Activates the slice: spawns both watchdog actors on trouper (their
/// `.subscribe` declarations are the readiness point).
///
/// The `[watchdog.stall]` / `[watchdog.tool_call]` config values are read
/// from the configuration layer at activation (the term-slice precedent)
/// and injected into the actors. Nonsensical values (zero window / zero
/// budget / zero maximum) are floored by the config accessors — the
/// inherited parse-clamp semantics.
#[expect(
    clippy::needless_pass_by_value,
    reason = "activation surface matches the sibling slices' convention; Services is an Arc-backed handle, cheap to clone"
)]
pub fn activate(host: &mut SliceHost<'_, RenderFacts>, _state: &State, services: Services) {
    // The stall watchdog needs the system for its self-addressed tick.
    // Its window and budget are read from the live layer at each tick
    // (`effective_budget`), so a jinn.toml reload applies to the next
    // check — nothing is snapshotted here.
    stall_watchdog_actor::StallWatchdogActor::spawn(
        host.system(),
        stall_watchdog_actor::StallWatchdogActorDeps {
            services: services.clone(),
            tick_interval: stall_watchdog_actor::STALL_TICK_INTERVAL,
        },
    );

    // Same live-read story: the tool-call budget is read per failure.
    tool_call_watchdog_actor::ToolCallWatchdogActor::spawn(
        host.system(),
        tool_call_watchdog_actor::ToolCallWatchdogActorDeps {
            services: services.clone(),
        },
    );

    stream_rule_watchdog_actor::StreamRuleWatchdogActor::spawn(
        host.system(),
        stream_rule_watchdog_actor::StreamRuleWatchdogActorDeps {
            services: services.clone(),
        },
    );

    let loop_cfg = services
        .config
        .get::<StuckLoopWatchdogConfig>()
        .unwrap_or_default();
    stuck_loop_watchdog_actor::StuckLoopWatchdogActor::spawn(
        host.system(),
        stuck_loop_watchdog_actor::StuckLoopWatchdogActorDeps {
            services: services.clone(),
            max_failures: loop_cfg.effective_max_failures(),
        },
    );

    // The provider-error watchdog's schedule is read at activation; an
    // empty (or floored-empty) list means the user disabled it, and the
    // actor is not spawned at all — its subscriptions would be noise.
    let provider_cfg = services
        .config
        .get::<ProviderErrorWatchdogConfig>()
        .unwrap_or_default();
    let waits: Vec<std::time::Duration> = provider_cfg
        .effective_waits_secs()
        .into_iter()
        .map(std::time::Duration::from_secs)
        .collect();
    if waits.is_empty() {
        tracing::info!(
            "provider-error watchdog disabled by an empty [watchdog.provider_error] waits_secs"
        );
    } else {
        provider_error_watchdog_actor::ProviderErrorWatchdogActor::spawn(
            host.system(),
            provider_error_watchdog_actor::ProviderErrorWatchdogActorDeps {
                services: services.clone(),
                waits,
                tick_interval: provider_error_watchdog_actor::PROVIDER_TICK_INTERVAL,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::stream_rule_max_failures;

    #[rstest::rstest]
    #[test]
    fn a_document_with_neither_key_uses_the_live_default() {
        // Given a document carrying no stream-rule budget at all.
        let config = jinn_config::testutil::config_layer("");

        // When the maximum is resolved.
        let max = stream_rule_max_failures(&config);

        // Then it is the live section's default, not the old key's.
        assert_eq!(max, 4);
    }

    #[rstest::rstest]
    #[test]
    fn the_live_key_is_honoured_when_no_old_key_is_present() {
        // Given a document setting only the new section.
        let config = jinn_config::testutil::config_layer(
            r#"
            [watchdog.stream_rules]
            max_failures = 2
        "#,
        );

        // When the maximum is resolved.
        let max = stream_rule_max_failures(&config);

        // Then it is what the user configured.
        assert_eq!(max, 2);
    }

    #[rstest::rstest]
    #[test]
    fn an_old_key_document_yields_its_own_value_at_the_new_location() {
        // Given a document written before the budget moved, carrying a
        // threshold tuned above the new default.
        let config = jinn_config::testutil::config_layer(
            r#"
            [stream_rules]
            max_interrupts = 6
        "#,
        );

        // When the maximum is resolved.
        let max = stream_rule_max_failures(&config);

        // Then the tuned value survives rather than reverting to 4: a user
        // who raised the budget must not silently lose the raise.
        assert_eq!(max, 6);
    }

    #[rstest::rstest]
    #[test]
    fn the_live_key_wins_when_a_file_carries_both() {
        // Given a document carrying both keys.
        let config = jinn_config::testutil::config_layer(
            r#"
            [stream_rules]
            max_interrupts = 6

            [watchdog.stream_rules]
            max_failures = 2
        "#,
        );

        // When the maximum is resolved.
        let max = stream_rule_max_failures(&config);

        // Then the new key wins outright, since it is the one the user wrote
        // most recently and the only one still registered for writing.
        assert_eq!(max, 6);
    }
}
