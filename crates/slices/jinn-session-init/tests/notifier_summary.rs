//! The discovery notifier: a settled event produces one transient
//! summary entry matching the recorded `build_summary` shape.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code"
)]

use std::time::Duration;

use jinn_domain::common::app_state::AppState;
use jinn_domain::common::state::State;
use jinn_session_init::contracts::{DiscoverySnapshot, SessionDiscoverySettled};
use trouper::topics::Topic;

/// Polls `check` until it passes or the retry budget runs out.
async fn wait_for(check: impl Fn() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never held within the retry budget");
}

/// A notifier on a fresh fabric, with one real session in state.
struct Wired {
    fabric: jinn_testutil::TestFabric,
    state: State,
    session_id: jinn_domain::protocol::SessionId,
}

impl Wired {
    async fn wire() -> Self {
        let state = State::new(AppState::default());
        let session_id = state.read().session.active_session_id().clone();
        let fabric = jinn_testutil::TestFabric::new();
        jinn_session_init::notifier::DiscoveryNotifier::spawn(fabric.system(), state.clone());
        Self {
            fabric,
            state,
            session_id,
        }
    }

    /// The session's transient history texts.
    fn transient_texts(&self) -> Vec<String> {
        let guard = self.state.read();
        guard
            .session
            .get(&self.session_id)
            .map(|s| {
                s.history()
                    .iter()
                    .filter_map(|e| match &e.kind {
                        jinn_domain::protocol::ChatEntryKind::Transient(text) => {
                            Some(text.to_string())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[tokio::test]
async fn settled_event_posts_one_transient_summary_entry() {
    // Given a notifier wired on the fabric.
    let wired = Wired::wire().await;

    // When a settled event with discovered resources crosses.
    wired
        .fabric
        .send_to_topic(
            &SessionDiscoverySettled {
                session_id: wired.session_id.clone(),
                snapshot: DiscoverySnapshot {
                    skill_count: 2,
                    prompt_count: 1,
                    context_file_count: 1,
                    ..Default::default()
                },
                delayed: None,
            },
            &Topic::new("SessionDiscoverySettled"),
        )
        .await;

    // Then exactly one transient entry lands, with the counts listed.
    wait_for(|| wired.transient_texts().len() == 1).await;
    let text = &wired.transient_texts()[0];
    assert!(text.contains("**Project resources discovered**"));
    assert!(text.contains("- 2 skill(s)"));
    assert!(text.contains("- 1 prompt(s)"));
    assert!(text.contains("- 1 AGENTS.md / context file(s)"));
}

#[tokio::test]
async fn empty_discovery_says_no_resources() {
    // Given a notifier wired on the fabric.
    let wired = Wired::wire().await;

    // When a settled event with an empty snapshot crosses.
    wired
        .fabric
        .send_to_topic(
            &SessionDiscoverySettled {
                session_id: wired.session_id.clone(),
                snapshot: DiscoverySnapshot::default(),
                delayed: None,
            },
            &Topic::new("SessionDiscoverySettled"),
        )
        .await;

    // Then the message says no project resources found.
    wait_for(|| wired.transient_texts().len() == 1).await;
    assert!(wired.transient_texts()[0].contains("No project resources found"));
}

#[tokio::test]
async fn delayed_reason_surfaces_in_summary() {
    // Given a notifier wired on the fabric.
    let wired = Wired::wire().await;

    // When a settled event carries a delayed reason.
    wired
        .fabric
        .send_to_topic(
            &SessionDiscoverySettled {
                session_id: wired.session_id.clone(),
                snapshot: DiscoverySnapshot {
                    skill_count: 2,
                    ..Default::default()
                },
                delayed: Some("discovery delayed by context".to_owned()),
            },
            &Topic::new("SessionDiscoverySettled"),
        )
        .await;

    // Then the reason surfaces in the message.
    wait_for(|| wired.transient_texts().len() == 1).await;
    assert!(wired.transient_texts()[0].contains("discovery delayed by context"));
}

#[tokio::test]
async fn failed_scan_notes_error_in_summary() {
    // Given a notifier wired on the fabric.
    let wired = Wired::wire().await;

    // When a settled event carries a skills scan error.
    wired
        .fabric
        .send_to_topic(
            &SessionDiscoverySettled {
                session_id: wired.session_id.clone(),
                snapshot: DiscoverySnapshot {
                    skill_count: 0,
                    skill_error: Some("permission denied".to_owned()),
                    ..Default::default()
                },
                delayed: None,
            },
            &Topic::new("SessionDiscoverySettled"),
        )
        .await;

    // Then the failure is noted in the message.
    wait_for(|| wired.transient_texts().len() == 1).await;
    assert!(wired.transient_texts()[0].contains("skills scan error: permission denied"));
}
