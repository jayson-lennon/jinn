//! Reasoning effort — how hard a reasoning-capable model thinks before answering.
//!
//! This module re-exports the [`ReasoningEffort`] type
//! (defined in `jinn-provider`, the lowest crate in the dependency chain) and
//! provides [`resolve_effort`], which surfaces a session's own effort.
//!
//! Effort is **session-owned**, mirroring model and persona selection: the last-used
//! `reasoning_effort` from `state.toml` is consulted **only at session creation** to
//! seed each session's `profile.reasoning_effort`. After that, the session's own value
//! is the sole source of truth at request and render time — it is never re-resolved
//! against the live global (which would leak one session's picker choice into every
//! other override-free session).
//!
//! The types live in `jinn-provider` so the OpenAI-compatible request builder
//! can emit them without `jinn-domain` reaching down into provider internals.

pub use jinn_provider::ReasoningEffort;

pub mod picker_entry;

pub use picker_entry::ReasoningEffortEntry;

/// Returns the session's own reasoning effort.
///
/// The effort is seeded from the global default at session creation and then
/// owned by the session. When the session's effort is `None`, `None` is
/// returned — meaning "send no effort field and let the provider decide"
/// (OpenRouter still requests reasoning tokens via `{ "enabled": true }`).
///
/// See the module docs for why the global is not consulted here.
#[must_use]
pub fn resolve_effort(session_effort: Option<ReasoningEffort>) -> Option<ReasoningEffort> {
    session_effort
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

    #[rstest::rstest]
    #[case(ReasoningEffort::Max, Some(ReasoningEffort::Max))]
    #[case(ReasoningEffort::Xhigh, Some(ReasoningEffort::Xhigh))]
    #[case(ReasoningEffort::High, Some(ReasoningEffort::High))]
    #[case(ReasoningEffort::Medium, Some(ReasoningEffort::Medium))]
    #[case(ReasoningEffort::Low, Some(ReasoningEffort::Low))]
    #[case(ReasoningEffort::Minimal, Some(ReasoningEffort::Minimal))]
    #[case(ReasoningEffort::None, Some(ReasoningEffort::None))]
    fn returns_the_sessions_own_effort(
        #[case] effort: ReasoningEffort,
        #[case] expected: Option<ReasoningEffort>,
    ) {
        // Given a session with its own effort.
        // When resolving.
        // Then the session's own effort is returned unchanged.
        assert_eq!(resolve_effort(Some(effort)), expected);
    }

    #[rstest::rstest]
    #[test]
    fn returns_none_when_session_has_no_effort() {
        // Given a session with no effort set.
        // When resolving.
        // Then None is returned (send no effort field; let the provider decide).
        assert_eq!(resolve_effort(None), None);
    }
}
