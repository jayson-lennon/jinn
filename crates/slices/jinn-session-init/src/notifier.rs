//! The discovery notifier — turns the settled discovery event into a
//! visible transient chat-history entry.
//!
//! Subscribes to [`SessionDiscoverySettled`] on its slice-internal
//! topic (the worker publishes there; no reverse relay — this is the
//! event's only consumer) and writes a markdown summary as a
//! `Transient` chat entry directly via [`State`] + [`SessionCap`], the
//! discord slice's write-path precedent. One entry per settled event —
//! it fires only on the coalesced signal, never per scan.

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_domain::common::state::State;
use jinn_domain::common::tcaps::session::SessionCap;
use jinn_domain::protocol::ChatEntry;

use crate::contracts::SessionDiscoverySettled;

/// Posts a transient chat entry summarising a session's settled
/// discovery.
pub struct DiscoveryNotifier {
    /// Shared application state.
    state: State,
    /// Authority to push the summary entry into the session.
    session_cap: SessionCap,
}

impl ServiceActor for DiscoveryNotifier {
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // The state handle and cap cannot ride JSON args; spawn
        // injects them via `start_with` (see `spawn`).
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec).attach(
                "DiscoveryNotifier is spawned via start_with; start requires state + cap",
            ),
        )
    }
}

impl DiscoveryNotifier {
    /// Spawns the notifier at its static path and subscribes it to the
    /// settled topic.
    ///
    /// # Panics
    ///
    /// Panics if the topic subscription fails, which can only happen on
    /// a broken actor system.
    pub fn spawn(system: &ActorSystem, state: State) -> ActorPath {
        let path = trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(crate::NOTIFIER_PATH))
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start_with({
                move || {
                    let state = state.clone();
                    Box::pin(async move {
                        Ok(Self {
                            state,
                            session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                        })
                    })
                }
            })
            .handles::<SessionDiscoverySettled>()
            .start();

        #[expect(
            clippy::expect_used,
            reason = "subscription failure is a broken actor system, not a caller bug"
        )]
        system
            .subscribe(&path, &crate::settled_topic(), None)
            .expect("discovery notifier subscribes to the settled topic");
        path
    }

    /// Writes the summary entry into the session, dropping silently if
    /// the session is gone (closed since the run started).
    fn push_summary(&self, session_id: &SessionId, summary: String) {
        self.state.with_session(&self.session_cap, |view| {
            if let Some(session) = view.session.map().get_mut(session_id) {
                session.push_entry(ChatEntry::transient(summary));
            } else {
                tracing::debug!(
                    %session_id,
                    "discovery summary arrived for a session that no longer exists; dropping",
                );
            }
        });
    }
}

impl MsgHandler<SessionDiscoverySettled> for DiscoveryNotifier {
    async fn handle(&mut self, msg: SessionDiscoverySettled, _ctx: &mut MsgCtx<'_>) {
        let summary = build_summary(&msg);
        self.push_summary(&msg.session_id, summary);
    }
}

/// Renders the markdown summary of a settled discovery snapshot — the
/// kameo notifier's `build_summary`, verbatim.
fn build_summary(event: &SessionDiscoverySettled) -> String {
    use std::fmt::Write as _;

    let snapshot = &event.snapshot;
    let mut out = String::new();
    let total = snapshot.skill_count + snapshot.prompt_count + snapshot.context_file_count;

    if total == 0 {
        out.push_str("No project resources found (no skills, prompts, or AGENTS.md).");
    } else {
        out.push_str("**Project resources discovered**\n");
        if snapshot.skill_count > 0 {
            let _ = writeln!(out, "- {} skill(s)", snapshot.skill_count);
        }
        if snapshot.prompt_count > 0 {
            let _ = writeln!(out, "- {} prompt(s)", snapshot.prompt_count);
        }
        if snapshot.context_file_count > 0 {
            let _ = writeln!(
                out,
                "- {} AGENTS.md / context file(s)",
                snapshot.context_file_count
            );
        }
    }

    let mut notes: Vec<String> = Vec::new();
    if let Some(reason) = &event.delayed {
        notes.push(reason.clone());
    }
    if let Some(err) = &snapshot.skill_error {
        notes.push(format!("skills scan error: {err}"));
    }
    if let Some(err) = &snapshot.prompt_error {
        notes.push(format!("prompts scan error: {err}"));
    }
    if let Some(err) = &snapshot.context_error {
        notes.push(format!("context-files scan error: {err}"));
    }

    if !notes.is_empty() {
        out.push('\n');
        for note in &notes {
            out.push_str(note.trim_end());
            out.push('\n');
        }
    }

    out
}
