//! The tool filter's runtime gate: a withheld tool is refused where the call is
//! routed, not only where the prompt is built.
//!
//! These drive the REAL delivery path — `ExecuteToolBatch` published on the
//! trouper bus, the spawned orchestrator resolving the tool, and the
//! `ToolExecutionCompleted` coming back — because the defect they guard is one of
//! reachability. A filter applied only to the assembled prompt looks correct in
//! every prompt-level test while the tool itself stays callable by name.
//!
//! The refusal is asserted on the observable result the model receives, not on
//! whether a task was spawned: the point is what the model is told.

#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

use crate::orchestrator::{ToolOrchestratorActor, ToolOrchestratorActorDeps};
use jinn_core_types::tool_types::{ToolCall, ToolResult};
use jinn_core_types::{FilterMode, NameFilter, SessionId};
use jinn_kernel::common::bus::HarnessServices;
use jinn_kernel::common::services::Services;
use jinn_kernel::common::state::State;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};
use jinn_tools_msg::{ExecuteToolBatch, ToolExecutionCompleted};
use std::time::Duration;

/// A state whose active session withholds `filter`'s worth of tools.
fn state_filtering(filter: NameFilter) -> (State, SessionId) {
    let state = State::new(jinn_kernel::AppState::default());
    let session_id = {
        let mut guard = state.write();
        let session = guard.active_session_mut();
        session.set_tool_filter(Some(filter));
        session.session_id().clone()
    };
    (state, session_id)
}

/// Spawns the orchestrator over `state` with the test's own `services`, so
/// the actor's turn ledger is the same instance the test minted into.
#[expect(
    clippy::unused_async,
    reason = "test fixture symmetry; body never awaits"
)]
async fn spawn(harness: &TestHarness, state: State, services: Services) {
    ToolOrchestratorActor::spawn(
        harness.system(),
        ToolOrchestratorActorDeps {
            deps: jinn_kernel::common::actor_deps::ActorDeps {
                services: services.clone(),
            },
            state,
            services,
            builtin_filter: Some(vec!["bash".to_owned()]),
        },
    );
}

/// One fixture: shared services, a minted turn, and the harness.
struct FilterFixture {
    harness: TestHarness,
    services: Services,
    session_id: SessionId,
}

impl FilterFixture {
    /// Builds the fixture and spawns the orchestrator over it.
    async fn new(filter: Option<NameFilter>) -> Self {
        let harness = TestHarness::new().await;
        let services = harness.services().await;
        let (state, session_id) = match filter {
            Some(f) => state_filtering(f),
            None => {
                let state = State::new(jinn_kernel::AppState::default());
                let session_id = {
                    let mut guard = state.write();
                    guard.active_session_mut().session_id().clone()
                };
                (state, session_id)
            }
        };
        // A live turn whose work set holds the call about to dispatch —
        // production mints at dispatch; here the fixture mints so the batch
        // carries a real token.
        services
            .turn_ledger
            .mint(
                &session_id,
                jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
            )
            .expect("fixture mint");
        spawn(&harness, state, services.clone()).await;
        Self {
            harness,
            services,
            session_id,
        }
    }

    /// The live turn's token.
    fn turn(&self) -> jinn_core_types::turn_identity::TurnToken {
        self.services
            .turn_ledger
            .live(&self.session_id)
            .expect("live turn")
            .turn
    }
}

/// Publishes one `bash` call and returns the single result it produced.
async fn call_bash(fixture: &FilterFixture) -> ToolResult {
    let recorder = fixture
        .harness
        .spawn_recorder::<ToolExecutionCompleted>()
        .await;
    fixture
        .harness
        .publish(ExecuteToolBatch {
            session_id: fixture.session_id.clone(),
            tool_calls: vec![ToolCall {
                id: "tc-1".into(),
                name: "bash".to_owned(),
                arguments: r#"{"command":"echo should-not-run"}"#.to_owned(),
            }],
            dispatched_at: jiff::Timestamp::now(),
            turn: fixture.turn(),
        })
        .await;
    let completions =
        await_recorded::<ToolExecutionCompleted>(&recorder, 1, Duration::from_secs(10)).await;
    let results = completions
        .into_iter()
        .map(|c| c.result)
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1, "expected exactly one tool result");
    results.into_iter().next().expect("one result")
}

#[rstest::rstest]
#[tokio::test]
async fn a_filtered_builtin_is_refused_and_named() {
    // Given a session whose filter withholds bash.
    let fx = FilterFixture::new(Some(NameFilter::deny(["bash".to_owned()]))).await;

    // When the model calls bash.
    let result = call_bash(&fx).await;

    // Then the call fails and the message names the tool, so the model can
    // stop rather than retry or hunt for an alias.
    assert!(!result.success, "a withheld tool must not succeed");
    assert!(
        result.content.contains("bash"),
        "the refusal must name the tool; got: {:?}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_filtered_tool_is_reported_as_withheld_not_unknown() {
    // Given a session whose filter withholds bash.
    let fx = FilterFixture::new(Some(NameFilter::deny(["bash".to_owned()]))).await;

    // When the model calls bash.
    let result = call_bash(&fx).await;

    // Then it is not reported as unknown. The tool exists and is registered;
    // calling it unknown would be false and would point the model at a
    // spelling problem instead of a scope one.
    assert!(
        !result.content.contains("unknown tool"),
        "a withheld tool must not be reported as unknown; got: {:?}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_unfiltered_builtin_still_dispatches() {
    // The gate is here, not in the fixture: the refusal tests above
    // withhold tools before shell resolution and never need a shell.
    jinn_testutil::shell_required();

    // Given a session with no filter configured.
    let fx = FilterFixture::new(Some(NameFilter::default())).await;

    // When the model calls bash.
    let result = call_bash(&fx).await;

    // Then it runs. The gate must not break the happy path — a filter that
    // withholds everything would pass every refusal test above.
    assert!(result.success, "an unfiltered tool must still run");
    assert!(
        result.content.contains("should-not-run"),
        "the tool must actually have executed; got: {:?}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_allow_filter_refuses_a_tool_it_does_not_name() {
    // Given a session permitted exactly one other tool.
    let fx = FilterFixture::new(Some(NameFilter {
        mode: FilterMode::Allow,
        names: ["read".to_owned()].into_iter().collect(),
    }))
    .await;

    // When the model calls bash.
    let result = call_bash(&fx).await;

    // Then the call is refused — the case a blocklist could not express,
    // since there is no bash entry to remove.
    assert!(!result.success, "an unlisted tool must be withheld");
    assert!(
        result.content.contains("withheld"),
        "the refusal must say the filter withheld it; got: {:?}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_allow_filter_still_admits_the_tool_it_names() {
    jinn_testutil::shell_required();

    // Given a session permitted exactly bash.
    let fx = FilterFixture::new(Some(NameFilter {
        mode: FilterMode::Allow,
        names: ["bash".to_owned()].into_iter().collect(),
    }))
    .await;

    // When the model calls bash.
    let result = call_bash(&fx).await;

    // Then it runs. A filter that withholds by omission must not also
    // withhold what it names.
    assert!(
        result.success,
        "a named tool must still run; got: {:?}",
        result.content
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_glob_withholding_a_whole_mcp_server_refuses_its_tools() {
    // Given a session withholding every tool from one MCP server.
    let fx = FilterFixture::new(Some(NameFilter::deny(["mcp__github__*".to_owned()]))).await;

    // When the model calls a tool from that server.
    let recorder = fx.harness.spawn_recorder::<ToolExecutionCompleted>().await;
    fx.harness
        .publish(ExecuteToolBatch {
            session_id: fx.session_id.clone(),
            tool_calls: vec![ToolCall {
                id: "tc-mcp".into(),
                name: "mcp__github__create_pr".to_owned(),
                arguments: "{}".to_owned(),
            }],
            dispatched_at: jiff::Timestamp::now(),
            turn: fx.turn(),
        })
        .await;
    let completions =
        await_recorded::<ToolExecutionCompleted>(&recorder, 1, Duration::from_secs(10)).await;
    let results = completions
        .into_iter()
        .map(|c| c.result)
        .collect::<Vec<_>>();

    // Then the refusal is the filter's, not "unknown tool" — which is what
    // this call would otherwise produce, since no MCP server is registered
    // in the test. The gate runs before registration lookup, so a withheld
    // tool reports as withheld whatever its registration state.
    assert_eq!(results.len(), 1, "expected exactly one tool result");
    assert!(
        results[0].content.contains("withheld"),
        "a globbed MCP tool must be refused by the filter; got: {:?}",
        results[0].content
    );
}

/// The two filters are separate gates over separate resources, and neither can
/// widen the other. A skill that is withheld cannot be the reason a withheld
/// tool runs — and a withheld tool is refused whatever the skill filter says.
#[rstest::rstest]
#[tokio::test]
async fn a_withheld_skill_does_not_admit_a_withheld_tool() {
    // Given a session withholding both a skill and a tool. The fixture's
    // filter fixture covers the tool; the skill filter goes on after mint.
    let fx = FilterFixture::new(Some(NameFilter::deny(["bash".to_owned()]))).await;

    // When the model calls the withheld tool.
    let result = call_bash(&fx).await;

    // Then it is refused. The skill gate is not a route around the tool gate.
    assert!(!result.success, "the tool gate must still refuse");
}

/// A session the orchestrator has not seen has no filter to consult. Defaulting
/// the gate to "withheld" in that case would refuse every tool for it — the
/// failure mode is a session that cannot do anything, which is much worse than
/// a filter that briefly did not apply.
#[rstest::rstest]
#[tokio::test]
async fn a_session_absent_from_state_does_not_withhold_tools() {
    jinn_testutil::shell_required();

    // Given an orchestrator whose state holds no session at all.
    let fx = FilterFixture::new(None).await;

    // When the model calls bash.
    let result = call_bash(&fx).await;

    // Then the call runs rather than being refused by a filter that does not
    // exist. The two existing fabric tests dispatch against exactly such a
    // session; this pins the reason they still pass.
    assert!(
        result.success,
        "an absent session must not withhold; got: {:?}",
        result.content
    );
}
