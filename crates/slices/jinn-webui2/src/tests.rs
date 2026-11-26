#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unreachable,
    reason = "test assertions need expect, indexed access to a recorded audit, and a let-else on a known variant"
)]

//! Behaviour of the inbound dispatch: one test, one observable result.
//!
//! Every test drives a `ClientMsg` through `dispatch` against a handle
//! built over a recording bus, then asserts either the messages published
//! (by name, in order — the terminal's own sequence) or the frames
//! replied to the page.

use std::sync::Arc;

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::{ChatEntry, ChatEntryKind, ModelSelection};
use jinn_inference_msg::CancelTurn;
use jinn_kernel::State;
use jinn_preferences_config::protocol::app_state_command::UpdateAppState;
use jinn_provider_selection_msg::ProviderSwitch;
use jinn_session_msg::MarkSessionInteracted;
use jinn_session_store_msg::{PersistSession, SessionLoadRequested};
use jinn_slices::bus::BusService;
use jinn_webui2_msg::{ClientMsg, ServerMsg};

use crate::{
    actions, connections::ConnectionRegistry, outbound::OutboundHandle, protocol::dispatch,
    revision::RevisionTracker,
};

/// A handle over a recording bus and a fresh state, with nothing spawned.
struct Harness {
    handle: OutboundHandle,
    audit: jinn_slices::bus::BusAudit,
    state: State,
}

fn harness() -> Harness {
    harness_with_providers(empty_providers())
}

/// A harness whose session store keeps what is saved into it, so a test
/// can hold a session that is stored and not in memory.
fn harness_with_store() -> Harness {
    harness_with(empty_providers(), Arc::new(KeepingSessionStore::default()))
}

/// A session store that remembers every snapshot saved to it and answers
/// reads from that set, and nothing else. What the kernel's test services
/// hand out accepts a save and forgets it, which makes "is this session in
/// the store?" unanswerable.
#[derive(Debug, Default)]
struct KeepingSessionStore {
    saved: std::sync::Mutex<Vec<jinn_session_state::SessionSnapshot>>,
}

impl KeepingSessionStore {
    fn snapshots(&self) -> Vec<jinn_session_state::SessionSnapshot> {
        self.saved.lock().expect("store lock").clone()
    }
}

#[async_trait::async_trait]
impl jinn_session_state::SessionStore for KeepingSessionStore {
    fn name(&self) -> &'static str {
        "keeping"
    }

    async fn save(
        &self,
        snapshot: &jinn_session_state::SessionSnapshot,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        let mut saved = self.saved.lock().expect("store lock");
        saved.retain(|held| held.metadata.session_id != snapshot.metadata.session_id);
        saved.push(snapshot.clone());
        Ok(())
    }

    /// No summaries: a stored snapshot's metadata is not a summary, and
    /// no test reads picker rows from this store.
    async fn load_summaries(
        &self,
    ) -> Result<
        Vec<jinn_session_store_msg::SessionSummary>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(Vec::new())
    }

    async fn load_session(
        &self,
        session_id: &jinn_core_types::SessionId,
    ) -> Result<
        Option<jinn_session_state::SessionSnapshot>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(self
            .snapshots()
            .into_iter()
            .find(|snapshot| &snapshot.metadata.session_id == session_id))
    }

    async fn delete(
        &self,
        _session_id: &jinn_core_types::SessionId,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(())
    }

    async fn fork(
        &self,
        _source_session_id: &jinn_core_types::SessionId,
        _at_ordinal: usize,
    ) -> Result<
        jinn_core_types::SessionId,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(jinn_core_types::SessionId::new())
    }

    async fn set_archived(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _archived: bool,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(())
    }

    async fn set_archived_many(
        &self,
        _session_ids: &[jinn_core_types::SessionId],
        _archived: bool,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(())
    }

    async fn set_in_library(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _in_library: bool,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(())
    }

    async fn load_unarchived_summaries(
        &self,
    ) -> Result<
        Vec<jinn_session_store_msg::SessionSummary>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        self.load_summaries().await
    }

    async fn dirty_session_ids(
        &self,
    ) -> Result<
        Vec<jinn_core_types::SessionId>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(Vec::new())
    }

    async fn reindex_session_chunk(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _max_entries: usize,
    ) -> Result<bool, error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(true)
    }

    async fn pending_dirty_count(
        &self,
    ) -> Result<usize, error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(0)
    }

    async fn search(
        &self,
        _params: jinn_session_store_msg::SearchParams,
    ) -> Result<
        jinn_session_store_msg::SearchOutcome,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(jinn_session_store_msg::SearchOutcome {
            total_matches: 0,
            per_session: Vec::new(),
            hits: Vec::new(),
        })
    }

    async fn fetch_window(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _anchor: &jinn_core_types::ChatEntryId,
        _context: usize,
    ) -> Result<
        Option<jinn_session_store_msg::TranscriptWindow>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(None)
    }

    async fn fetch_tail(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _limit: usize,
    ) -> Result<
        Option<jinn_session_store_msg::TranscriptWindow>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(None)
    }
}

/// A registry with nothing in it: the picker has no rows to offer.
fn empty_providers() -> jinn_provider_config::ProvidersConfig {
    jinn_provider_config::ProvidersConfig {
        providers: std::collections::BTreeMap::new(),
        aliases: vec![],
        default_provider: None,
        endpoint_defaults: vec![],
    }
}

/// A harness over a registry seeded with the given providers, so the
/// model picker has rows to filter.
fn harness_with_providers(providers: jinn_provider_config::ProvidersConfig) -> Harness {
    harness_with(
        providers,
        Arc::new(jinn_kernel::common::services::test_services::FakeSessionStore),
    )
}

/// A harness over a registry seeded with the given providers and a
/// session store of the caller's choosing.
fn harness_with(
    providers: jinn_provider_config::ProvidersConfig,
    store: Arc<dyn jinn_session_state::SessionStore>,
) -> Harness {
    harness_seeded(providers, store, Seed::default())
}

/// What a harness pre-loads into the cells the picker rows read.
///
/// Every field is opt-in: a test that does not need personas or a
/// configured MCP server pays nothing for them, and a test that does
/// names exactly what it needs.
#[derive(Debug, Default, Clone)]
struct Seed {
    /// Personas placed in the persona catalog, as `(name, description,
    /// has_handoff_block)`.
    personas: Vec<(&'static str, &'static str, bool)>,
    /// MCP servers placed in the `[mcp]` config section, as `(name,
    /// command)`.
    mcp_servers: Vec<(&'static str, &'static str)>,
    /// Tools placed in the global tool registry, as `(name, description)`.
    tools: Vec<(&'static str, &'static str)>,
}

/// A harness over a registry, a session store, and a pre-seeded set of
/// cells.
fn harness_seeded(
    providers: jinn_provider_config::ProvidersConfig,
    store: Arc<dyn jinn_session_state::SessionStore>,
    seed: Seed,
) -> Harness {
    let (bus, audit) = BusService::new_recording();
    // One `Slices`, registered with the catalog once, is both the registry
    // the cells are resolved from and the one the state carries: the
    // frontend and the session map each attach it, so `tool_registry()` on
    // the state resolves the very cell the seed writes. A bare
    // `State::new(AppState::default())` reads `None`, and every tool row
    // test would pass vacuously with no rows.
    let slices = jinn_slices::Slices::new();
    jinn_cell_catalog::register_all_cells(&slices);
    let app_state = jinn_kernel::AppState::default();
    app_state.frontend.attach_slices(slices.clone());
    app_state.session.attach_slices(slices.clone());
    let state = State::new(app_state);

    let provider_cell = slices
        .reader::<jinn_provider_selection_msg::ProviderCell>(
            &jinn_provider_selection_msg::provider_state_slot(),
        )
        .expect("the catalog registers the provider slot");
    let personas_cell = slices
        .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
        .expect("the catalog registers the persona slot");
    let mcp_runtime_cell = slices
        .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot())
        .expect("the catalog registers the MCP runtime slot");
    let registry_cell = slices
        .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
        .expect("the catalog registers the tool registry slot");

    seed_personas(&personas_cell, &seed.personas);
    seed_tools(&registry_cell, &seed.tools);
    // One services: the config the seed writes is the config the handle
    // reads. A second `TestServices` would hand the handle a different
    // layer and every MCP row would read as unconfigured.
    let services = jinn_kernel::TestServices::builder()
        .providers(providers)
        .build();
    seed_mcp_servers(&services.config, &seed.mcp_servers);

    let handle = OutboundHandle::new(crate::outbound::OutboundActorDeps {
        state: state.clone(),
        bus,
        connections: ConnectionRegistry::default(),
        revisions: RevisionTracker::default(),
        session_store: jinn_session_state::SessionStoreService::new(store),
        config: services.config.clone(),
        services,
        provider_cell,
        personas_cell,
        mcp_runtime_cell,
    });
    Harness {
        handle,
        audit,
        state,
    }
}

/// Seeds the persona catalog with the given personas.
fn seed_personas(
    cell: &jinn_slices::TypedCell<jinn_persona_msg::Personas>,
    entries: &[(&str, &str, bool)],
) {
    if entries.is_empty() {
        return;
    }
    let seeded = personas_entries(entries);
    cell.update(|catalog| catalog.replace_entries(seeded));
}

/// The seeded personas as catalog entries.
fn personas_entries(entries: &[(&str, &str, bool)]) -> Vec<jinn_slices::Persona> {
    entries
        .iter()
        .map(|(name, description, has_block)| jinn_slices::Persona {
            name: (*name).to_owned(),
            description: (*description).to_owned(),
            body: format!("You are {name}."),
            handoff_prompt: if *has_block {
                "<compaction>x</compaction>"
            } else {
                ""
            }
            .to_owned(),
        })
        .collect()
}

/// Seeds the global tool registry with the given tools.
fn seed_tools(cell: &jinn_slices::TypedCell<jinn_tools_msg::ToolRegistry>, tools: &[(&str, &str)]) {
    if tools.is_empty() {
        return;
    }
    cell.update(|registry| {
        registry
            .global
            .extend(tools.iter().map(|(name, description)| {
                (
                    (*name).to_owned(),
                    jinn_core_types::ToolDefinition {
                        name: (*name).to_owned(),
                        description: (*description).to_owned(),
                        parameters: serde_json::json!({ "type": "object" }),
                        prompt_snippet: None,
                        prompt_guidelines: Vec::new(),
                        server_tool_type: None,
                    },
                )
            }));
    });
}

/// Seeds the `[mcp]` config section with the given servers.
fn seed_mcp_servers(config: &jinn_config::ConfigLayer, servers: &[(&str, &str)]) {
    if servers.is_empty() {
        return;
    }
    let configured = jinn_preferences_config::schemas::mcp::McpServersConfig(
        servers
            .iter()
            .map(|(name, command)| {
                (
                    (*name).to_owned(),
                    jinn_preferences_config::schemas::mcp::McpServerConfig {
                        command: Some((*command).to_owned()),
                        ..Default::default()
                    },
                )
            })
            .collect(),
    );
    config
        .put::<jinn_preferences_config::schemas::mcp::McpServersConfig>(&configured)
        .expect("the test config accepts the mcp section");
}

fn active_id(state: &State) -> jinn_core_types::SessionId {
    state.read().session.active_session_id().clone()
}

fn push(state: &State, entry: ChatEntry) {
    state.write().session.active_session_mut().push_entry(entry);
}

// ---------------------------------------------------------------------------
// Send
// ---------------------------------------------------------------------------

/// Sends one message the way the page does, and fails the test when the
/// server refuses it: every caller here is exercising an action that must
/// succeed. The one refusal under test sends its message by hand.
async fn say(h: &Harness, msg: ClientMsg) {
    let reply = dispatch(msg, &h.handle).await;
    assert!(
        !reply
            .frames
            .iter()
            .any(|frame| matches!(frame, ServerMsg::Error(_))),
        "the server refused: {reply:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn send_publishes_mark_interacted_before_enqueue() {
    // Given an idle session.
    let h = harness();

    // When the page sends a message.
    say(
        &h,
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
    )
    .await;

    // Then the session is marked interacted first and the message enqueued second.
    assert_eq!(
        h.audit.names(),
        ["MarkSessionInteracted", "EnqueueUserMessage"]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn send_enqueues_the_text_as_a_user_entry() {
    // Given an idle session.
    let h = harness();

    // When the page sends a message.
    say(
        &h,
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
    )
    .await;

    // Then the enqueued entry is a user entry carrying that text.
    let enqueued = h.audit.of_type::<EnqueueUserMessage>();
    assert_eq!(enqueued.len(), 1);
    assert_eq!(enqueued[0].entry.prompt_text(), Some("hello"));
}

#[rstest::rstest]
#[tokio::test]
async fn send_enqueues_on_the_active_session() {
    // Given an idle session.
    let h = harness();
    let id = active_id(&h.state);

    // When the page sends a message.
    say(
        &h,
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
    )
    .await;

    // Then both messages name the active session.
    let ids = h
        .audit
        .of_type::<EnqueueUserMessage>()
        .iter()
        .map(|m| m.session_id.clone())
        .chain(
            h.audit
                .of_type::<MarkSessionInteracted>()
                .iter()
                .map(|m| m.session_id.clone()),
        )
        .collect::<Vec<_>>();
    assert!(ids.iter().all(|published| *published == id));
}

#[rstest::rstest]
#[tokio::test]
async fn send_while_busy_is_refused_with_an_error() {
    // Given a session running a turn.
    let h = harness();
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page sends a message.
    let reply = dispatch(
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told the session is busy.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
}

#[rstest::rstest]
#[tokio::test]
async fn send_while_busy_publishes_nothing() {
    // Given a session running a turn.
    let h = harness();
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page sends a message — by hand, since this is the one
    // refusal under test and `say` fails on a refused message.
    let reply = dispatch(
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then nothing reaches the bus: the refusal is a reply to the page, not
    // a message to the session.
    assert_eq!(h.audit.names(), Vec::<&str>::new());
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
}

#[rstest::rstest]
#[tokio::test]
async fn send_of_blank_text_is_refused() {
    // Given an idle session.
    let h = harness();

    // When the page sends only whitespace.
    let reply = dispatch(
        ClientMsg::Send {
            text: "   ".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told the message is empty.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn queue_while_busy_still_enqueues() {
    // Given a session running a turn.
    let h = harness();
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page queues a message.
    say(
        &h,
        ClientMsg::Queue {
            text: "next".to_owned(),
        },
    )
    .await;

    // Then the message is enqueued, exactly as an idle send would.
    assert_eq!(
        h.audit.names(),
        ["MarkSessionInteracted", "EnqueueUserMessage"]
    );
}

// ---------------------------------------------------------------------------
// Cancel
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn cancel_publishes_cancel_turn_draining_to_input() {
    // Given a session running a turn.
    let h = harness();

    // When the page cancels.
    say(&h, ClientMsg::Cancel).await;

    // Then the terminal's own cancel is published.
    let cancels = h.audit.of_type::<CancelTurn>();
    assert_eq!(cancels.len(), 1);
    assert!(cancels[0].drain_buffers_to_input);
}

// ---------------------------------------------------------------------------
// Activate
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn activate_switches_the_active_session() {
    // Given two loaded sessions.
    let h = harness();
    let second = jinn_core_types::SessionId::new();
    h.state.write().session.get_or_create(&second);

    // When the page activates the second.
    say(
        &h,
        ClientMsg::ActivateSession {
            session_id: second.clone(),
        },
    )
    .await;

    // Then it is the instance's active session.
    assert_eq!(*h.state.read().session.active_session_id(), second);
}

#[rstest::rstest]
#[tokio::test]
async fn activate_replies_with_a_snapshot_of_the_activated_session() {
    // Given two loaded sessions.
    let h = harness();
    let second = jinn_core_types::SessionId::new();
    h.state.write().session.get_or_create(&second);

    // When the page activates the second.
    let reply = dispatch(
        ClientMsg::ActivateSession {
            session_id: second.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told it now views that session.
    let [ServerMsg::Snapshot(snapshot)] = reply.frames.as_slice() else {
        unreachable!("an activation answers a snapshot");
    };
    assert_eq!(snapshot.session_id, Some(second));
}

#[rstest::rstest]
#[tokio::test]
async fn activate_replies_with_the_activated_sessions_transcript() {
    // Given two loaded sessions, each with its own entry.
    let h = harness();
    push(&h.state, ChatEntry::user("first session's entry"));
    let second = jinn_core_types::SessionId::new();
    {
        let mut guard = h.state.write();
        let session = guard.session.get_or_create(&second);
        session.push_entry(ChatEntry::user("second session's entry"));
    }

    // When the page activates the second.
    let reply = dispatch(
        ClientMsg::ActivateSession {
            session_id: second.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the snapshot carries the second session's entry alone.
    let [ServerMsg::Snapshot(snapshot)] = reply.frames.as_slice() else {
        unreachable!("an activation answers a snapshot");
    };
    let texts: Vec<String> = snapshot
        .entries
        .iter()
        .map(|envelope| serde_json::to_string(&envelope.entry).unwrap_or_default())
        .collect();
    assert!(
        texts
            .iter()
            .any(|text| text.contains("second session's entry")),
        "the snapshot carries the activated session's entry: {texts:?}"
    );
    assert!(
        !texts
            .iter()
            .any(|text| text.contains("first session's entry")),
        "the snapshot carries no entry from the session it left: {texts:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn activate_publishes_a_load_request_without_a_width() {
    // Given two loaded sessions.
    let h = harness();
    let second = jinn_core_types::SessionId::new();
    h.state.write().session.get_or_create(&second);

    // When the page activates the second.
    say(
        &h,
        ClientMsg::ActivateSession {
            session_id: second.clone(),
        },
    )
    .await;

    // Then the store is asked to load it, with no content width.
    let loads = h.audit.of_type::<SessionLoadRequested>();
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0].session_id, second);
    assert_eq!(loads[0].content_width, None);
}

#[rstest::rstest]
#[tokio::test]
async fn activate_of_an_unknown_session_is_refused() {
    // Given a page naming a session that is not loaded.
    let h = harness();

    // When it asks to activate that session.
    let reply = dispatch(
        ClientMsg::ActivateSession {
            session_id: jinn_core_types::SessionId::new(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told so, and nothing is published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
}

#[rstest::rstest]
#[tokio::test]
async fn activate_of_a_stored_session_loads_it_instead_of_refusing() {
    // Given a session that is in the store but not in memory.
    let h = harness_with_store();
    let session_id = store_only_session(&h, "archived notes").await;

    // When the page asks to activate it.
    let reply = dispatch(
        ClientMsg::ActivateSession {
            session_id: session_id.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the load is requested, and the page is answered with a snapshot
    // of the stored session rather than an error.
    let loads = h.audit.of_type::<SessionLoadRequested>();
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0].session_id, session_id);
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Snapshot(_)]));
}

// ---------------------------------------------------------------------------
// Switch model
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn switch_model_publishes_provider_switch_then_last_model() {
    // Given a session.
    let h = harness();

    // When the page picks a model.
    say(
        &h,
        ClientMsg::SwitchModel {
            provider_id: "ollama/llama3".to_owned(),
        },
    )
    .await;

    // Then the switch is requested and the last model recorded, in that order.
    assert_eq!(h.audit.names(), ["ProviderSwitch", "UpdateAppState"]);
}

#[rstest::rstest]
#[tokio::test]
async fn switch_model_does_not_write_the_model_synchronously() {
    // Given a session with no model.
    let h = harness();

    // When the page picks a model.
    say(
        &h,
        ClientMsg::SwitchModel {
            provider_id: "ollama/llama3".to_owned(),
        },
    )
    .await;

    // Then the session's model is unchanged — it arrives with ProviderSwitched.
    assert_eq!(
        h.state.read().session.active_session().model_selection(),
        &ModelSelection::default()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn switch_model_records_the_picked_id_as_a_single_selection() {
    // Given a session.
    let h = harness();

    // When the page picks a model.
    say(
        &h,
        ClientMsg::SwitchModel {
            provider_id: "ollama/llama3".to_owned(),
        },
    )
    .await;

    // Then both messages carry it as a single-model selection.
    let switches = h.audit.of_type::<ProviderSwitch>();
    assert_eq!(
        switches[0].provider_id,
        ModelSelection::Single("ollama/llama3".to_owned())
    );
    assert_eq!(h.audit.of_type::<UpdateAppState>().len(), 1);
}

// ---------------------------------------------------------------------------
// Rename
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn rename_writes_the_trimmed_title() {
    // Given a session.
    let h = harness();

    // When the page renames it.
    say(
        &h,
        ClientMsg::Rename {
            title: "  Road trip  ".to_owned(),
        },
    )
    .await;

    // Then the session's title is the trimmed text.
    assert_eq!(
        h.state.read().session.active_session().title(),
        Some("Road trip")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn rename_replies_with_the_new_title() {
    // Given a session.
    let h = harness();
    let id = active_id(&h.state);

    // When the page renames it.
    let reply = dispatch(
        ClientMsg::Rename {
            title: "  Road trip ".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told the session's new title, trimmed.
    assert!(matches!(
        reply.frames.as_slice(),
        [ServerMsg::Title(body)]
            if body.session_id == id && body.title == "Road trip"
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn rename_publishes_a_persist_for_the_session() {
    // Given a session.
    let h = harness();
    let id = active_id(&h.state);

    // When the page renames it.
    say(
        &h,
        ClientMsg::Rename {
            title: "Road trip".to_owned(),
        },
    )
    .await;

    // Then the store is asked to persist exactly that session.
    let persists = h.audit.of_type::<PersistSession>();
    assert_eq!(persists.len(), 1);
    assert_eq!(persists[0].session_id, id);
}

#[rstest::rstest]
#[tokio::test]
async fn rename_to_blank_is_refused_and_leaves_the_title() {
    // Given a session with a title.
    let h = harness();
    let id = active_id(&h.state);
    if let Some(session) = h.state.write().session.get_mut(&id) {
        session.set_title("Keep".to_owned());
    }

    // When the page renames it to whitespace.
    let reply = dispatch(
        ClientMsg::Rename {
            title: "  ".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is refused and the title is untouched.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(
        h.state.read().session.active_session().title(),
        Some("Keep")
    );
}

// ---------------------------------------------------------------------------
// Hello
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn hello_on_a_fresh_page_answers_a_snapshot() {
    // Given a session with a transcript.
    let h = harness();
    push(&h.state, ChatEntry::user("one"));

    // When a page connects holding nothing.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: None,
            revision: None,
        },
        &h.handle,
    )
    .await;

    // Then it receives the whole state in one frame.
    let [ServerMsg::Snapshot(snapshot)] = reply.frames.as_slice() else {
        unreachable!("a fresh hello answers a snapshot");
    };
    assert_eq!(snapshot.entries.len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn hello_with_a_matching_cursor_and_revision_sends_only_later_entries() {
    // Given a page that saw the first of two entries at the current revision.
    let h = harness();
    push(&h.state, ChatEntry::user("one"));
    let cursor = active_last_entry_id(&h.state);
    let revision = h.handle.revision(&active_id(&h.state));
    push(&h.state, ChatEntry::user("two"));

    // When it reconnects naming both.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: Some(cursor),
            revision: Some(revision),
        },
        &h.handle,
    )
    .await;

    // Then it receives the second entry alone, at its live index.
    let [ServerMsg::EntryUpsert(upsert)] = reply.frames.as_slice() else {
        unreachable!("a resumable hello answers upserts");
    };
    assert_eq!(upsert.index, 1);
}

#[rstest::rstest]
#[tokio::test]
async fn hello_with_a_stale_revision_answers_a_snapshot() {
    // Given a page whose revision predates an in-place rewrite of the history.
    let h = harness();
    push(&h.state, ChatEntry::user("one"));
    let cursor = active_last_entry_id(&h.state);
    h.handle.bump(&active_id(&h.state));

    // When it reconnects with the old revision.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: Some(cursor),
            revision: Some(0),
        },
        &h.handle,
    )
    .await;

    // Then it receives the whole transcript rather than a guess.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Snapshot(_)]));
}

#[rstest::rstest]
#[tokio::test]
async fn hello_with_an_unknown_cursor_answers_a_snapshot() {
    // Given a page naming an entry the session no longer holds.
    let h = harness();
    push(&h.state, ChatEntry::user("one"));

    // When it reconnects.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: Some(jinn_core_types::ChatEntryId::new()),
            revision: Some(0),
        },
        &h.handle,
    )
    .await;

    // Then it receives the whole transcript.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Snapshot(_)]));
}

fn active_last_entry_id(state: &State) -> jinn_core_types::ChatEntryId {
    state
        .read()
        .session
        .active_session()
        .history()
        .last()
        .map(|e| e.id.clone())
        .expect("history has an entry")
}

// ---------------------------------------------------------------------------
// Stored-session picker
// ---------------------------------------------------------------------------

/// A session with two entries, written to the store only: it is on disk
/// and not in memory, which is what an archived session is.
async fn store_only_session(h: &Harness, title: &str) -> jinn_core_types::SessionId {
    let mut session = jinn_session_state::ChatSessionState::new();
    let id = jinn_core_types::SessionId::new();
    session.set_session_id(id.clone());
    session.set_title(title.to_owned());
    session.push_entry(ChatEntry::user("from the archive"));
    session.push_entry(ChatEntry::assistant("restored from disk"));
    h.handle
        .session_store()
        .save(&session.capture_snapshot())
        .await
        .expect("seed the store");
    id
}

fn summary(title: &str) -> jinn_session_store_msg::SessionSummary {
    jinn_session_store_msg::SessionSummary {
        session_id: jinn_core_types::SessionId::new(),
        title: title.to_owned(),
        updated_at: jiff::Timestamp::UNIX_EPOCH,
        created_at: jiff::Timestamp::UNIX_EPOCH,
        session_state: jinn_session_store_msg::SessionState::Loaded,
        in_library: false,
        is_bot: false,
        parent_session: None,
        project: None,
    }
}

fn titles(rows: &[jinn_webui2_msg::PickerRowBody]) -> Vec<&str> {
    rows.iter().map(|row| row.title.as_str()).collect()
}

#[rstest::rstest]
#[test]
fn picker_filter_keeps_only_titles_that_match() {
    // Given stored sessions with mixed titles.
    let stored = vec![
        summary("jinn work"),
        summary("groceries"),
        summary("jinn plans"),
    ];

    // When the picker is filtered to "jinn".
    let rows = crate::actions::match_titles(stored, "jinn");

    // Then only the jinn titles remain.
    assert_eq!(titles(&rows), ["jinn work", "jinn plans"]);
}

#[rstest::rstest]
#[test]
fn picker_ranks_the_closer_title_match_first() {
    // Given a title that starts with the filter and one that merely contains it.
    let stored = vec![summary("my jinn notes"), summary("jinn work")];

    // When the picker is filtered to "jinn".
    let rows = crate::actions::match_titles(stored, "jinn");

    // Then the prefix match ranks first.
    assert_eq!(titles(&rows), ["jinn work", "my jinn notes"]);
}

#[rstest::rstest]
#[test]
fn picker_with_an_empty_filter_keeps_every_session() {
    // Given three stored sessions.
    let stored = vec![summary("one"), summary("two"), summary("three")];

    // When the picker is unfiltered.
    let rows = crate::actions::match_titles(stored, "");

    // Then all three are listed.
    assert_eq!(rows.len(), 3);
}

#[rstest::rstest]
#[test]
fn picker_row_carries_the_stored_state_and_parent() {
    // Given an archived session forked from another.
    let mut archived = summary("child");
    archived.session_state = jinn_session_store_msg::SessionState::Archived;
    let parent = jinn_core_types::SessionId::new();
    archived.parent_session = Some(parent.clone());

    // When the picker lists it.
    let rows = crate::actions::match_titles(vec![archived], "");

    // Then the row says so.
    assert!(matches!(
        rows.as_slice(),
        [jinn_webui2_msg::PickerRowBody { state, parent_id: Some(p), .. }]
            if *state == jinn_session_store_msg::SessionState::Archived && *p == parent
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn picker_rows_on_an_empty_store_answers_no_rows() {
    // Given a store with no sessions.
    let h = harness();

    // When the page asks for the picker's rows.
    let reply = dispatch(
        ClientMsg::PickerRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then it gets an empty, well-formed answer rather than an error.
    let [ServerMsg::PickerRows(body)] = reply.frames.as_slice() else {
        unreachable!("a picker request answers picker rows");
    };
    assert_eq!(body.rows, Vec::new());
}

// ---------------------------------------------------------------------------
// Model picker
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn model_rows_with_no_configured_providers_answers_no_rows() {
    // Given a registry with no providers.
    let h = harness();

    // When the page asks for the model picker's rows.
    let reply = dispatch(
        ClientMsg::ModelRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then it gets an empty, well-formed answer.
    let [ServerMsg::ModelRows(body)] = reply.frames.as_slice() else {
        unreachable!("a model-rows request answers model rows");
    };
    assert_eq!(body.rows, Vec::new());
}

/// Two providers whose model names share no letters with each other's
/// provider: a filter for one can only be satisfied by a real match.
fn two_providers() -> jinn_provider_config::ProvidersConfig {
    let entry = |model: &str| jinn_provider_config::ProviderEntry {
        backend: "sample".to_owned(),
        models: vec![model.to_owned()],
        base_url: None,
        api_key_env: None,
        requires_key: false,
        extra_body: None,
        context_length: None,
        model_info: Vec::new(),
    };
    jinn_provider_config::ProvidersConfig {
        providers: std::collections::BTreeMap::from([
            ("deepseek".to_owned(), entry("deepseek-chat")),
            ("aion".to_owned(), entry("aion-alpha")),
        ]),
        aliases: vec![],
        default_provider: None,
        endpoint_defaults: vec![],
    }
}

/// The model names in a model-rows reply, in the order the page shows them.
fn model_names(reply: &crate::protocol::Reply) -> Vec<String> {
    let [ServerMsg::ModelRows(body)] = reply.frames.as_slice() else {
        unreachable!("a model-rows request answers model rows");
    };
    body.rows.iter().map(|row| row.model.clone()).collect()
}

#[rstest::rstest]
#[tokio::test]
async fn model_rows_with_an_empty_filter_name_every_provider() {
    // Given two configured providers.
    let h = harness_with_providers(two_providers());

    // When the page asks with no filter.
    let reply = dispatch(
        ClientMsg::ModelRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then both are offered.
    let mut names = model_names(&reply);
    names.sort();
    assert_eq!(names, vec!["aion-alpha", "deepseek-chat"]);
}

#[rstest::rstest]
#[tokio::test]
async fn model_rows_filter_keeps_only_the_matching_provider() {
    // Given two configured providers.
    let h = harness_with_providers(two_providers());

    // When the page filters for one of them.
    let reply = dispatch(
        ClientMsg::ModelRows {
            filter: "deepseek".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then only that provider's model is offered.
    assert_eq!(model_names(&reply), vec!["deepseek-chat"]);
}

#[rstest::rstest]
#[tokio::test]
async fn model_rows_filter_matches_the_provider_name_too() {
    // Given two configured providers, neither model named like its provider.
    let h = harness_with_providers(two_providers());

    // When the page filters by a provider's name.
    let reply = dispatch(
        ClientMsg::ModelRows {
            filter: "aion".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then that provider's model is the one offered.
    assert_eq!(model_names(&reply), vec!["aion-alpha"]);
}

// ---------------------------------------------------------------------------
// Malformed frames
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn a_malformed_frame_is_answered_with_an_error() {
    // Given a frame that is not a ClientMsg.
    let parsed: Result<ClientMsg, _> = serde_json::from_str("{\"type\":\"teleport\"}");

    // When the dispatch is handed the failure.
    let reply = match parsed {
        Err(_) => actions::malformed(),
        Ok(msg) => unreachable!("teleport is not a message: {msg:?}"),
    };

    // Then the page is told, and told in a frame it can read.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
}

// ---------------------------------------------------------------------------
// Streaming: the flush tick is the one reader of streamed text
// ---------------------------------------------------------------------------

/// A connected page over a fresh state: the flush broadcasts to the
/// connection, and the test drains what the page would have received.
struct Page {
    handle: OutboundHandle,
    inbox: tokio::sync::mpsc::UnboundedReceiver<ServerMsg>,
    state: State,
}

fn connected_page() -> Page {
    let h = harness();
    let inbox = h.handle.register();
    Page {
        handle: h.handle,
        inbox,
        state: h.state,
    }
}

/// Puts the active session into its streaming phase, as a live turn does.
fn begin_streaming(page: &Page) {
    page.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();
}

/// Streams `tokens` into the session the way the session actor does when it
/// receives each token: appended to the live assistant entry.
fn stream_tokens(page: &Page, tokens: &[&str]) {
    for token in tokens {
        page.state
            .write()
            .session
            .active_session_mut()
            .append_stream_token(*token, jiff::Timestamp::now())
            .expect("the session is streaming");
    }
}

/// Runs one flush and returns every frame the page received.
fn flush_frames(page: &mut Page) -> Vec<ServerMsg> {
    crate::outbound::flush(&page.handle);
    let mut frames = Vec::new();
    while let Ok(frame) = page.inbox.try_recv() {
        frames.push(frame);
    }
    frames
}

#[rstest::rstest]
#[test]
fn tokens_within_one_window_leave_as_one_delta() {
    // Given a streaming session that has received four tokens.
    let mut page = connected_page();
    begin_streaming(&page);
    stream_tokens(&page, &["This", " is", " a", " sample"]);

    // When the tick flushes.
    let frames = flush_frames(&mut page);

    // Then the text leaves as exactly one delta carrying all four tokens.
    let deltas: Vec<_> = frames
        .iter()
        .filter(|frame| matches!(frame, ServerMsg::EntryDelta(_)))
        .collect();
    assert_eq!(deltas.len(), 1);
    assert!(matches!(
        deltas[0],
        ServerMsg::EntryDelta(body) if body.chunk == "This is a sample"
    ));
}

#[rstest::rstest]
#[test]
fn a_lazily_created_entry_reaches_the_page_before_its_first_delta() {
    // Given a streaming session whose first token created the entry.
    let mut page = connected_page();
    begin_streaming(&page);
    stream_tokens(&page, &["Hello"]);

    // When the tick flushes.
    let frames = flush_frames(&mut page);

    // Then the entry's upsert precedes its delta.
    let upsert_at = frames
        .iter()
        .position(|frame| matches!(frame, ServerMsg::EntryUpsert(_)))
        .expect("the entry is upserted");
    let delta_at = frames
        .iter()
        .position(|frame| matches!(frame, ServerMsg::EntryDelta(_)))
        .expect("the text is sent");
    assert!(
        upsert_at < delta_at,
        "upsert at {upsert_at} must precede delta at {delta_at}"
    );
}

#[rstest::rstest]
#[test]
fn a_flush_with_no_new_text_sends_nothing() {
    // Given a streaming session whose text was already flushed once.
    let mut page = connected_page();
    begin_streaming(&page);
    stream_tokens(&page, &["settled"]);
    let _ = flush_frames(&mut page);

    // When the tick flushes again with nothing new.
    let frames = flush_frames(&mut page);

    // Then the page receives no frame.
    assert!(frames.is_empty());
}

#[rstest::rstest]
#[test]
fn a_later_flush_sends_only_the_text_arrived_since() {
    // Given a streaming session whose first two tokens were flushed.
    let mut page = connected_page();
    begin_streaming(&page);
    stream_tokens(&page, &["Hello", " world"]);
    let _ = flush_frames(&mut page);

    // When two more tokens arrive and the tick flushes.
    stream_tokens(&page, &["!", " Done"]);
    let frames = flush_frames(&mut page);

    // Then the delta carries only the new text.
    assert!(matches!(
        frames.as_slice(),
        [ServerMsg::EntryDelta(body)] if body.chunk == "! Done"
    ));
}

#[rstest::rstest]
#[test]
fn thinking_streams_as_its_own_field() {
    // Given a streaming session that has received a thinking token — which
    // creates the thinking entry first, the way the session actor does.
    let mut page = connected_page();
    begin_streaming(&page);
    {
        let mut guard = page.state.write();
        let session = guard.session.active_session_mut();
        session.begin_thinking(jiff::Timestamp::now());
        session
            .append_thinking_token("let me think")
            .expect("the session is streaming");
    }

    // When the tick flushes.
    let frames = flush_frames(&mut page);

    // Then the delta names the thinking field.
    assert!(matches!(
        frames.as_slice(),
        [_, ServerMsg::EntryDelta(body)] if body.field == jinn_webui2_msg::DeltaField::Thinking
    ));
}

#[rstest::rstest]
#[test]
fn nothing_is_flushed_while_no_page_is_connected() {
    // Given a streaming session with tokens, and no page connected.
    let h = harness();
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();
    h.state
        .write()
        .session
        .active_session_mut()
        .append_stream_token("unread", jiff::Timestamp::now())
        .expect("the session is streaming");

    // When the tick flushes.
    crate::outbound::flush(&h.handle);

    // Then a page connecting afterwards is sent the entry whole — an upsert
    // first, carrying the text the tick never streamed — never a gap.
    let mut inbox = h.handle.register();
    crate::outbound::flush(&h.handle);
    let first = inbox.try_recv().expect("the new page receives the entry");
    assert!(matches!(first, ServerMsg::EntryUpsert(_)));
}

// ---------------------------------------------------------------------------
// Reconcile: a turn that settles between ticks still reaches the page
// ---------------------------------------------------------------------------

/// Puts the active session through a whole turn by hand — begins streaming,
/// appends text, and settles to idle — the way a turn shorter than one
/// flush interval looks to the actor.
fn settle_an_instant_turn(state: &State, text: &str) {
    let mut guard = state.write();
    let session = guard.session.active_session_mut();
    session.begin_streaming();
    session
        .append_stream_token(text, jiff::Timestamp::now())
        .expect("the session is streaming");
    session.settle_to_idle();
}

/// Everything the connected page has received since it was last drained.
fn drain(inbox: &mut tokio::sync::mpsc::UnboundedReceiver<ServerMsg>) -> Vec<ServerMsg> {
    let mut frames = Vec::new();
    while let Ok(frame) = inbox.try_recv() {
        frames.push(frame);
    }
    frames
}

#[rstest::rstest]
#[test]
fn an_instant_turn_reaches_the_page_when_it_settles() {
    // Given a connected page holding one entry, and a turn that began and
    // settled before any tick read it.
    let h = harness();
    let mut inbox = h.handle.register();
    push(&h.state, ChatEntry::user("before"));
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);
    settle_an_instant_turn(&h.state, "instant reply");

    // When the settle reconciles the history.
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));

    // Then the settled assistant entry is the one frame the page receives.
    let frames = drain(&mut inbox);
    assert!(matches!(
        frames.as_slice(),
        [ServerMsg::EntryUpsert(body)] if body.index == 1
    ));
}

#[rstest::rstest]
#[test]
fn a_settled_turn_that_changed_nothing_sends_no_frame() {
    // Given a connected page that holds every entry the session has.
    let h = harness();
    let mut inbox = h.handle.register();
    push(&h.state, ChatEntry::user("held"));
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);

    // When another reconcile runs with the history unchanged.
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));

    // Then the page receives nothing.
    assert!(drain(&mut inbox).is_empty());
}

#[rstest::rstest]
#[test]
fn an_entry_that_changed_while_settling_is_sent_again() {
    // Given a connected page that holds one entry as it stood before a turn.
    let h = harness();
    let mut inbox = h.handle.register();
    push(&h.state, ChatEntry::user("before"));
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);
    let entry_id = h.state.read().session.active_session().history()[0]
        .id
        .clone();

    // When the entry is rewritten in place — a pin does that — and a
    // settle reconciles the history.
    h.state
        .write()
        .session
        .active_session_mut()
        .pin_entry(&entry_id, jinn_core_types::PinPosition::Top);
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));

    // Then the rewritten entry is the one frame the page receives.
    assert!(matches!(
        drain(&mut inbox).as_slice(),
        [ServerMsg::EntryUpsert(_)]
    ));
}

#[rstest::rstest]
#[test]
fn an_entry_the_snapshot_delivered_is_not_sent_again() {
    // Given a page that connected and was answered with a snapshot.
    let h = harness();
    let mut inbox = h.handle.register();
    push(&h.state, ChatEntry::user("in the snapshot"));
    h.handle.mark_delivered(&active_id(&h.state));
    let _ = drain(&mut inbox);

    // When a reconcile runs with the history unchanged.
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));

    // Then the snapshot's entry is not re-sent.
    assert!(drain(&mut inbox).is_empty());
}

// ---------------------------------------------------------------------------
// Skills modal: rows
// ---------------------------------------------------------------------------

/// A discovered global skill named `name`, with a description.
fn skill(
    dir: &marked_path::MarkedPathBuf<marked_path::Absolute>,
    name: &str,
) -> jinn_skills_msg::Skill {
    jinn_skills_msg::Skill {
        name: name.to_owned(),
        description: format!("{name} description"),
        body: String::new(),
        file_path: dir
            .join_all([name, "SKILL.md"])
            .expect("typed parent joins bare names"),
        base_dir: dir.join(name).expect("typed parent joins a bare name"),
        source: jinn_skills_msg::SkillSource::Global,
    }
}

/// Seeds the active session's discovered set with the named skills, in
/// the order given. The directory outlives the seeded paths.
fn discover(h: &Harness, names: &[&str]) -> jinn_testutil::JinnTempDir {
    let dir = jinn_testutil::tempdir();
    let skills: Vec<_> = names.iter().map(|name| skill(&dir.path, name)).collect();
    h.state
        .write()
        .session
        .active_session_mut()
        .set_discovered_skills(skills);
    dir
}

/// Every skill row in a reply's one `SkillRows` frame; fails the test
/// when the reply is anything else.
fn skill_rows(reply: &crate::protocol::Reply) -> Vec<jinn_webui2_msg::SkillRowBody> {
    let [ServerMsg::SkillRows(body)] = reply.frames.as_slice() else {
        unreachable!("one skill-rows frame expected: {reply:?}");
    };
    body.rows.clone()
}

/// The names of every skill row in a reply, in order.
fn skill_names(reply: &crate::protocol::Reply) -> Vec<String> {
    skill_rows(reply).into_iter().map(|row| row.name).collect()
}

#[rstest::rstest]
#[tokio::test]
async fn skill_rows_list_the_discovered_skills_sorted_by_name() {
    // Given a session that discovered two skills, in non-sorted order.
    let h = harness();
    let _dir = discover(&h, &["zeta", "alpha"]);

    // When the page asks for the rows with no filter.
    let reply = dispatch(
        ClientMsg::SkillRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then both are listed, alphabetically.
    assert_eq!(skill_names(&reply), ["alpha", "zeta"]);
}

#[rstest::rstest]
#[tokio::test]
async fn skill_rows_match_the_shared_filter() {
    // Given a session that discovered two skills.
    let h = harness();
    let _dir = discover(&h, &["zeta", "alpha"]);

    // When the page asks for the rows with a filter only one name matches.
    let reply = dispatch(
        ClientMsg::SkillRows {
            filter: "zet".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then only that skill is listed, and its name's matched run is marked.
    let rows = skill_rows(&reply);
    let [row] = rows.as_slice() else {
        unreachable!("one row expected: {reply:?}");
    };
    assert_eq!(row.name, "zeta");
    assert!(row.name_runs.iter().any(|run| run.matched));
}

#[rstest::rstest]
#[tokio::test]
async fn skill_rows_carry_each_skills_enablement() {
    // Given a session that discovered two skills and withholds one.
    let h = harness();
    let _dir = discover(&h, &["alpha", "beta"]);
    h.state
        .write()
        .session
        .active_session_mut()
        .set_skill_filter(Some(jinn_core_types::NameFilter::deny(["beta".to_owned()])));

    // When the page asks for the rows.
    let reply = dispatch(
        ClientMsg::SkillRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the withheld skill is the one row marked disabled.
    let enabled: Vec<_> = skill_rows(&reply)
        .into_iter()
        .map(|row| (row.name, row.enabled))
        .collect();
    assert_eq!(
        enabled,
        [("alpha".to_owned(), true), ("beta".to_owned(), false)]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn skill_rows_mark_a_skill_already_loaded_into_context() {
    // Given a session that discovered one skill and holds the skill's
    // pinned body in its history.
    let h = harness();
    let _dir = discover(&h, &["alpha"]);
    let mut pinned = ChatEntry::tool_result(
        "call-1",
        "skill",
        "<skill name=\"alpha\">body</skill>",
        jinn_core_types::ToolResultStatus::Success,
    );
    pinned.pin_position = Some(jinn_core_types::PinPosition::Relative);
    push(&h.state, pinned);

    // When the page asks for the rows.
    let reply = dispatch(
        ClientMsg::SkillRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then that row is flagged loaded.
    let rows = skill_rows(&reply);
    let [row] = rows.as_slice() else {
        unreachable!("one row expected: {reply:?}");
    };
    assert!(row.loaded);
}

#[rstest::rstest]
#[tokio::test]
async fn a_skill_rows_frame_names_the_session_and_echoes_the_filter() {
    // Given a session that discovered one skill.
    let h = harness();
    let _dir = discover(&h, &["alpha"]);

    // When the page asks for the rows with a filter.
    let reply = dispatch(
        ClientMsg::SkillRows {
            filter: "alp".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the one frame names the active session and echoes the filter.
    let [ServerMsg::SkillRows(body)] = reply.frames.as_slice() else {
        unreachable!("one skill-rows frame expected: {reply:?}");
    };
    assert_eq!(body.session_id, active_id(&h.state));
    assert_eq!(body.filter, "alp");
}

#[rstest::rstest]
#[tokio::test]
async fn a_discovery_scan_pushes_rows_for_its_session_in_the_last_asked_filter() {
    // Given a page connected to a session whose modal asked for a filter,
    // before the session discovered anything.
    let h = harness();
    let mut inbox = h.handle.register();
    let _ = dispatch(
        ClientMsg::SkillRows {
            filter: "alp".to_owned(),
        },
        &h.handle,
    )
    .await;
    let _ = drain(&mut inbox);
    let _dir = discover(&h, &["alpha", "zeta"]);

    // When a discovery scan lands for that session.
    skills_loaded(
        &h,
        &jinn_skills_msg::SkillsLoaded {
            session_id: active_id(&h.state),
            skills: Vec::new(),
            error: None,
        },
    );

    // Then the page receives the fresh rows in the filter it last asked for.
    let frames = drain(&mut inbox);
    let [ServerMsg::SkillRows(body)] = frames.as_slice() else {
        unreachable!("one skill-rows frame expected");
    };
    assert_eq!(body.filter, "alp");
    assert_eq!(body.session_id, active_id(&h.state));
    let names: Vec<_> = body.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, ["alpha"]);
}

#[rstest::rstest]
#[tokio::test]
async fn a_discovery_scan_for_another_session_pushes_nothing_for_this_one() {
    // Given a page connected to a session that discovered one skill.
    let h = harness();
    let mut inbox = h.handle.register();
    let _dir = discover(&h, &["alpha"]);
    let _ = drain(&mut inbox);

    // When a discovery scan lands for a different, unknown session.
    skills_loaded(
        &h,
        &jinn_skills_msg::SkillsLoaded {
            session_id: jinn_core_types::SessionId::new(),
            skills: Vec::new(),
            error: None,
        },
    );

    // Then the frame it pushes names that other session, so a page
    // viewing this one leaves its rows alone.
    let frames = drain(&mut inbox);
    let [ServerMsg::SkillRows(body)] = frames.as_slice() else {
        unreachable!("one skill-rows frame expected");
    };
    assert_ne!(body.session_id, active_id(&h.state));
}

/// Drives the actor's `SkillsLoaded` behaviour with nothing spawned, the
/// way the tick's and the reconcile's tests drive theirs.
fn skills_loaded(h: &Harness, msg: &jinn_skills_msg::SkillsLoaded) {
    crate::outbound::skills_loaded(&h.handle, msg);
}

// ---------------------------------------------------------------------------
// Skills modal: toggle
// ---------------------------------------------------------------------------

/// A harness whose active session discovered the named skills; the
/// returned directory is what their paths name and must outlive the test.
fn harness_with_skills(names: &[&str]) -> (Harness, jinn_testutil::JinnTempDir) {
    let h = harness();
    let dir = discover(&h, names);
    (h, dir)
}

/// Pins `name`'s skill body into the active session's history, as the
/// force-load does: a `skill` tool call followed by a pinned result.
fn pin_loaded(h: &Harness, name: &str) {
    let mut call = ChatEntry::tool_call(
        format!("call-{name}"),
        "skill",
        format!("{{\"name\":\"{name}\"}}"),
    );
    call.pin_position = Some(jinn_core_types::PinPosition::Relative);
    push(&h.state, call);
    let mut result = ChatEntry::tool_result(
        format!("result-{name}"),
        "skill",
        format!("<skill name=\"{name}\">body</skill>"),
        jinn_core_types::ToolResultStatus::Success,
    );
    result.pin_position = Some(jinn_core_types::PinPosition::Relative);
    push(&h.state, result);
}

/// Withholds `name` on the active session's filter, as a deny filter.
fn withhold(h: &Harness, name: &str) {
    h.state
        .write()
        .session
        .active_session_mut()
        .set_skill_filter(Some(jinn_core_types::NameFilter::deny([name.to_owned()])));
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_an_enabled_skill_withholds_it_on_the_session_filter() {
    // Given a session that discovered one skill and permits it.
    let (h, _dir) = harness_with_skills(&["alpha"]);

    // When the page toggles that skill.
    let reply = dispatch(
        ClientMsg::ToggleSkill {
            skill: "alpha".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the session's filter withholds it, and the row says so.
    assert!(
        !h.state
            .read()
            .session
            .active_session()
            .is_skill_enabled("alpha")
    );
    let enabled: Vec<_> = skill_rows(&reply)
        .into_iter()
        .map(|row| (row.name, row.enabled))
        .collect();
    assert_eq!(enabled, [("alpha".to_owned(), false)]);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_a_disabled_skill_permits_it_again() {
    // Given a session that discovered one skill and withholds it.
    let (h, _dir) = harness_with_skills(&["alpha"]);
    withhold(&h, "alpha");

    // When the page toggles that skill.
    let _ = dispatch(
        ClientMsg::ToggleSkill {
            skill: "alpha".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the skill is permitted again.
    assert!(
        h.state
            .read()
            .session
            .active_session()
            .is_skill_enabled("alpha")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_keeps_an_allow_mode_filter_in_allow_mode() {
    // Given a session whose filter is an allow list naming one of its two
    // skills.
    let (h, _dir) = harness_with_skills(&["alpha", "beta"]);
    h.state
        .write()
        .session
        .active_session_mut()
        .set_skill_filter(Some(jinn_core_types::NameFilter {
            mode: jinn_core_types::FilterMode::Allow,
            names: ["alpha".to_owned()].into_iter().collect(),
        }));

    // When the page withholds that skill by toggling it.
    let _ = dispatch(
        ClientMsg::ToggleSkill {
            skill: "alpha".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the filter is still an allow list — it was not rewritten as a
    // blocklist — and the skill is withheld.
    let state = h.state.read();
    let filter = state
        .session
        .active_session()
        .skill_filter()
        .expect("a filter was written");
    assert_eq!(filter.mode, jinn_core_types::FilterMode::Allow);
    assert!(!filter.permits("alpha"));
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_a_loaded_skill_changes_nothing() {
    // Given a session that discovered one skill, permits it, and holds
    // its pinned pair.
    let (h, _dir) = harness_with_skills(&["alpha"]);
    pin_loaded(&h, "alpha");

    // When the page toggles that skill.
    let _ = dispatch(
        ClientMsg::ToggleSkill {
            skill: "alpha".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the session still has no filter at all — nothing was written.
    assert!(
        h.state
            .read()
            .session
            .active_session()
            .skill_filter()
            .is_none()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_a_skill_publishes_nothing() {
    // Given a session that discovered one skill.
    let (h, _dir) = harness_with_skills(&["alpha"]);

    // When the page toggles that skill.
    let _ = dispatch(
        ClientMsg::ToggleSkill {
            skill: "alpha".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then no bus message was published.
    assert_eq!(h.audit.names(), Vec::<&str>::new());
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_an_undiscovered_skill_is_refused() {
    // Given a session that discovered one skill.
    let (h, _dir) = harness_with_skills(&["alpha"]);

    // When the page toggles a skill the session never discovered.
    let reply = dispatch(
        ClientMsg::ToggleSkill {
            skill: "phantom".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told so, and the filter is untouched.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        unreachable!("one error frame expected: {reply:?}");
    };
    assert_eq!(error.message, "unknown skill");
    assert!(
        h.state
            .read()
            .session
            .active_session()
            .skill_filter()
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Skills modal: load
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn loading_a_skill_pins_its_body_as_a_tool_call_and_result() {
    // Given a session that discovered one skill.
    let (h, _dir) = harness_with_skills(&["alpha"]);

    // When the page loads that skill.
    let reply = dispatch(
        ClientMsg::LoadSkill {
            skill: "alpha".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then history ends with a `skill` call and a pinned `skill` result
    // carrying the body, and the reply upserts both.
    let state = h.state.read();
    let history = state.session.active_session().history();
    let len = history.len();
    let ChatEntryKind::ToolCall { name, .. } = &history[len - 2].kind else {
        unreachable!("a tool call was expected: {:?}", history[len - 2].kind);
    };
    assert_eq!(name, "skill");
    let ChatEntryKind::ToolResult { name, content, .. } = &history[len - 1].kind else {
        unreachable!("a tool result was expected: {:?}", history[len - 1].kind);
    };
    assert_eq!(name, "skill");
    assert!(content.starts_with("<skill name=\"alpha\""));
    assert!(history[len - 1].is_pinned());
    drop(state);
    let upserts: Vec<_> = reply
        .frames
        .iter()
        .filter(|frame| matches!(frame, ServerMsg::EntryUpsert(_)))
        .collect();
    assert_eq!(upserts.len(), 2);
}

#[rstest::rstest]
#[tokio::test]
async fn loading_a_disabled_skill_enables_it_first() {
    // Given a session that discovered one skill and withholds it.
    let (h, _dir) = harness_with_skills(&["alpha"]);
    withhold(&h, "alpha");

    // When the page loads that skill.
    let _ = dispatch(
        ClientMsg::LoadSkill {
            skill: "alpha".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the skill is permitted on the session's filter.
    assert!(
        h.state
            .read()
            .session
            .active_session()
            .is_skill_enabled("alpha")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn loading_a_skill_publishes_only_that_the_session_was_interacted_with() {
    // Given a session that discovered one skill.
    let (h, _dir) = harness_with_skills(&["alpha"]);

    // When the page loads that skill.
    let _ = dispatch(
        ClientMsg::LoadSkill {
            skill: "alpha".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then exactly one message was published, naming that interaction.
    assert_eq!(h.audit.names(), ["MarkSessionInteracted"]);
}

#[rstest::rstest]
#[tokio::test]
async fn loading_an_already_loaded_skill_appends_a_notice_and_publishes_nothing() {
    // Given a session that discovered one skill and already holds its
    // pinned pair.
    let (h, _dir) = harness_with_skills(&["alpha"]);
    pin_loaded(&h, "alpha");

    // When the page loads that skill again.
    let _ = dispatch(
        ClientMsg::LoadSkill {
            skill: "alpha".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then a transient notice was appended, and nothing was published.
    let state = h.state.read();
    let history = state.session.active_session().history();
    let ChatEntryKind::Transient(text) = &history[history.len() - 1].kind else {
        unreachable!(
            "a transient was expected: {:?}",
            history[history.len() - 1].kind
        );
    };
    assert!(text.contains("already loaded"));
    drop(state);
    assert_eq!(h.audit.names(), Vec::<&str>::new());
}

#[rstest::rstest]
#[tokio::test]
async fn loading_an_undiscovered_skill_is_refused() {
    // Given a session that discovered one skill.
    let (h, _dir) = harness_with_skills(&["alpha"]);

    // When the page loads a skill the session never discovered.
    let reply = dispatch(
        ClientMsg::LoadSkill {
            skill: "phantom".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told so, and nothing was appended or published.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        unreachable!("one error frame expected: {reply:?}");
    };
    assert_eq!(error.message, "unknown skill");
    assert_eq!(h.audit.names(), Vec::<&str>::new());
}

// ---------------------------------------------------------------------------
// Tool rows / toggle
// ---------------------------------------------------------------------------

/// A harness with the given tools in the registry.
fn harness_with_tools(tools: &[(&'static str, &'static str)]) -> Harness {
    harness_seeded(
        empty_providers(),
        Arc::new(jinn_kernel::common::services::test_services::FakeSessionStore),
        Seed {
            tools: tools.to_vec(),
            ..Seed::default()
        },
    )
}

/// The tool names one rows reply carries, in order.
fn tool_names(reply: &crate::protocol::Reply) -> Vec<String> {
    let [ServerMsg::ToolRows(body)] = reply.frames.as_slice() else {
        unreachable!("one tool_rows frame expected: {reply:?}");
    };
    body.rows.iter().map(|row| row.name.clone()).collect()
}

/// Whether the session's filter permits `tool`.
fn tool_permits(state: &State, tool: &str) -> bool {
    let guard = state.read();
    let session = guard.session.active_session();
    session
        .tool_filter()
        .is_none_or(|filter| filter.permits(tool))
}

#[rstest::rstest]
#[tokio::test]
async fn tool_rows_list_the_registry_tools_sorted_case_insensitively() {
    // Given a registry holding two tools whose names differ in case.
    let h = harness_with_tools(&[("get_time", "Tells the time."), ("Bash", "Runs a command.")]);

    // When the page asks for the tools modal's rows.
    let reply = dispatch(
        ClientMsg::ToolRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then both are listed, case-insensitively sorted by name.
    assert_eq!(tool_names(&reply), ["Bash", "get_time"]);
}

#[rstest::rstest]
#[tokio::test]
async fn tool_rows_carry_the_descriptions_first_sentence() {
    // Given a registry tool whose description has two sentences.
    let h = harness_with_tools(&[("get_time", "Tells the time. Also the date.")]);

    // When the page asks for the tools modal's rows.
    let reply = dispatch(
        ClientMsg::ToolRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the row's description is the first sentence alone.
    let [ServerMsg::ToolRows(body)] = reply.frames.as_slice() else {
        unreachable!("one tool_rows frame expected: {reply:?}");
    };
    assert_eq!(body.rows[0].description, "Tells the time.");
}

#[rstest::rstest]
#[tokio::test]
async fn tool_rows_with_no_registry_answer_no_rows() {
    // Given a state whose registry holds nothing.
    let h = harness();

    // When the page asks for the tools modal's rows.
    let reply = dispatch(
        ClientMsg::ToolRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the frame carries no rows and is not a refusal.
    assert!(tool_names(&reply).is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn tool_rows_match_the_shared_filter() {
    // Given a registry holding two tools.
    let h = harness_with_tools(&[("get_time", "Tells the time."), ("bash", "Runs a command.")]);

    // When the page asks for the rows under a filter only one matches.
    let reply = dispatch(
        ClientMsg::ToolRows {
            filter: "time".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then only the matching tool is listed.
    assert_eq!(tool_names(&reply), ["get_time"]);
}

#[rstest::rstest]
#[tokio::test]
async fn tool_rows_split_the_matched_run_for_the_filter() {
    // Given a registry holding one tool.
    let h = harness_with_tools(&[("get_time", "Tells the time.")]);

    // When the page asks for the rows under a filter the row matches.
    let reply = dispatch(
        ClientMsg::ToolRows {
            filter: "time".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the row's text is split into runs, at least one of them marked
    // matched — the matcher places its hits wherever they score best
    // across `{name} {description}`, so the field is not pinned here, only
    // that the browser has a stretch to highlight.
    let [ServerMsg::ToolRows(body)] = reply.frames.as_slice() else {
        unreachable!("one tool_rows frame expected: {reply:?}");
    };
    let all_runs = body.rows[0]
        .name_runs
        .iter()
        .chain(body.rows[0].description_runs.iter());
    assert!(
        all_runs
            .clone()
            .any(|run| run.matched && !run.text.is_empty())
    );
    // And the runs still reconstruct the row's two fields.
    let name: String = body.rows[0]
        .name_runs
        .iter()
        .map(|run| run.text.as_str())
        .collect();
    assert_eq!(name, "get_time");
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_an_enabled_tool_withholds_it_in_a_deny_filter() {
    // Given a registry holding two tools, both permitted.
    let h = harness_with_tools(&[("get_time", "Tells the time."), ("bash", "Runs a command.")]);
    let tool = "get_time";

    // When the page toggles one.
    say(
        &h,
        ClientMsg::ToggleTool {
            tool: tool.to_owned(),
        },
    )
    .await;

    // Then the session's filter is a deny filter naming that tool alone.
    let guard = h.state.read();
    let filter = guard
        .session
        .active_session()
        .tool_filter()
        .expect("a filter");
    assert_eq!(filter.mode, jinn_core_types::FilterMode::Deny);
    assert!(!filter.permits(tool));
    assert!(filter.permits("bash"));
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_a_disabled_tool_permits_it_again() {
    // Given a registry holding two tools, one already withheld.
    let h = harness_with_tools(&[("get_time", "Tells the time."), ("bash", "Runs a command.")]);
    let tool = "get_time";
    say(
        &h,
        ClientMsg::ToggleTool {
            tool: tool.to_owned(),
        },
    )
    .await;
    assert!(!tool_permits(&h.state, tool));

    // When the page toggles it again.
    say(
        &h,
        ClientMsg::ToggleTool {
            tool: tool.to_owned(),
        },
    )
    .await;

    // Then it is permitted again, and the other still is.
    assert!(tool_permits(&h.state, tool));
    assert!(tool_permits(&h.state, "bash"));
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_a_tool_replies_with_rows_marking_it_disabled() {
    // Given a registry holding one permitted tool.
    let h = harness_with_tools(&[("get_time", "Tells the time.")]);

    // When the page toggles it.
    let reply = dispatch(
        ClientMsg::ToggleTool {
            tool: "get_time".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the reply's rows mark it disabled.
    let [ServerMsg::ToolRows(body)] = reply.frames.as_slice() else {
        unreachable!("one tool_rows frame expected: {reply:?}");
    };
    assert!(!body.rows[0].enabled);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_a_tool_publishes_nothing() {
    // Given a registry holding one tool.
    let h = harness_with_tools(&[("get_time", "Tells the time.")]);

    // When the page toggles it.
    say(
        &h,
        ClientMsg::ToggleTool {
            tool: "get_time".to_owned(),
        },
    )
    .await;

    // Then nothing was published.
    assert_eq!(h.audit.names(), Vec::<&str>::new());
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_an_unknown_tool_is_refused() {
    // Given a registry holding one tool.
    let h = harness_with_tools(&[("get_time", "Tells the time.")]);

    // When the page toggles a tool the registry does not offer.
    let reply = dispatch(
        ClientMsg::ToggleTool {
            tool: "phantom".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told so, and the session holds no filter.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        unreachable!("one error frame expected: {reply:?}");
    };
    assert_eq!(error.message, "unknown tool");
    assert!(
        h.state
            .read()
            .session
            .active_session()
            .tool_filter()
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// MCP rows / toggle
// ---------------------------------------------------------------------------

/// A harness with the given servers in the `[mcp]` config section.
fn harness_with_mcp(servers: &[(&'static str, &'static str)]) -> Harness {
    harness_seeded(
        empty_providers(),
        Arc::new(jinn_kernel::common::services::test_services::FakeSessionStore),
        Seed {
            mcp_servers: servers.to_vec(),
            ..Seed::default()
        },
    )
}

/// The MCP names one rows reply carries, in order.
fn mcp_names(reply: &crate::protocol::Reply) -> Vec<String> {
    let [ServerMsg::McpRows(body)] = reply.frames.as_slice() else {
        unreachable!("one mcp_rows frame expected: {reply:?}");
    };
    body.rows.iter().map(|row| row.name.clone()).collect()
}

#[rstest::rstest]
#[tokio::test]
async fn mcp_rows_list_the_configured_servers_sorted_case_insensitively() {
    // Given a config holding two servers whose names differ in case.
    let h = harness_with_mcp(&[
        ("echo", "jinn luau serve a.lua"),
        ("Browser", "jinn mcp browser"),
    ]);

    // When the page asks for the MCP modal's rows.
    let reply = dispatch(
        ClientMsg::McpRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then both are listed, case-insensitively sorted by name.
    assert_eq!(mcp_names(&reply), ["Browser", "echo"]);
}

#[rstest::rstest]
#[tokio::test]
async fn mcp_rows_carry_the_launch_summary_as_the_description() {
    // Given a config holding one server.
    let h = harness_with_mcp(&[("echo", "jinn luau serve a.lua")]);

    // When the page asks for the MCP modal's rows.
    let reply = dispatch(
        ClientMsg::McpRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the row's description is the launch summary.
    let [ServerMsg::McpRows(body)] = reply.frames.as_slice() else {
        unreachable!("one mcp_rows frame expected: {reply:?}");
    };
    assert_eq!(body.rows[0].description, "jinn luau serve a.lua");
}

#[rstest::rstest]
#[tokio::test]
async fn mcp_rows_with_no_configured_servers_answer_no_rows() {
    // Given a config holding no servers.
    let h = harness();

    // When the page asks for the MCP modal's rows.
    let reply = dispatch(
        ClientMsg::McpRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the frame carries no rows and is not a refusal.
    assert!(mcp_names(&reply).is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn mcp_rows_match_the_shared_filter() {
    // Given a config holding two servers.
    let h = harness_with_mcp(&[
        ("echo", "jinn luau serve a.lua"),
        ("weather", "weather-server"),
    ]);

    // When the page asks for the rows under a filter only one matches.
    let reply = dispatch(
        ClientMsg::McpRows {
            filter: "wea".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then only the matching server is listed.
    assert_eq!(mcp_names(&reply), ["weather"]);
}

#[rstest::rstest]
#[tokio::test]
async fn mcp_rows_mark_a_disabled_server_off_whatever_the_cell_reports() {
    // Given a config holding one server the session has not enabled, whose
    // runtime cell reports a connection still running.
    let h = harness_with_mcp(&[("echo", "jinn luau serve a.lua")]);
    let session_id = active_id(&h.state);
    h.handle.mcp_runtime_cell().update(|runtime| {
        runtime.set_status(
            &session_id,
            "echo",
            jinn_mcp_msg::McpConnectionStatus::Running,
        )
    });

    // When the page asks for the MCP modal's rows.
    let reply = dispatch(
        ClientMsg::McpRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the badge reads `off` — the stale report does not surface.
    let [ServerMsg::McpRows(body)] = reply.frames.as_slice() else {
        unreachable!("one mcp_rows frame expected: {reply:?}");
    };
    assert_eq!(body.rows[0].badge, jinn_webui2_msg::McpBadgeBody::Off);
}

#[rstest::rstest]
#[tokio::test]
async fn mcp_rows_mark_an_enabled_server_running_when_the_cell_says_so() {
    // Given a config holding one server the session enabled, whose runtime
    // cell reports the connection running.
    let h = harness_with_mcp(&[("echo", "jinn luau serve a.lua")]);
    let session_id = active_id(&h.state);
    h.state
        .write()
        .session
        .active_session_mut()
        .set_enabled_mcp_servers(std::collections::BTreeSet::from(["echo".to_owned()]));
    h.handle.mcp_runtime_cell().update(|runtime| {
        runtime.set_status(
            &session_id,
            "echo",
            jinn_mcp_msg::McpConnectionStatus::Running,
        )
    });

    // When the page asks for the MCP modal's rows.
    let reply = dispatch(
        ClientMsg::McpRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the badge reads `running`.
    let [ServerMsg::McpRows(body)] = reply.frames.as_slice() else {
        unreachable!("one mcp_rows frame expected: {reply:?}");
    };
    assert_eq!(body.rows[0].badge, jinn_webui2_msg::McpBadgeBody::Running);
}

#[rstest::rstest]
#[tokio::test]
async fn mcp_rows_read_a_singletons_status_for_any_session() {
    // Given a config holding one singleton-like server the session enabled,
    // whose runtime cell reports it running under the singleton bucket.
    let h = harness_with_mcp(&[("echo", "jinn luau serve a.lua")]);
    h.state
        .write()
        .session
        .active_session_mut()
        .set_enabled_mcp_servers(std::collections::BTreeSet::from(["echo".to_owned()]));
    h.handle.mcp_runtime_cell().update(|runtime| {
        runtime.set_singleton_status("echo", jinn_mcp_msg::McpConnectionStatus::Running)
    });

    // When the page asks for the MCP modal's rows.
    let reply = dispatch(
        ClientMsg::McpRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the badge reads `running`, not a per-session `starting`.
    let [ServerMsg::McpRows(body)] = reply.frames.as_slice() else {
        unreachable!("one mcp_rows frame expected: {reply:?}");
    };
    assert_eq!(body.rows[0].badge, jinn_webui2_msg::McpBadgeBody::Running);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_a_disabled_server_enables_it() {
    // Given a config holding one server the session has not enabled.
    let h = harness_with_mcp(&[("echo", "jinn luau serve a.lua")]);

    // When the page toggles it.
    say(
        &h,
        ClientMsg::ToggleMcp {
            server: "echo".to_owned(),
        },
    )
    .await;

    // Then the session's enabled set names it.
    let guard = h.state.read();
    assert!(
        guard
            .session
            .active_session()
            .enabled_mcp_servers()
            .contains("echo")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_an_enabled_server_disables_it() {
    // Given a config holding one server the session already enabled.
    let h = harness_with_mcp(&[("echo", "jinn luau serve a.lua")]);
    h.state
        .write()
        .session
        .active_session_mut()
        .set_enabled_mcp_servers(std::collections::BTreeSet::from(["echo".to_owned()]));

    // When the page toggles it.
    say(
        &h,
        ClientMsg::ToggleMcp {
            server: "echo".to_owned(),
        },
    )
    .await;

    // Then the session's enabled set no longer names it.
    let guard = h.state.read();
    assert!(
        !guard
            .session
            .active_session()
            .enabled_mcp_servers()
            .contains("echo")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_a_server_publishes_the_full_desired_set() {
    // Given a config holding two servers, one already enabled.
    let h = harness_with_mcp(&[
        ("echo", "jinn luau serve a.lua"),
        ("weather", "weather-server"),
    ]);
    h.state
        .write()
        .session
        .active_session_mut()
        .set_enabled_mcp_servers(std::collections::BTreeSet::from(["echo".to_owned()]));

    // When the page toggles the other one on.
    say(
        &h,
        ClientMsg::ToggleMcp {
            server: "weather".to_owned(),
        },
    )
    .await;

    // Then one message named the full desired set — both servers.
    let published = h.audit.of_type::<jinn_mcp_msg::McpEnablementChanged>();
    assert_eq!(published.len(), 1);
    assert_eq!(
        published[0].enabled,
        std::collections::BTreeSet::from(["echo".to_owned(), "weather".to_owned()])
    );
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_a_server_publishes_nothing_else() {
    // Given a config holding one server.
    let h = harness_with_mcp(&[("echo", "jinn luau serve a.lua")]);

    // When the page toggles it.
    say(
        &h,
        ClientMsg::ToggleMcp {
            server: "echo".to_owned(),
        },
    )
    .await;

    // Then the only message published is the enablement change — no persist.
    assert_eq!(h.audit.names(), ["McpEnablementChanged"]);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_an_unknown_server_is_refused() {
    // Given a config holding one server.
    let h = harness_with_mcp(&[("echo", "jinn luau serve a.lua")]);

    // When the page toggles a server the config does not hold.
    let reply = dispatch(
        ClientMsg::ToggleMcp {
            server: "phantom".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told so, nothing was published, and the enabled set
    // is untouched.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        unreachable!("one error frame expected: {reply:?}");
    };
    assert_eq!(error.message, "unknown mcp server");
    assert_eq!(h.audit.names(), Vec::<&str>::new());
    assert!(
        h.state
            .read()
            .session
            .active_session()
            .enabled_mcp_servers()
            .is_empty()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_runtime_store_pushes_rows_in_the_last_asked_filter() {
    // Given a config holding one server the session enabled, whose modal
    // last asked for rows under a filter, and a connected page.
    let h = harness_with_mcp(&[("echo", "jinn luau serve a.lua")]);
    h.state
        .write()
        .session
        .active_session_mut()
        .set_enabled_mcp_servers(std::collections::BTreeSet::from(["echo".to_owned()]));
    let mut inbox = h.handle.register();
    dispatch(
        ClientMsg::McpRows {
            filter: "ec".to_owned(),
        },
        &h.handle,
    )
    .await;
    drain(&mut inbox);

    // When the runtime cell is written and the coordinator's stored event
    // is handled.
    let session_id = active_id(&h.state);
    h.handle.mcp_runtime_cell().update(|runtime| {
        runtime.set_status(
            &session_id,
            "echo",
            jinn_mcp_msg::McpConnectionStatus::Running,
        )
    });
    crate::outbound::mcp_runtime_stored(&h.handle);

    // Then the page receives fresh rows, in the filter it last asked, with
    // the badge now running.
    let frames = drain(&mut inbox);
    let [ServerMsg::McpRows(body)] = frames.as_slice() else {
        unreachable!("one mcp_rows frame expected: {frames:?}");
    };
    assert_eq!(body.filter, "ec");
    assert_eq!(body.rows[0].badge, jinn_webui2_msg::McpBadgeBody::Running);
}

#[rstest::rstest]
#[tokio::test]
async fn a_runtime_store_for_a_session_with_no_mcp_modal_pushes_nothing() {
    // Given a config holding one server, and a page whose modal never asked.
    let h = harness_with_mcp(&[("echo", "jinn luau serve a.lua")]);
    let mut inbox = h.handle.register();

    // When the stored event is handled.
    crate::outbound::mcp_runtime_stored(&h.handle);

    // Then nothing was pushed.
    assert!(drain(&mut inbox).is_empty());
}

// ---------------------------------------------------------------------------
// Persona rows / switch
// ---------------------------------------------------------------------------

/// A harness with the given personas in the catalog.
fn harness_with_personas(personas: &[(&'static str, &'static str, bool)]) -> Harness {
    harness_seeded(
        empty_providers(),
        Arc::new(jinn_kernel::common::services::test_services::FakeSessionStore),
        Seed {
            personas: personas.to_vec(),
            ..Seed::default()
        },
    )
}

/// The persona names one rows reply carries, in order.
fn persona_names(reply: &crate::protocol::Reply) -> Vec<String> {
    let [ServerMsg::PersonaRows(body)] = reply.frames.as_slice() else {
        unreachable!("one persona_rows frame expected: {reply:?}");
    };
    body.rows.iter().map(|row| row.name.clone()).collect()
}

#[rstest::rstest]
#[tokio::test]
async fn persona_rows_follow_the_catalogs_order() {
    // Given a catalog seeded in a non-alphabetical order. The catalog is a
    // name-keyed map, so its own order is alphabetical whatever the seed
    // order; the modal must follow the catalog, not re-sort.
    let h = harness_with_personas(&[
        ("brainstorm", "Explores the problem.", true),
        ("reviewer", "Reviews code.", true),
        ("planner", "Plans the work.", true),
    ]);

    // When the page asks for the persona modal's rows.
    let reply = dispatch(
        ClientMsg::PersonaRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then all three are listed, in the catalog's order.
    assert_eq!(persona_names(&reply), ["brainstorm", "planner", "reviewer"]);
}

#[rstest::rstest]
#[tokio::test]
async fn persona_rows_mark_the_sessions_persona_active() {
    // Given a catalog holding two personas, one of them the session's.
    let h = harness_with_personas(&[
        ("brainstorm", "Explores the problem.", true),
        ("reviewer", "Reviews code.", true),
    ]);
    h.state
        .write()
        .session
        .active_session_mut()
        .set_persona_name("reviewer".to_owned());

    // When the page asks for the persona modal's rows.
    let reply = dispatch(
        ClientMsg::PersonaRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then exactly that one is marked active.
    let [ServerMsg::PersonaRows(body)] = reply.frames.as_slice() else {
        unreachable!("one persona_rows frame expected: {reply:?}");
    };
    let active: Vec<&str> = body
        .rows
        .iter()
        .filter(|row| row.is_active)
        .map(|row| row.name.as_str())
        .collect();
    assert_eq!(active, ["reviewer"]);
}

#[rstest::rstest]
#[tokio::test]
async fn persona_rows_match_the_shared_filter() {
    // Given a catalog holding two personas.
    let h = harness_with_personas(&[
        ("brainstorm", "Explores the problem.", true),
        ("reviewer", "Reviews code.", true),
    ]);

    // When the page asks for the rows under a filter only one matches.
    let reply = dispatch(
        ClientMsg::PersonaRows {
            filter: "revie".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then only the matching persona is listed.
    assert_eq!(persona_names(&reply), ["reviewer"]);
}

#[rstest::rstest]
#[tokio::test]
async fn switching_persona_writes_the_sessions_persona() {
    // Given a catalog holding two personas.
    let h = harness_with_personas(&[
        ("brainstorm", "Explores the problem.", true),
        ("reviewer", "Reviews code.", true),
    ]);

    // When the page switches to one.
    say(
        &h,
        ClientMsg::SwitchPersona {
            persona: "reviewer".to_owned(),
        },
    )
    .await;

    // Then the session's persona is that one.
    let guard = h.state.read();
    assert_eq!(guard.session.active_session().persona_name(), "reviewer");
}

#[rstest::rstest]
#[tokio::test]
async fn switching_persona_publishes_the_terminals_two_messages_in_order() {
    // Given a catalog holding one persona with a handoff block.
    let h = harness_with_personas(&[("reviewer", "Reviews code.", true)]);

    // When the page switches to it.
    say(
        &h,
        ClientMsg::SwitchPersona {
            persona: "reviewer".to_owned(),
        },
    )
    .await;

    // Then the two messages are published in the terminal's order.
    assert_eq!(h.audit.names(), ["UpdateAppState", "MarkSessionInteracted"]);
}

#[rstest::rstest]
#[tokio::test]
async fn switching_persona_with_a_handoff_block_appends_no_entry() {
    // Given a catalog holding one persona that carries a handoff block.
    let h = harness_with_personas(&[("reviewer", "Reviews code.", true)]);

    // When the page switches to it.
    let reply = dispatch(
        ClientMsg::SwitchPersona {
            persona: "reviewer".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then nothing was appended, and the reply carries no upsert.
    assert!(h.state.read().session.active_session().history().is_empty());
    assert!(reply.frames.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn switching_to_a_blockless_persona_appends_the_shared_error_message() {
    // Given a catalog holding one persona with no handoff block.
    let h = harness_with_personas(&[("blockless", "No block here.", false)]);

    // When the page switches to it.
    say(
        &h,
        ClientMsg::SwitchPersona {
            persona: "blockless".to_owned(),
        },
    )
    .await;

    // Then one error entry carries the shared message.
    let guard = h.state.read();
    let history: &[ChatEntry] = guard.session.active_session().history();
    let [entry] = history else {
        unreachable!("one entry expected: {history:?}");
    };
    assert!(matches!(entry.kind, ChatEntryKind::Error(_)));
    let ChatEntryKind::Error(text) = &entry.kind else {
        unreachable!("an error was expected");
    };
    assert_eq!(
        text.as_str(),
        jinn_persona_msg::missing_compaction_block_message("blockless")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn switching_to_a_blockless_persona_delivers_the_entry_in_the_reply() {
    // Given a catalog holding one persona with no handoff block.
    let h = harness_with_personas(&[("blockless", "No block here.", false)]);

    // When the page switches to it.
    let reply = dispatch(
        ClientMsg::SwitchPersona {
            persona: "blockless".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the reply carries the entry's upsert.
    let [ServerMsg::EntryUpsert(upsert)] = reply.frames.as_slice() else {
        unreachable!("one upsert frame expected: {reply:?}");
    };
    assert_eq!(upsert.index, 0);
    assert_eq!(upsert.entry.kind, "error");
}

#[rstest::rstest]
#[tokio::test]
async fn switching_to_an_unknown_persona_is_refused() {
    // Given a catalog holding one persona.
    let h = harness_with_personas(&[("reviewer", "Reviews code.", true)]);

    // When the page switches to one the catalog does not hold.
    let reply = dispatch(
        ClientMsg::SwitchPersona {
            persona: "phantom".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told so, and nothing was written, appended, or
    // published.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        unreachable!("one error frame expected: {reply:?}");
    };
    assert_eq!(error.message, "unknown persona");
    assert!(h.state.read().session.active_session().history().is_empty());
    assert_eq!(h.audit.names(), Vec::<&str>::new());
}
