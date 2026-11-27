//! The context figure the status bar shows for a session: how full the
//! model's window is, as `<used>/<max>` in whatever form the two numbers take.
//!
//! One implementation for the terminal status bar and the web UI, so the two
//! surfaces cannot disagree. The used side follows one fallback chain: the
//! freshly assembled prompt size when the context-size actor has one for this
//! session, otherwise the size recorded at the last dispatch, otherwise
//! unknown. The window comes from the model cache.

use jinn_provider_config::{ModelCache, ModelInfo};
use jinn_session_state::ChatSessionState;

/// A context figure, before it is formatted: the used tokens and the model's
/// window, each when known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextFigure {
    /// How many tokens the session's prompt currently occupies, when known.
    pub used: Option<u32>,
    /// The model's context window in tokens, when the cache records one.
    pub max: Option<u32>,
}

/// The figure for `session`'s current model, read from `model_cache`.
///
/// The used side is [`displayed_context_size`]'s chain; the window is the
/// session's model looked up in the cache by `provider/model-id`.
#[must_use]
pub fn context_figure(
    session: &ChatSessionState,
    model_cache: Option<&ModelCache>,
) -> ContextFigure {
    ContextFigure {
        used: displayed_context_size(session),
        max: resolve_context_limit(model_cache, session.profile().model.display_str()),
    }
}

/// The figure as the status bar displays it.
///
/// `<pct>%/<max>` when both halves are known — the percentage to one decimal,
/// the window as a whole-number budget; `0.0%/<max>` when the session has
/// never dispatched; `<used>/???` when the cache records no window for the
/// model; `0/???` when neither is known.
#[must_use]
pub fn format_context_figure(figure: &ContextFigure) -> String {
    match (figure.used, figure.max) {
        (Some(used), Some(max)) => {
            let ctx_used = u64::from(used);
            let max_u64 = u64::from(max);
            let pct = if max_u64 > 0 {
                format!("{:.1}%", (ctx_used as f64 / max_u64 as f64) * 100.0)
            } else {
                "0.0%".to_owned()
            };
            format!("{}/{}", pct, format_budget(max as usize))
        }
        (None, Some(max)) => format!("0.0%/{}", format_budget(max as usize)),
        (Some(used), None) => format!("{}/???", format_tokens(u64::from(used))),
        (None, None) => "0/???".to_owned(),
    }
}

/// The context size the status bar displays for a session.
///
/// A session's cached assembled size is only written for the active session —
/// the context-size actor recalculates `active_session_id()` on
/// context-affecting events — so a background session read straight from the
/// session map carries `None` and would render `0.0%/<max>` even though it has
/// dispatched turns. The last token-ledger record is the fallback: both
/// dispatch writers push the dispatch's `assembled.estimated_tokens()` into
/// `tokens_sent`, which is the same per-dispatch whole-context estimate the
/// actor caches (an estimate of that request's entire prompt, not an increment
/// — so it must be read from one record, never summed across the ledger).
///
/// Cached wins when present: the actor re-assembles with pins, overrides, and
/// pruning applied, which the raw dispatch estimate does not reflect. A
/// session with no cached size and an empty ledger has genuinely never
/// dispatched, so `None` — and the zero display is correct.
fn displayed_context_size(session: &ChatSessionState) -> Option<u32> {
    session.context_size().or_else(|| {
        session
            .token_ledger()
            .last()
            .map(|record| record.tokens_sent)
    })
}

/// Resolve a `provider/model-id` selection against the model cache.
///
/// Returns the model's row when the cache is present and records it under that
/// provider with that id.
#[must_use]
pub fn resolve_model_info<'a>(
    model_cache: Option<&'a ModelCache>,
    active_model: &str,
) -> Option<&'a ModelInfo> {
    let cache = model_cache?;
    let provider_name = active_model.split('/').next()?;
    let models = cache.entries.get(provider_name)?;
    let model_suffix = active_model.get((provider_name.len() + 1)..)?;
    models.iter().find(|m| m.id == model_suffix)
}

/// The model's context window in tokens, when the cache records one.
fn resolve_context_limit(model_cache: Option<&ModelCache>, active_model: &str) -> Option<u32> {
    let info = resolve_model_info(model_cache, active_model)?;
    info.context_length
}

/// Format a token count in human-readable form with one decimal place.
#[expect(
    clippy::cast_precision_loss,
    reason = "loss is negligible for UI calculations"
)]
pub fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

/// Format a token budget as whole numbers only (e.g. `150k`, `1M`, `999`).
#[must_use]
pub fn format_budget(count: usize) -> String {
    if count >= 1_000_000 {
        format!("{}M", count / 1_000_000)
    } else if count >= 1_000 {
        format!("{}k", count / 1_000)
    } else {
        count.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jinn_core_types::ModelSelection;
    use jinn_token_count_msg::TokenRecord;

    /// The provider and model id the cache below is keyed by.
    const PROVIDER: &str = "openrouter";
    const MODEL_ID: &str = "anthropic/claude-sonnet-4";
    const WINDOW: u32 = 200_000;

    /// A session on the cached model, with no history and no dispatch.
    fn session() -> ChatSessionState {
        let mut session = ChatSessionState::new();
        session.set_model(ModelSelection::Single(format!("{PROVIDER}/{MODEL_ID}")));
        session
    }

    /// A cache recording the session's model with the given window.
    fn cache(context_length: Option<u32>) -> ModelCache {
        ModelCache {
            entries: [(
                PROVIDER.to_owned(),
                vec![ModelInfo {
                    id: MODEL_ID.to_owned(),
                    context_length,
                    input_modalities: jinn_provider_config::InputModalities::text(),
                }],
            )]
            .into_iter()
            .collect(),
            last_updated_at: None,
        }
    }

    /// A token record carrying `tokens_sent`, as a dispatch's placeholder does.
    fn record(tokens_sent: u32) -> TokenRecord {
        TokenRecord {
            model_used: None,
            timestamp: jiff::Timestamp::now(),
            tokens_sent,
            tokens_received: 0,
            cost: None,
            prompt_tokens: None,
            cached_tokens: None,
        }
    }

    #[rstest::rstest]
    #[test]
    fn known_window_formats_percentage_over_budget() {
        // Given a session whose assembled prompt is half a known window.
        let mut session = session();
        session.set_context_sizes(100_000, 0);
        let cache = cache(Some(WINDOW));

        // When formatting its figure.
        let label = format_context_figure(&context_figure(&session, Some(&cache)));

        // Then it reads as the percentage over the whole-number budget.
        assert_eq!(label, "50.0%/200k");
    }

    #[rstest::rstest]
    #[test]
    fn unknown_window_formats_used_tokens_over_unknown() {
        // Given a session with an assembled size whose model records no window.
        let mut session = session();
        session.set_context_sizes(100_000, 10_000);
        let cache = cache(None);

        // When formatting its figure.
        let label = format_context_figure(&context_figure(&session, Some(&cache)));

        // Then it reads as the used tokens over an unknown window — no
        // percentage is invented.
        assert_eq!(label, "100.0k/???");
    }

    #[rstest::rstest]
    #[test]
    fn no_cache_formats_used_tokens_over_unknown() {
        // Given a session with an assembled size and no model cache at all.
        let mut session = session();
        session.set_context_sizes(100_000, 10_000);

        // When formatting its figure.
        let label = format_context_figure(&context_figure(&session, None));

        // Then it reads as the used tokens over an unknown window.
        assert_eq!(label, "100.0k/???");
    }

    #[rstest::rstest]
    #[test]
    fn never_dispatched_formats_zero_percentage_over_budget() {
        // Given a session with no assembled size and an empty ledger, on a
        // model with a known window.
        let session = session();
        let cache = cache(Some(WINDOW));

        // When formatting its figure.
        let label = format_context_figure(&context_figure(&session, Some(&cache)));

        // Then it reads as zero over the budget.
        assert_eq!(label, "0.0%/200k");
    }

    #[rstest::rstest]
    #[test]
    fn nothing_known_formats_zero_over_unknown() {
        // Given a session with no size, an empty ledger, and no cache.
        let session = session();

        // When formatting its figure.
        let label = format_context_figure(&context_figure(&session, None));

        // Then it reads as zero over an unknown window.
        assert_eq!(label, "0/???");
    }

    #[rstest::rstest]
    #[test]
    fn no_assembled_size_falls_back_to_the_last_ledger_record() {
        // Given a session that has dispatched once but holds no assembled size.
        let mut session = session();
        session.push_token_record(record(10_000));
        let cache = cache(Some(WINDOW));

        // When formatting its figure.
        let label = format_context_figure(&context_figure(&session, Some(&cache)));

        // Then the used side is that record's estimate: 10,000 of 200,000.
        assert_eq!(label, "5.0%/200k");
    }

    #[rstest::rstest]
    #[test]
    fn ledger_fallback_reads_the_last_record_rather_than_a_sum() {
        // Given a session that dispatched three times and holds no assembled size.
        let mut session = session();
        for _ in 0..3 {
            session.push_token_record(record(10_000));
        }
        let cache = cache(Some(WINDOW));

        // When formatting its figure.
        let label = format_context_figure(&context_figure(&session, Some(&cache)));

        // Then the used side is the last record alone, not the ledger's sum.
        assert_eq!(label, "5.0%/200k");
    }

    #[rstest::rstest]
    #[test]
    fn assembled_size_wins_over_the_ledger_fallback() {
        // Given a session with both an assembled size and a ledger record.
        let mut session = session();
        session.push_token_record(record(10_000));
        session.set_context_sizes(60_000, 0);
        let cache = cache(Some(WINDOW));

        // When formatting its figure.
        let label = format_context_figure(&context_figure(&session, Some(&cache)));

        // Then the assembled size is the used side.
        assert_eq!(label, "30.0%/200k");
    }

    #[rstest::rstest]
    #[test]
    fn zero_window_formats_zero_percentage() {
        // Given a figure whose recorded window is zero.
        let figure = ContextFigure {
            used: Some(5_000),
            max: Some(0),
        };

        // When formatting it.
        let label = format_context_figure(&figure);

        // Then the percentage is zero rather than a division by zero.
        assert_eq!(label, "0.0%/0");
    }
}
