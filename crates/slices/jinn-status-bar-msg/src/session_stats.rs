//! The statistics the status bar shows for one session, and for its tree.
//!
//! One implementation for the terminal status bar, the sidebar's session
//! preview, and the web UI's dash, so the three surfaces cannot disagree.
//! Everything here is a pure function over data the caller hands in: the
//! session, the model cache, and a working time already measured. The
//! numbers are returned as plain data, and the display strings — the
//! terminal's own formats — are methods on that data, so a surface that
//! renders text and a surface that ships it down a wire read the same
//! labels.
//!
//! The tree aggregate is the session plus every descendant and frozen node
//! under the tree's root. Whether the bar shows it at all is the TUI's own
//! rule, shared as [`tree_is_shown`].

use std::collections::HashMap;
use std::hash::BuildHasher;

use jiff::SignedDuration;
use jinn_common::shorten_path;
use jinn_core_types::{SessionId, WorkingInterval, format_working_duration};
use jinn_provider_config::{InputModalities, ModelCache};
use jinn_provider_selection_msg::resolve_effort;
use jinn_session_state::{
    ChatSessionState, FrozenTreeNode, aggregate_tree_stats, compute_turn_count,
};
use jinn_token_count_msg::TokenStats;

use crate::{
    ContextFigure, context_figure, format_context_figure, format_tokens, resolve_model_info,
};

/// A displayed cache-hit percentage at or above this is healthy.
pub const CACHE_HIT_HEALTHY: u32 = 95;
/// A displayed cache-hit percentage at or above this, and below
/// [`CACHE_HIT_HEALTHY`], is degraded; below it is poor.
pub const CACHE_HIT_DEGRADED: u32 = 90;

/// Every figure the status bar shows for one session, as plain data.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionStats {
    /// The session's working directory, home collapsed to `~`.
    pub cwd_display: String,
    /// The cache-hit percentage over measured turns, or `None` when no
    /// measured turn reported a cache hit — the bar then shows no figure.
    pub cache_hit_percent: Option<u32>,
    /// Provider-reported prompt tokens, with the local estimate as fallback.
    pub effective_sent: u64,
    /// Provider-reported output tokens.
    pub total_received: u64,
    /// How full the model's window is.
    pub context_figure: ContextFigure,
    /// The summed provider-reported cost; the bar prints its absolute value.
    pub total_cost: f64,
    /// The derived turn count, inherited fork entries excluded.
    pub turn_count: u32,
    /// The session's working time, in whole seconds.
    pub working_seconds: u64,
    /// The resolved model display: provider, effort, and modality indicator.
    pub model: String,
}

impl SessionStats {
    /// The sent tokens as the bar labels them.
    #[must_use]
    pub fn tokens_sent_label(&self) -> String {
        format_tokens(self.effective_sent)
    }

    /// The received tokens as the bar labels them.
    #[must_use]
    pub fn tokens_received_label(&self) -> String {
        format_tokens(self.total_received)
    }

    /// The context figure as the bar labels it.
    #[must_use]
    pub fn context_label(&self) -> String {
        format_context_figure(&self.context_figure)
    }

    /// The cost as the bar labels it: five decimal places, never negative.
    #[must_use]
    pub fn cost_label(&self) -> String {
        format!("${:.5}", self.total_cost.abs())
    }

    /// The working time as the bar labels it: `HH:MM:SS`.
    #[must_use]
    pub fn working_label(&self) -> String {
        format_working_duration(&SignedDuration::from_secs(
            i64::try_from(self.working_seconds).unwrap_or(i64::MAX),
        ))
    }
}

/// The session's statistics, read from its ledger, history, profile, and cwd.
///
/// `working_time` is measured by the caller, because the intervals it is
/// measured over live in a cell a pure function cannot reach. `model_cache`
/// supplies the window and the modalities; `None` degrades exactly as the
/// bar does with no provider cell.
#[must_use]
pub fn session_stats(
    session: &ChatSessionState,
    model_cache: Option<&ModelCache>,
    working_time: SignedDuration,
) -> SessionStats {
    let token_stats = TokenStats::from_ledger(session.token_ledger());
    SessionStats {
        cwd_display: shorten_path(session.cwd()),
        cache_hit_percent: cache_hit_percent_from(
            token_stats.cached_total,
            token_stats.measured_sent,
        ),
        effective_sent: token_stats.effective_sent,
        total_received: token_stats.total_received,
        context_figure: context_figure(session, model_cache),
        total_cost: TokenStats::total_cost(session.token_ledger()),
        turn_count: compute_turn_count(session.history(), session.fork_ordinal()),
        working_seconds: u64::try_from(working_time.as_secs()).unwrap_or_default(),
        model: model_string(model_cache, session),
    }
}

/// The tree aggregate: every figure summed over the session's whole tree.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TreeStats {
    /// How many live sessions and frozen nodes the tree holds.
    pub session_count: usize,
    /// The tree's cache-hit percentage over measured turns, when it has one.
    pub cache_hit_percent: Option<u32>,
    /// The tree's effective sent tokens.
    pub effective_sent: u64,
    /// The tree's received tokens.
    pub total_received: u64,
    /// The tree's summed cost.
    pub total_cost: f64,
    /// The tree's summed turns.
    pub turn_count: u32,
    /// The time the tree was working — a second shared by several members
    /// counted once — in whole seconds.
    pub working_seconds: u64,
}

impl TreeStats {
    /// The tree's sent tokens as the bar labels them.
    #[must_use]
    pub fn tokens_sent_label(&self) -> String {
        format_tokens(self.effective_sent)
    }

    /// The tree's received tokens as the bar labels them.
    #[must_use]
    pub fn tokens_received_label(&self) -> String {
        format_tokens(self.total_received)
    }

    /// The tree's cost as the bar labels it.
    #[must_use]
    pub fn cost_label(&self) -> String {
        format!("${:.5}", self.total_cost.abs())
    }

    /// The tree's working time as the bar labels it.
    #[must_use]
    pub fn working_label(&self) -> String {
        format_working_duration(&SignedDuration::from_secs(
            i64::try_from(self.working_seconds).unwrap_or(i64::MAX),
        ))
    }
}

/// The statistics of the tree `session_id` belongs to.
///
/// `live_intervals` carries every live member's working intervals — the
/// work-time cell's snapshot — because the aggregate cannot read them off
/// the sessions. A member absent from it contributes no working time.
#[must_use]
pub fn tree_stats<S: BuildHasher>(
    sessions: &HashMap<SessionId, ChatSessionState, S>,
    frozen_nodes: &HashMap<SessionId, FrozenTreeNode, S>,
    session_id: &SessionId,
    live_intervals: &HashMap<SessionId, Vec<WorkingInterval>>,
) -> TreeStats {
    let tree = aggregate_tree_stats(sessions, frozen_nodes, session_id, live_intervals);
    TreeStats {
        session_count: tree.session_count,
        cache_hit_percent: cache_hit_percent_from(tree.cached_total, tree.measured_sent),
        effective_sent: tree.effective_sent,
        total_received: tree.total_received,
        total_cost: tree.total_cost,
        turn_count: tree.total_turns,
        working_seconds: u64::try_from(tree.total_working.as_secs()).unwrap_or_default(),
    }
}

/// Whether the status bar shows the tree aggregate at all: only a tree of
/// more than one member has anything to aggregate.
#[must_use]
pub fn tree_is_shown(tree: &TreeStats) -> bool {
    tree.session_count > 1
}

/// The cache-hit percentage over measured turns only, or `None` when there
/// are no cache hits or no measured turns — so the glyph stays hidden for
/// providers and turns that report no usage. The denominator is
/// provider-reported `prompt_tokens`, so a cancelled turn, which reports no
/// usage, is excluded from both halves.
#[must_use]
pub fn cache_hit_percent_from(cached_total: u64, measured_sent: u64) -> Option<u32> {
    if cached_total == 0 || measured_sent == 0 {
        return None;
    }
    let pct = (cached_total as f64 / measured_sent as f64) * 100.0;
    Some(pct.round() as u32)
}

/// The model display string: resolved name, reasoning effort, modality.
fn model_string(model_cache: Option<&ModelCache>, session: &ChatSessionState) -> String {
    let active_model = session.profile().model.clone();
    let (model, resolved_for_lookup) = {
        // For an Alloy, the bar surfaces the last-rotated member (which one
        // actually answered) from the token ledger, falling back to the first
        // member before any dispatch. For a Single selection the model _is_ the
        // source of truth — never read the historical ledger, which would show
        // a stale model after switching providers via the picker.
        let last_dispatched = active_model.as_alloy().and_then(|_| {
            session
                .token_ledger()
                .last()
                .and_then(|record| record.model_used.clone())
        });
        let resolved = last_dispatched
            .as_deref()
            .unwrap_or_else(|| active_model.display_str());
        if active_model.is_no_provider() {
            ("no model selected".to_owned(), None)
        } else {
            let display = if let Some((provider, model_id)) = resolved.split_once('/') {
                format!("({provider})/{model_id}")
            } else {
                resolved.to_owned()
            };
            (display, Some(resolved.to_owned()))
        }
    };
    let model = match active_model.as_alloy() {
        Some(alloy) => format!("[alloy {}] {model}", alloy.models.len()),
        None => model,
    };
    // Append the resolved reasoning effort as `[<mode>]`, omitted entirely
    // when no effort is resolved.
    let model = match resolve_effort(session.profile().reasoning_effort) {
        Some(effort) => format!("{model} [{}]", effort.as_str()),
        None => model,
    };
    // The modality indicator follows the effort bracket, read from the SAME
    // resolved model the name shows. An unknown model shows `<t>`: text is
    // always available.
    let modalities = resolved_for_lookup.map(|resolved| {
        resolve_modalities(model_cache, &resolved).unwrap_or_else(InputModalities::text)
    });
    match modalities {
        Some(m) => format!("{model} <{}>", m.display()),
        None => model,
    }
}

/// The cached modalities of `resolved`, when the cache records the model.
fn resolve_modalities(model_cache: Option<&ModelCache>, resolved: &str) -> Option<InputModalities> {
    resolve_model_info(model_cache, resolved).map(|info| info.input_modalities)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        reason = "test assertions need expect, and a let-else on a known variant is unreachable by construction"
    )]
    use super::*;
    use jinn_core_types::{ChatEntry, ModelSelection, WorkingInterval};
    use jinn_provider_config::ModelInfo;
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

    /// A cache recording the session's model with a known window.
    fn cache() -> ModelCache {
        ModelCache {
            entries: [(
                PROVIDER.to_owned(),
                vec![ModelInfo {
                    id: MODEL_ID.to_owned(),
                    context_length: Some(WINDOW),
                    input_modalities: InputModalities::text(),
                }],
            )]
            .into_iter()
            .collect(),
            last_updated_at: None,
        }
    }

    /// A token record as a dispatch leaves one: prompt tokens, cache hits,
    /// received tokens, and a cost.
    fn record(tokens_sent: u32, received: u32, prompt: u32, cached: u32, cost: f64) -> TokenRecord {
        TokenRecord {
            model_used: None,
            timestamp: jiff::Timestamp::now(),
            tokens_sent,
            tokens_received: received,
            cost: Some(cost),
            prompt_tokens: Some(prompt),
            cached_tokens: Some(cached),
        }
    }

    /// A session that dispatched twice and assembled a prompt of 100k tokens.
    fn dispatched_session() -> ChatSessionState {
        let mut session = session();
        session.push_token_record(record(10_000, 2_000, 9_000, 8_100, 0.01234));
        session.push_token_record(record(10_000, 3_000, 8_000, 7_200, 0.00666));
        session.set_context_sizes(100_000, 5_000);
        session
    }

    #[rstest::rstest]
    #[test]
    fn sent_tokens_label_the_provider_reported_sum() {
        // Given a session whose measured turns reported 9,000 and 8,000 prompt tokens.
        let session = dispatched_session();

        // When computing its statistics.
        let stats = session_stats(&session, Some(&cache()), SignedDuration::ZERO);

        // Then the sent label is their sum, in the terminal's format.
        assert_eq!(stats.tokens_sent_label(), "17.0k");
    }

    #[rstest::rstest]
    #[test]
    fn received_tokens_label_the_sum() {
        // Given a session that received 2,000 then 3,000 tokens.
        let session = dispatched_session();

        // When computing its statistics.
        let stats = session_stats(&session, Some(&cache()), SignedDuration::ZERO);

        // Then the received label is their sum.
        assert_eq!(stats.tokens_received_label(), "5.0k");
    }

    #[rstest::rstest]
    #[test]
    fn context_label_is_the_shared_figure() {
        // Given a session with an assembled size of 100k on a 200k window.
        let session = dispatched_session();

        // When computing its statistics.
        let stats = session_stats(&session, Some(&cache()), SignedDuration::ZERO);

        // Then the context label is the context figure's own format.
        assert_eq!(stats.context_label(), "50.0%/200k");
    }

    #[rstest::rstest]
    #[test]
    fn cost_label_sums_the_ledger_to_five_decimals() {
        // Given a session whose two turns cost 0.01234 and 0.00666.
        let session = dispatched_session();

        // When computing its statistics.
        let stats = session_stats(&session, Some(&cache()), SignedDuration::ZERO);

        // Then the cost label is their sum, five decimals, no sign.
        assert_eq!(stats.cost_label(), "$0.01900");
    }

    #[rstest::rstest]
    #[test]
    fn cache_hit_percent_is_measured_turns_only() {
        // Given a session with 15,300 cached of 17,000 measured prompt tokens.
        let session = dispatched_session();

        // When computing its statistics.
        let stats = session_stats(&session, Some(&cache()), SignedDuration::ZERO);

        // Then the percentage is their ratio, rounded.
        assert_eq!(stats.cache_hit_percent, Some(90));
    }

    #[rstest::rstest]
    #[test]
    fn cache_hit_percent_is_none_without_cache_hits() {
        // Given a session that dispatched with no cache hits reported.
        let mut session = session();
        session.push_token_record(record(10_000, 2_000, 9_000, 0, 0.01));

        // When computing its statistics.
        let stats = session_stats(&session, Some(&cache()), SignedDuration::ZERO);

        // Then no percentage is derived.
        assert_eq!(stats.cache_hit_percent, None);
    }

    #[rstest::rstest]
    #[test]
    fn working_label_formats_whole_seconds_as_hours_minutes_seconds() {
        // Given a session that worked for one hour, two minutes, three seconds, and a fraction.
        let working = SignedDuration::from_secs(3_723)
            .checked_add(SignedDuration::from_nanos(900_000_000))
            .expect("a duration");

        // When computing its statistics.
        let stats = session_stats(&session(), Some(&cache()), working);

        // Then the label is the whole seconds in HH:MM:SS.
        assert_eq!(stats.working_label(), "01:02:03");
    }

    #[rstest::rstest]
    #[test]
    fn turn_count_counts_each_user_entry_and_each_final_reply() {
        // Given a session with two user entries and one final assistant reply.
        let mut session = session();
        session.push_entry(ChatEntry::user("one"));
        session.push_entry(ChatEntry::assistant("two"));
        session.push_entry(ChatEntry::user("three"));

        // When computing its statistics.
        let stats = session_stats(&session, Some(&cache()), SignedDuration::ZERO);

        // Then the terminal's own rule counts three turns.
        assert_eq!(stats.turn_count, 3);
    }

    #[rstest::rstest]
    #[test]
    fn turn_count_does_not_count_a_reply_that_continues_into_a_tool_call() {
        // Given one user turn whose reply continues into a tool call.
        let mut session = session();
        session.push_entry(ChatEntry::user("look"));
        session.push_entry(ChatEntry::assistant("checking"));
        session.push_entry(ChatEntry::tool_call("call-1", "read", "{}"));

        // When computing its statistics.
        let stats = session_stats(&session, Some(&cache()), SignedDuration::ZERO);

        // Then it is one turn: the mid-loop reply is not its own.
        assert_eq!(stats.turn_count, 1);
    }

    #[rstest::rstest]
    #[test]
    fn model_label_carries_provider_effort_and_modality() {
        // Given a session on a cached text model with a reasoning effort set.
        let mut session = session();
        session.profile_mut().reasoning_effort = Some(jinn_core_types::ReasoningEffort::High);

        // When computing its statistics.
        let stats = session_stats(&session, Some(&cache()), SignedDuration::ZERO);

        // Then the model reads as provider/model, the effort, and text.
        assert_eq!(
            stats.model,
            "(openrouter)/anthropic/claude-sonnet-4 [high] <t>"
        );
    }

    #[rstest::rstest]
    #[test]
    fn cwd_display_is_the_session_cwd_shortened() {
        // Given a fresh session on its default cwd.
        let session = session();

        // When computing its statistics.
        let stats = session_stats(&session, Some(&cache()), SignedDuration::ZERO);

        // Then the display is the terminal's own shortening of that cwd.
        assert_eq!(stats.cwd_display, shorten_path(session.cwd()));
    }

    // --- the tree ---

    /// A parent and one child session, the child linked to the parent.
    fn parent_and_child() -> (SessionId, ChatSessionState, ChatSessionState) {
        let mut parent = session();
        parent.push_token_record(record(1_200, 600, 1_000, 960, 0.01));
        let parent_id = parent.session_id().clone();
        let mut child = session();
        child.push_token_record(record(500, 250, 200, 192, 0.005));
        child.set_parent_session(parent_id.clone());
        (parent_id, parent, child)
    }

    /// Two intervals over the same ten seconds: the parent's and the child's.
    fn overlapping_intervals(
        parent: &SessionId,
        child: &SessionId,
    ) -> HashMap<SessionId, Vec<WorkingInterval>> {
        let start = jiff::Timestamp::from_second(1_000_000).expect("a timestamp");
        let end = jiff::Timestamp::from_second(1_000_010).expect("a timestamp");
        HashMap::from([
            (parent.clone(), vec![WorkingInterval::closed(start, end)]),
            (child.clone(), vec![WorkingInterval::closed(start, end)]),
        ])
    }

    #[rstest::rstest]
    #[test]
    fn tree_totals_sum_every_member() {
        // Given a parent and a child in one tree, each with one measured turn.
        let (parent_id, parent, child) = parent_and_child();
        let child_id = child.session_id().clone();
        let sessions = HashMap::from([(parent_id.clone(), parent), (child_id, child)]);
        let frozen = HashMap::new();

        // When computing the tree's statistics from the parent.
        let stats = tree_stats(&sessions, &frozen, &parent_id, &HashMap::new());

        // Then every figure is the two members' sum.
        assert_eq!(stats.session_count, 2);
        // And the sent tokens are 1,000 + 200.
        assert_eq!(stats.effective_sent, 1_200);
        // And the received tokens are 600 + 250.
        assert_eq!(stats.total_received, 850);
        // And the cost is 0.01 + 0.005.
        assert!((stats.total_cost - 0.015).abs() < 1e-9);
    }

    #[rstest::rstest]
    #[test]
    fn tree_cache_hit_percent_is_the_tree_ratio() {
        // Given a tree with 1,152 cached of 1,200 measured prompt tokens.
        let (parent_id, parent, child) = parent_and_child();
        let child_id = child.session_id().clone();
        let sessions = HashMap::from([(parent_id.clone(), parent), (child_id, child)]);
        let frozen = HashMap::new();

        // When computing the tree's statistics.
        let stats = tree_stats(&sessions, &frozen, &parent_id, &HashMap::new());

        // Then the percentage is the whole tree's ratio, rounded.
        assert_eq!(stats.cache_hit_percent, Some(96));
    }

    #[rstest::rstest]
    #[test]
    fn tree_working_time_counts_a_shared_second_once() {
        // Given a parent and a child that both worked the same ten seconds.
        let (parent_id, parent, child) = parent_and_child();
        let child_id = child.session_id().clone();
        let sessions = HashMap::from([(parent_id.clone(), parent), (child_id.clone(), child)]);
        let frozen = HashMap::new();

        // When computing the tree's statistics.
        let stats = tree_stats(
            &sessions,
            &frozen,
            &parent_id,
            &overlapping_intervals(&parent_id, &child_id),
        );

        // Then the tree worked ten seconds, not twenty.
        assert_eq!(stats.working_seconds, 10);
    }

    #[rstest::rstest]
    #[test]
    fn tree_is_hidden_for_a_lone_session() {
        // Given a tree of one session.
        let tree = TreeStats {
            session_count: 1,
            ..TreeStats::default()
        };

        // When asking whether the bar shows it.
        let shown = tree_is_shown(&tree);

        // Then it does not.
        assert!(!shown);
    }

    #[rstest::rstest]
    #[test]
    fn tree_is_shown_for_a_parent_with_a_child() {
        // Given a tree of two sessions.
        let tree = TreeStats {
            session_count: 2,
            ..TreeStats::default()
        };

        // When asking whether the bar shows it.
        let shown = tree_is_shown(&tree);

        // Then it does.
        assert!(shown);
    }
}
