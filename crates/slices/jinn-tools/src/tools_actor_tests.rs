//! End-to-end fabric verification: orchestrator completes a real builtin tool
//! batch and the session actor continues the tool loop.
//!
//! The "MCP tools never returning" bug class was caused by tool-completion
//! messages lost on the pre-trouper fabric. These tests drive the REAL
//! delivery path — `ExecuteToolBatch` published on the trouper bus, the
//! spawned orchestrator executing a builtin, `ToolExecutionCompleted`
//! broadcast back, and the spawned session actor emitting `SendToLlmProvider`
//! — so a fabric regression cannot silently reintroduce it.

use crate::orchestrator::{ToolOrchestratorActor, ToolOrchestratorActorDeps};
use jinn_core_types::tool_types::ToolCall;
use jinn_inference_msg::SendToLlmProvider;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_kernel::common::state::State;
use jinn_testutil::bus_harness::{TestHarness, await_recorded};
use jinn_tools_msg::{ExecuteToolBatch, ToolExecutionCompleted};
use std::time::Duration;

/// A state whose active session holds one `bash` tool call, phase Sending.
///
/// The seeded history is what lets the session actor continue the tool loop
/// once the batch completes.
fn bash_batch_state() -> (State, jinn_core_types::SessionId) {
    let state = State::new(jinn_kernel::AppState::default());
    {
        let mut s = state.write();
        let session = s.active_session_mut();
        session.push_entry(jinn_core_types::ChatEntry::user("list files"));
        session.push_entry(jinn_core_types::ChatEntry::assistant("checking"));
        session.push_entry(jinn_core_types::ChatEntry::tool_call(
            "tc-bash-2",
            "bash",
            r#"{"command":"echo loop-continues"}"#,
        ));
    }
    let session_id = state.read().session.active_session_id().clone();
    (state, session_id)
}

/// Spawns the session-turn actor that turns a completed batch into the next
/// provider call.
/// Spawns the session actor over the test's own `services` — its ledger must
/// be the instance the orchestrator and the fixture see.
async fn spawn_session_actor_with(
    harness: &TestHarness,
    state: State,
    services: jinn_kernel::common::services::Services,
) {
    jinn_session_turn::activate(
        harness.system(),
        jinn_session_turn::session_actor::SessionPersistenceActorDeps {
            deps: {
                let _ = jinn_context_assembly::service::ensure_spawned(&services.trouper_system);
                jinn_kernel::common::actor_deps::ActorDeps {
                    services: services.clone(),
                }
            },
            state,
            counter: jinn_llm_support::token_estimator::TiktokenCounter::o200k_base(),
            token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::default(),
            image_converter: jinn_llm_support::image_convert::ImageConverterService::unavailable(),
            handoff_prompt: String::new(),
        },
    );
}

/// A stub provider actor that answers `ExecuteTool` with a completed result,
/// like an MCP server would.
struct StubProvider {
    bus: jinn_kernel::common::services::bus_service::BusService,
    session_id: jinn_core_types::SessionId,
}

impl BusPublish for StubProvider {
    fn bus(&self) -> &jinn_kernel::common::services::bus_service::BusService {
        &self.bus
    }
}

impl trouper::actor::ServiceActor for StubProvider {
    async fn init(
        _args: &trouper::json::Json,
    ) -> Result<Self, error_stack::Report<trouper::registry::RegistryError>> {
        Err(
            error_stack::Report::new(trouper::registry::RegistryError::InvalidSpec)
                .attach("StubProvider is spawned via init_with"),
        )
    }
}

impl trouper::actor::MsgHandler<jinn_tools_msg::ExecuteTool> for StubProvider {
    async fn handle(
        &mut self,
        msg: &jinn_tools_msg::ExecuteTool,
        _ctx: &mut trouper::context::MsgCtx<'_>,
    ) {
        use jinn_tools_msg::ToolExecutionCompleted;

        self.publish(ToolExecutionCompleted {
            turn: msg.turn,
            synthesized: false,
            session_id: self.session_id.clone(),
            result: jinn_core_types::tool_types::ToolResult {
                tool_call_id: msg.tool_call.id.clone(),
                name: msg.tool_call.name.clone(),
                content: "mcp-stub-answer".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
                attachments: Vec::new(),
            },
        })
        .await;
    }
}

/// Spawns the stub provider at `test.stub-mcp-provider`.
fn spawn_stub_provider(harness: &TestHarness, session_id: jinn_core_types::SessionId) {
    let _ = trouper::builder::spawn_service_builder::<StubProvider>(harness.system())
        .at(trouper::actor::ActorPath::new("test.stub-mcp-provider"))
        .init_with({
            let bus = harness.bus();
            move || {
                let bus = bus.clone();
                let session_id = session_id.clone();
                Box::pin(async move { Ok(StubProvider { bus, session_id }) })
            }
        })
        .handles::<jinn_tools_msg::ExecuteTool>()
        .start();
}

/// Registers `mcp__stub__echo` as a session-scoped actor tool for `session_id`.
async fn register_stub_tool(harness: &TestHarness, session_id: &jinn_core_types::SessionId) {
    harness
        .publish(jinn_tools_msg::RegisterTools {
            provider: "mcp__stub__".to_owned(),
            definitions: vec![jinn_core_types::tool_types::ToolDefinition {
                name: "mcp__stub__echo".to_owned(),
                description: "echo".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                server_tool_type: None,
            }],
            session_id: Some(session_id.clone()),
        })
        .await;
}

/// The orchestrator executes a real builtin (`bash`) dispatched over the bus
/// and emits `ToolExecutionCompleted` with the tool's output.
#[rstest::rstest]
#[tokio::test]
async fn orchestrator_completes_builtin_bash_batch_dispatched_over_the_bus() {
    // The bash builtin runs a real shell command, so the suite's shell
    // requirement is asserted here: the failure names the missing
    // dependency instead of surfacing as a tool-result assertion.
    jinn_testutil::shell_required();
    // Given a spawned orchestrator with the bash builtin registered, over
    // the same services the fixture mints into.
    use jinn_kernel::common::bus::HarnessServices;
    let harness = TestHarness::new().await;
    let services = harness.services().await;
    let recorder = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    ToolOrchestratorActor::spawn(
        harness.system(),
        ToolOrchestratorActorDeps {
            deps: jinn_kernel::common::actor_deps::ActorDeps {
                services: services.clone(),
            },
            state: State::new(jinn_kernel::AppState::default()),
            services: services.clone(),
            builtin_filter: Some(vec!["bash".to_owned()]),
        },
    );

    let session_id = jinn_core_types::SessionId::new();

    // When an ExecuteToolBatch carrying one bash call is published on the bus.
    services
        .turn_ledger
        .mint(
            &session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
        )
        .expect("fixture mint");
    harness
        .publish(ExecuteToolBatch {
            session_id: session_id.clone(),
            tool_calls: vec![ToolCall {
                id: "tc-bash-1".into(),
                name: "bash".into(),
                arguments: r#"{"command":"echo fabric-loop-ok"}"#.to_owned(),
            }],
            dispatched_at: jiff::Timestamp::now(),
            turn: services.turn_ledger.live(&session_id).expect("live").turn,
        })
        .await;
    let completions =
        await_recorded::<ToolExecutionCompleted>(&recorder, 1, Duration::from_secs(10)).await;

    // Then exactly one completion landed, for this session.
    let completions: Vec<_> = completions
        .into_iter()
        .filter(|c| c.session_id == session_id)
        .collect();
    assert_eq!(
        completions.len(),
        1,
        "expected exactly one ToolExecutionCompleted for the session"
    );

    // And it carries the bash tool's successful result.
    let result = &completions[0].result;
    assert_eq!(result.name, "bash");
    assert!(result.success, "bash tool call should succeed");
    assert!(
        result.content.contains("fabric-loop-ok"),
        "result should carry the command's stdout, got: {:?}",
        result.content
    );
}

/// The full tool loop closes over the fabric: the completion closing the
/// ledger's open call empties the work set, and the spawned session actor
/// emits `SendToLlmProvider` to continue the conversation.
#[rstest::rstest]
#[tokio::test]
async fn completion_over_the_bus_continues_the_tool_loop() {
    // Given a spawned orchestrator (bash) and a spawned session actor with a
    // tool-call entry in its history, phase Sending.
    use jinn_kernel::common::bus::HarnessServices;
    let harness = TestHarness::new().await;
    let services = harness.services().await;
    let loop_recorder = harness.spawn_recorder::<SendToLlmProvider>().await;
    let (state, session_id) = bash_batch_state();

    ToolOrchestratorActor::spawn(
        harness.system(),
        ToolOrchestratorActorDeps {
            deps: jinn_kernel::common::actor_deps::ActorDeps {
                services: services.clone(),
            },
            state: state.clone(),
            services: services.clone(),
            builtin_filter: Some(vec!["bash".to_owned()]),
        },
    );
    spawn_session_actor_with(&harness, state.clone(), services.clone()).await;

    // The live turn and the mid-tool-loop shape. Production mints at
    // dispatch, and the `StreamCompleted(ToolUse)` ack retires the stream
    // while registering the model's calls — the batch can trail the ack,
    // and the work set must not read empty before the results land.
    {
        let grant = services
            .turn_ledger
            .mint(
                &session_id,
                jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
            )
            .expect("fixture mint must be admitted");
        services
            .turn_ledger
            .ack_stream_with_calls(
                &session_id,
                &grant.turn,
                &grant.stream,
                jinn_kernel::common::turn_ledger::Next::Continue,
                vec![jinn_core_types::ToolCallId::from("tc-bash-2")],
            )
            .expect("the tool-use edge retires the live stream");
    }

    // When the batch is dispatched over the bus with the live turn's token
    // (the orchestrator registers the call in the ledger and executes the
    // builtin; the completion closes the call).
    {
        let live = services.turn_ledger.live(&session_id).expect("live turn");
        harness
            .publish(ExecuteToolBatch {
                session_id: session_id.clone(),
                tool_calls: vec![ToolCall {
                    id: "tc-bash-2".into(),
                    name: "bash".into(),
                    arguments: r#"{"command":"echo loop-continues"}"#.to_owned(),
                }],
                dispatched_at: jiff::Timestamp::now(),
                turn: live.turn,
            })
            .await;
    }
    let sent =
        await_recorded::<SendToLlmProvider>(&loop_recorder, 1, Duration::from_secs(10)).await;

    // Then the session actor continued the tool loop.
    assert!(
        sent.iter().any(|m| m.session_id == session_id),
        "expected SendToLlmProvider after the executed batch reached the session actor"
    );
}

/// An MCP-shaped tool (actor tool, session-scoped registration) routes to its
/// provider via `ExecuteTool`; the provider's `ToolExecutionCompleted`
/// completes the batch. The registration/routing half of the "MCP tools never
/// returning" fix.
#[rstest::rstest]
#[tokio::test]
async fn registered_session_scoped_actor_tool_completes_its_batch() {
    // Given a stub provider actor that answers ExecuteTool with a completed
    // result (like an MCP server would), and a spawned orchestrator over the
    // same services, so its ledger is the one the fixture mints into.
    use jinn_kernel::common::bus::HarnessServices;
    let harness = TestHarness::new().await;
    let services = harness.services().await;
    let batch_recorder = harness.spawn_recorder::<ToolExecutionCompleted>().await;
    let session_id = jinn_core_types::SessionId::new();
    spawn_stub_provider(&harness, session_id.clone());

    ToolOrchestratorActor::spawn(
        harness.system(),
        ToolOrchestratorActorDeps {
            deps: jinn_kernel::common::actor_deps::ActorDeps {
                services: services.clone(),
            },
            state: State::new(jinn_kernel::AppState::default()),
            services: services.clone(),
            builtin_filter: Some(vec![]),
        },
    );

    // When the provider registers an actor tool and a batch referencing it is
    // dispatched under a minted turn.
    register_stub_tool(&harness, &session_id).await;
    services
        .turn_ledger
        .mint(
            &session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
        )
        .expect("fixture mint");
    harness
        .publish(ExecuteToolBatch {
            session_id: session_id.clone(),
            tool_calls: vec![ToolCall {
                id: "tc-mcp-1".into(),
                name: "mcp__stub__echo".into(),
                arguments: r#"{"x":1}"#.to_owned(),
            }],
            dispatched_at: jiff::Timestamp::now(),
            turn: services.turn_ledger.live(&session_id).expect("live").turn,
        })
        .await;
    let completions =
        await_recorded::<ToolExecutionCompleted>(&batch_recorder, 1, Duration::from_secs(10)).await;

    // Then the completion arrived with the provider's answer.
    let completions: Vec<_> = completions
        .into_iter()
        .filter(|c| c.session_id == session_id)
        .collect();
    assert_eq!(
        completions.len(),
        1,
        "expected the MCP-shaped call to complete"
    );
    let result = &completions[0].result;
    assert_eq!(result.name, "mcp__stub__echo");
    assert!(result.success);
    assert_eq!(result.content, "mcp-stub-answer");
}
