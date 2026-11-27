#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable,
    reason = "test assertions need expect, indexed access to a recorded audit, a let-else on a known variant is unreachable by construction, and a let-else that does fall through reports with panic"
)]

//! Behaviour of the inbound dispatch: one test, one observable result.
//!
//! Every test drives a `ClientMsg` through `dispatch` against a handle
//! built over a recording bus, then asserts either the messages published
//! (by name, in order — the terminal's own sequence) or the frames
//! replied to the page.

use std::sync::Arc;

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_context_assembly_msg::ContextOverrideChanged;
use jinn_core_types::{
    ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride, ModelSelection, PinPosition,
};
use jinn_inference_msg::CancelTurn;
use jinn_kernel::State;
use jinn_preferences_config::protocol::app_state_command::{AppStateUpdate, UpdateAppState};
use jinn_provider_selection_msg::ProviderSwitch;
use jinn_session_history_msg::{
    ChatEntryPinChanged, PinChatEntry, SetEntryContextOverride, UnpinChatEntry,
};
use jinn_session_lifecycle_msg::{CloseSession, SessionTeardownFinished, SessionTeardownStarted};
use jinn_session_msg::MarkSessionInteracted;
use jinn_session_store_msg::{
    ArchiveSession, PersistSession, SessionForkRequested, SessionLoadRequested, SetSessionLibrary,
};
use jinn_slices::bus::BusService;
use jinn_webui2_msg::{ClientMsg, EntryEnvelope, ServerMsg};

use crate::{
    actions,
    connections::ConnectionRegistry,
    outbound::OutboundHandle,
    protocol::{Reply, dispatch},
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
    /// The library membership each stored session carries. A no-op
    /// `set_in_library` and empty summaries would let a library test pass
    /// without proving anything, so the store records the flag and reports
    /// it back, the way the real one does.
    library: std::sync::Mutex<std::collections::HashMap<jinn_core_types::SessionId, bool>>,
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

    /// One summary per stored session, carrying the library flag the
    /// store holds for it — so `library_members()` filters real rows.
    async fn load_summaries(
        &self,
    ) -> Result<
        Vec<jinn_session_store_msg::SessionSummary>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        let library = self.library.lock().expect("library lock");
        Ok(self
            .snapshots()
            .into_iter()
            .map(|snapshot| jinn_session_store_msg::SessionSummary {
                session_id: snapshot.metadata.session_id.clone(),
                title: snapshot.metadata.title.clone().unwrap_or_default(),
                updated_at: snapshot.metadata.updated_at,
                created_at: snapshot.metadata.created_at,
                session_state: jinn_session_store_msg::SessionState::Loaded,
                in_library: *library.get(&snapshot.metadata.session_id).unwrap_or(&false),
                is_bot: false,
                parent_session: None,
                project: None,
            })
            .collect())
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
        session_id: &jinn_core_types::SessionId,
        in_library: bool,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        self.library
            .lock()
            .expect("library lock")
            .insert(session_id.clone(), in_library);
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
    harness_seeded(providers, store, &Seed::default())
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
    /// Saved applets placed in the applets catalog cell, as `(id, title)`
    /// in the order given — the store actor's recency order is the test's
    /// to arrange.
    applets: Vec<(&'static str, &'static str)>,
    /// The applets server's serving phase folded into the picker cell: the
    /// resolved bind while serving, `None` while offline (the default).
    applets_serving: Option<&'static str>,
    /// Projects placed in `[[project.entry]]`.
    projects: Vec<jinn_preferences_config::schemas::ProjectConfig>,
    /// Lifecycles placed in `[[session_lifecycle.script]]`.
    lifecycles: Vec<jinn_preferences_config::schemas::SessionLifecycle>,
}

/// A harness over a registry, a session store, and a pre-seeded set of
/// cells.
fn harness_seeded(
    providers: jinn_provider_config::ProvidersConfig,
    store: Arc<dyn jinn_session_state::SessionStore>,
    seed: &Seed,
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
    let applets_catalog_cell = slices
        .reader::<jinn_applets_msg::AppletsCatalog>(&jinn_applets_msg::applets_catalog_slot())
        .expect("the catalog registers the applets catalog slot");
    let applets_picker_cell = slices
        .reader::<jinn_applets_msg::AppletsPickerState>(&jinn_applets_msg::applets_picker_slot())
        .expect("the catalog registers the applets picker slot");

    seed_personas(&personas_cell, &seed.personas);
    seed_tools(&registry_cell, &seed.tools);
    seed_applets(
        &applets_catalog_cell,
        &applets_picker_cell,
        &seed.applets,
        seed.applets_serving,
    );
    // One services: the config the seed writes is the config the handle
    // reads. A second `TestServices` would hand the handle a different
    // layer and every MCP row would read as unconfigured.
    let services = jinn_kernel::TestServices::builder()
        .providers(providers)
        .build();
    seed_mcp_servers(&services.config, &seed.mcp_servers);
    if !seed.projects.is_empty() {
        services
            .config
            .put_list::<jinn_preferences_config::schemas::ProjectConfig>(&seed.projects)
            .expect("the test config accepts the project list");
    }
    if !seed.lifecycles.is_empty() {
        services
            .config
            .put_list::<jinn_preferences_config::schemas::SessionLifecycle>(&seed.lifecycles)
            .expect("the test config accepts the lifecycle list");
    }

    let handle = OutboundHandle::new(crate::outbound::OutboundActorDeps {
        state: state.clone(),
        bus,
        connections: ConnectionRegistry::default(),
        revisions: RevisionTracker::default(),
        settles: crate::settle::SettleTracker::default(),
        session_store: jinn_session_state::SessionStoreService::new(store),
        config: services.config.clone(),
        services,
        provider_cell,
        personas_cell,
        mcp_runtime_cell,
        applets_catalog_cell,
        applets_picker_cell,
    });
    Harness {
        handle,
        audit,
        state,
    }
}

/// Seeds the applets catalog cell with the given entries, in the order
/// given, and folds the serving phase into the picker cell — the two writes
/// the applets slice's own actors perform, done here by hand because no
/// applets slice runs under the harness.
fn seed_applets(
    catalog_cell: &jinn_slices::TypedCell<jinn_applets_msg::AppletsCatalog>,
    picker_cell: &jinn_slices::TypedCell<jinn_applets_msg::AppletsPickerState>,
    applets: &[(&'static str, &'static str)],
    serving: Option<&str>,
) {
    catalog_cell.update(|cell| {
        cell.entries = applets
            .iter()
            .map(|(id, title)| jinn_applets_msg::AppletEntry {
                id: (*id).to_owned(),
                title: (*title).to_owned(),
                created_at: String::new(),
            })
            .collect();
    });
    picker_cell.update(|cell| cell.apply_serving(serving.is_some(), serving.unwrap_or("")));
}

/// A harness with the given saved applets in the catalog and the applets
/// server serving at `bind` (`None` for offline).
fn harness_with_applets(
    applets: &[(&'static str, &'static str)],
    serving: Option<&'static str>,
) -> Harness {
    harness_seeded(
        empty_providers(),
        Arc::new(jinn_kernel::common::services::test_services::FakeSessionStore),
        &Seed {
            applets: applets.to_vec(),
            applets_serving: serving,
            ..Seed::default()
        },
    )
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

/// The seeded personas as catalog entries, name-sorted — the order the
/// real scan produces, so a seeded catalog reads like a scanned one.
fn personas_entries(entries: &[(&str, &str, bool)]) -> Vec<jinn_slices::Persona> {
    let mut personas: Vec<jinn_slices::Persona> = entries
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
        .collect();
    personas.sort_by(|a, b| a.name.cmp(&b.name));
    personas
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

/// Records the active session's tail as delivered — what a snapshot reply
/// does for the page it answers.
fn mark_tail_delivered(h: &Harness) {
    let session_id = active_id(&h.state);
    let window = {
        let guard = h.state.read();
        let session = guard.session.active_session();
        crate::page::tail(session.history())
    };
    h.handle.mark_delivered(&session_id, &window);
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
            images: Vec::new(),
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
            images: Vec::new(),
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
            images: Vec::new(),
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
            images: Vec::new(),
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
            images: Vec::new(),
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
            images: Vec::new(),
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
            images: Vec::new(),
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
async fn hello_with_a_cursor_at_the_newest_entry_answers_current() {
    // Given a page that saw the only entry at the current revision, and
    // nothing has arrived since.
    let h = harness();
    push(&h.state, ChatEntry::user("one"));
    let cursor = active_last_entry_id(&h.state);
    let revision = h.handle.revision(&active_id(&h.state));

    // When it reconnects naming both.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: Some(cursor),
            revision: Some(revision),
        },
        &h.handle,
    )
    .await;

    // Then the answer is the "current" frame alone, never silence.
    let [ServerMsg::Current] = reply.frames.as_slice() else {
        unreachable!("a hello that missed nothing answers current");
    };
}

#[rstest::rstest]
#[tokio::test]
async fn hello_with_a_gap_answers_upserts_and_no_current_frame() {
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

    // Then the answer is exactly the missed entry, with no "current" frame.
    let [ServerMsg::EntryUpsert(_)] = reply.frames.as_slice() else {
        unreachable!("a hello with a gap answers one upsert per missed entry");
    };
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

#[rstest::rstest]
#[tokio::test]
async fn hello_snapshot_reports_hydration_while_startup_loads_are_outstanding() {
    // Given an instance whose startup hydration is still running.
    let h = harness();
    h.state.write().session.begin_startup_hydration();

    // When a page connects holding nothing.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: None,
            revision: None,
        },
        &h.handle,
    )
    .await;

    // Then the snapshot says the saved sessions are still loading.
    let [ServerMsg::Snapshot(snapshot)] = reply.frames.as_slice() else {
        unreachable!("a fresh hello answers a snapshot");
    };
    assert!(snapshot.hydrating);
}

#[rstest::rstest]
#[tokio::test]
async fn hello_snapshot_reports_hydration_finished_once_it_is() {
    // Given an instance whose startup hydration is not running.
    let h = harness();

    // When a page connects holding nothing.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: None,
            revision: None,
        },
        &h.handle,
    )
    .await;

    // Then the snapshot says nothing is loading.
    let [ServerMsg::Snapshot(snapshot)] = reply.frames.as_slice() else {
        unreachable!("a fresh hello answers a snapshot");
    };
    assert!(!snapshot.hydrating);
}

/// Pushes `count` user entries onto the active session, so its history
/// runs past one page.
fn push_many(state: &State, count: usize) {
    for i in 0..count {
        push(state, ChatEntry::user(format!("entry {i}")));
    }
}

#[rstest::rstest]
#[tokio::test]
async fn hello_on_a_fresh_page_carries_the_newest_entries_up_to_one_page() {
    // Given a session whose history runs past one page.
    let h = harness();
    push_many(&h.state, jinn_webui2_msg::TAIL_MAX_ENTRIES + 50);

    // When a page connects holding nothing.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: None,
            revision: None,
        },
        &h.handle,
    )
    .await;

    // Then the snapshot carries one page of the newest entries.
    let [ServerMsg::Snapshot(snapshot)] = reply.frames.as_slice() else {
        unreachable!("a fresh hello answers a snapshot");
    };
    assert_eq!(snapshot.entries.len(), jinn_webui2_msg::TAIL_MAX_ENTRIES);
    assert_eq!(snapshot.entries_before, 50);
}

#[rstest::rstest]
#[tokio::test]
async fn hello_on_a_fresh_page_carries_the_oldest_entry_of_the_tail_at_index_zero_of_the_frame() {
    // Given a session whose history runs past one page.
    let h = harness();
    push_many(&h.state, jinn_webui2_msg::TAIL_MAX_ENTRIES + 50);

    // When a page connects holding nothing.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: None,
            revision: None,
        },
        &h.handle,
    )
    .await;

    // Then the frame's first entry is history entry 50, and its last is the newest.
    let [ServerMsg::Snapshot(snapshot)] = reply.frames.as_slice() else {
        unreachable!("a fresh hello answers a snapshot");
    };
    let texts = snapshot
        .entries
        .iter()
        .map(EntryEnvelope::entry_text)
        .collect::<Vec<_>>();
    assert_eq!(texts.first().copied(), Some("entry 50"));
    assert_eq!(texts.last().copied(), Some("entry 249"));
}

#[rstest::rstest]
#[tokio::test]
async fn hello_with_a_cursor_more_than_one_page_behind_answers_a_snapshot() {
    // Given a page that saw the first entry, while the session grew past a page since.
    let h = harness();
    push(&h.state, ChatEntry::user("one"));
    let cursor = active_last_entry_id(&h.state);
    let revision = h.handle.revision(&active_id(&h.state));
    push_many(&h.state, jinn_webui2_msg::TAIL_MAX_ENTRIES + 1);

    // When it reconnects naming the current revision and a cursor now more than a page old.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: Some(cursor),
            revision: Some(revision),
        },
        &h.handle,
    )
    .await;

    // Then it receives the bounded tail, never a replay of the gap.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Snapshot(_)]));
}

// ---------------------------------------------------------------------------
// Older pages
// ---------------------------------------------------------------------------

/// Sends a `MoreEntries` for the oldest entry the page holds.
async fn more_before(h: &Harness, before: &jinn_core_types::ChatEntryId) -> Reply {
    dispatch(
        ClientMsg::MoreEntries {
            before: before.clone(),
        },
        &h.handle,
    )
    .await
}

/// The texts of one page reply's entries, in history order, together with
/// how many entries precede the page.
fn page_texts(reply: &Reply) -> (Vec<&str>, usize) {
    let [ServerMsg::EntriesPage(page)] = reply.frames.as_slice() else {
        unreachable!("a MoreEntries reply is one EntriesPage");
    };
    let texts = page.entries.iter().map(EntryEnvelope::entry_text).collect();
    (texts, page.entries_before)
}

#[rstest::rstest]
#[tokio::test]
async fn more_entries_returns_the_page_before_the_oldest_held_entry() {
    // Given a page that holds the tail of a 250-entry history.
    let h = harness();
    push_many(&h.state, jinn_webui2_msg::TAIL_MAX_ENTRIES + 50);

    // When it asks for the entries before history entry 50, the tail's oldest.
    let before = h.state.read().session.active_session().history()[50]
        .id
        .clone();
    let reply = more_before(&h, &before).await;

    // Then it receives entries 0–49, with nothing before them.
    let (texts, before_count) = page_texts(&reply);
    assert_eq!(texts.first().copied(), Some("entry 0"));
    assert_eq!(texts.last().copied(), Some("entry 49"));
    assert_eq!(before_count, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn more_entries_names_how_many_entries_precede_the_page() {
    // Given a page that holds the tail of a 650-entry history.
    let h = harness();
    push_many(&h.state, 3 * jinn_webui2_msg::TAIL_MAX_ENTRIES + 50);

    // When it asks for the entries before history entry 450, the tail's oldest.
    let before = h.state.read().session.active_session().history()[450]
        .id
        .clone();
    let reply = more_before(&h, &before).await;

    // Then the page says 250 entries precede it.
    let (_, before_count) = page_texts(&reply);
    assert_eq!(before_count, 250);
}

#[rstest::rstest]
#[tokio::test]
async fn more_entries_is_refused_for_the_sessions_first_entry() {
    // Given a session whose first entry is what the page names.
    let h = harness();
    push_many(&h.state, 3);

    // When the page asks for entries before history entry 0.
    let first = h.state.read().session.active_session().history()[0]
        .id
        .clone();
    let reply = more_before(&h, &first).await;

    // Then the answer is a refusal, not a page.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        panic!("expected a refusal, got {:?}", reply.frames);
    };
    assert_eq!(error.message, "no older entries");
}

#[rstest::rstest]
#[tokio::test]
async fn more_entries_is_refused_for_an_entry_of_another_session() {
    // Given a session, and an entry that belongs to no session it holds.
    let h = harness();
    push_many(&h.state, 3);

    // When the page asks for entries before that foreign id.
    let reply = more_before(&h, &jinn_core_types::ChatEntryId::new()).await;

    // Then the answer is a refusal.
    let [ServerMsg::Error(_)] = reply.frames.as_slice() else {
        panic!("expected a refusal, got {:?}", reply.frames);
    };
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

/// A summary forked from `parent`, so the tree nests it.
fn child_of(
    parent: &jinn_core_types::SessionId,
    title: &str,
) -> jinn_session_store_msg::SessionSummary {
    let mut summary = summary(title);
    summary.parent_session = Some(parent.clone());
    summary
}

/// A summary last modified at a given Unix-second offset, so tree order
/// can be pinned rather than left to equal timestamps.
/// A summary with a chosen id, so a child's `parent_session` can name a
/// row that really is in the list.
fn with_id(id: jinn_core_types::SessionId, title: &str) -> jinn_session_store_msg::SessionSummary {
    let mut summary = summary(title);
    summary.session_id = id;
    summary
}

fn at(
    mut summary: jinn_session_store_msg::SessionSummary,
    secs: i64,
) -> jinn_session_store_msg::SessionSummary {
    summary.updated_at = jiff::Timestamp::from_second(secs).expect("a valid second");
    summary
}

/// Builds the tree entries the way the picker's handler does, then asks
/// for the rows a filter yields.
fn tree_rows(
    summaries: Vec<jinn_session_store_msg::SessionSummary>,
    filter: &str,
) -> Vec<jinn_webui2_msg::PickerRowBody> {
    let entries =
        jinn_session_store_msg::session_tree_entries(summaries, &jinn_theme::default_theme());
    crate::actions::session_tree_rows(entries, filter)
}

fn titles(rows: &[jinn_webui2_msg::PickerRowBody]) -> Vec<&str> {
    rows.iter().map(|row| row.title.as_str()).collect()
}

fn depths(rows: &[jinn_webui2_msg::PickerRowBody]) -> Vec<usize> {
    rows.iter().map(|row| row.depth).collect()
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
    let rows = tree_rows(stored, "jinn");

    // Then only the jinn titles remain, strongest match first.
    assert_eq!(titles(&rows), ["jinn work", "jinn plans"]);
}

#[rstest::rstest]
#[test]
fn picker_ranks_the_closer_title_match_first() {
    // Given a title that starts with the filter and one that merely contains it.
    let stored = vec![summary("my jinn notes"), summary("jinn work")];

    // When the picker is filtered to "jinn".
    let rows = tree_rows(stored, "jinn");

    // Then the prefix match ranks first.
    assert_eq!(titles(&rows), ["jinn work", "my jinn notes"]);
}

#[rstest::rstest]
#[test]
fn picker_filter_with_a_space_ands_each_term() {
    // Given three sessions, only one of which holds both words.
    let stored = vec![
        summary("jinn work"),
        summary("jinn plans"),
        summary("garden work"),
    ];

    // When the picker is filtered to "jinn work".
    let rows = tree_rows(stored, "jinn work");

    // Then only the session holding both terms is listed.
    assert_eq!(titles(&rows), ["jinn work"]);
}

#[rstest::rstest]
#[test]
fn picker_with_an_empty_filter_keeps_every_session() {
    // Given three stored sessions.
    let stored = vec![summary("one"), summary("two"), summary("three")];

    // When the picker is unfiltered.
    let rows = tree_rows(stored, "");

    // Then all three are listed.
    assert_eq!(rows.len(), 3);
}

#[rstest::rstest]
#[test]
fn picker_row_carries_the_stored_state_and_parent() {
    // Given an archived session forked from another.
    let parent = jinn_core_types::SessionId::new();
    let mut archived = child_of(&parent, "child");
    archived.session_state = jinn_session_store_msg::SessionState::Archived;

    // When the picker lists it.
    let rows = tree_rows(vec![summary("parent"), archived], "");

    // Then the row says so.
    assert!(matches!(
        rows.as_slice(),
        [_, jinn_webui2_msg::PickerRowBody { state, parent_id: Some(p), .. }]
            if *state == jinn_session_store_msg::SessionState::Archived && *p == parent
    ));
}

#[rstest::rstest]
#[test]
fn picker_nests_a_forked_session_under_its_parent() {
    // Given a session forked from another.
    let parent = jinn_core_types::SessionId::new();
    let child = child_of(&parent, "the child");

    // When the picker is unfiltered.
    let rows = tree_rows(vec![child, with_id(parent, "the parent")], "");

    // Then the child is one level under a root, right after its parent.
    assert_eq!(titles(&rows), ["the parent", "the child"]);
    assert_eq!(depths(&rows), [0, 1]);
}

#[rstest::rstest]
#[test]
fn picker_keeps_the_parent_of_a_child_only_match() {
    // Given a child whose title alone matches the filter, under a parent
    // whose title does not.
    let parent = jinn_core_types::SessionId::new();
    let child = child_of(&parent, "needle in a child");
    let mut parent_summary = summary("unrelated parent");
    parent_summary.session_id = parent.clone();

    // When the picker is filtered to the child's word.
    let rows = tree_rows(vec![parent_summary, child], "needle");

    // Then the parent is listed above its matching child, unhighlighted.
    assert_eq!(titles(&rows), ["unrelated parent", "needle in a child"]);
    assert_eq!(depths(&rows), [0, 1]);
    assert!(rows[0].title_runs.iter().all(|run| !run.matched));
}

#[rstest::rstest]
#[test]
fn picker_marks_the_letters_the_filter_hit_in_the_title() {
    // Given one stored session.
    let stored = vec![summary("jinn work")];

    // When the picker is filtered to "work".
    let rows = tree_rows(stored, "work");

    // Then exactly that word is the matched run.
    assert_eq!(
        rows[0]
            .title_runs
            .iter()
            .map(|run| (run.text.as_str(), run.matched))
            .collect::<Vec<_>>(),
        [("jinn ", false), ("work", true)]
    );
}

#[rstest::rstest]
#[test]
fn picker_title_runs_cut_on_grapheme_boundaries() {
    // Given a title with a multi-byte character the filter straddles.
    let stored = vec![summary("café notes")];

    // When the picker is filtered to "café".
    let rows = tree_rows(stored, "café");

    // Then the runs reassemble the title byte for byte.
    let joined: String = rows[0]
        .title_runs
        .iter()
        .map(|run| run.text.as_str())
        .collect();
    assert_eq!(joined, "café notes");
}

#[rstest::rstest]
#[test]
fn picker_ranks_the_tree_with_the_strongest_match_first() {
    // Given two trees whose best matches differ in strength, the weaker
    // one modified more recently.
    let parent_a = jinn_core_types::SessionId::new();
    let parent_b = jinn_core_types::SessionId::new();
    let stored = vec![
        at(with_id(parent_b.clone(), "zzz parent"), 20),
        at(child_of(&parent_b, "my jinn notes"), 20),
        at(with_id(parent_a.clone(), "jinn work"), 10),
        at(child_of(&parent_a, "other"), 10),
    ];

    // When the picker is filtered to "jinn".
    let rows = tree_rows(stored, "jinn");

    // Then the tree holding the prefix match ranks first.
    assert_eq!(titles(&rows), ["jinn work", "zzz parent", "my jinn notes"]);
}

#[rstest::rstest]
#[test]
fn picker_cap_keeps_whole_trees() {
    // Given more trees than the cap holds rows for: roots alternating with
    // one child each, so every tree is two rows.
    let stored: Vec<_> = (0..40)
        .flat_map(|i| {
            let parent = jinn_core_types::SessionId::new();
            let root = at(child_of(&parent, &format!("child {i}")), i + 1);
            let mut root_summary = at(summary(&format!("root {i}")), i + 1);
            root_summary.session_id = parent;
            vec![root_summary, root]
        })
        .collect();

    // When the picker is unfiltered.
    let rows = tree_rows(stored, "");

    // Then the cap lands on a tree boundary: 50 rows, and the last row a
    // root, never a parent shown without its child.
    assert_eq!(rows.len(), crate::actions::PICKER_ROW_CAP);
    // And no tree is cut: every depth-1 row is preceded by a root, so
    // every child kept has its parent kept too.
    assert!(
        rows.windows(2)
            .all(|pair| pair[1].depth == 0 || pair[0].depth == 0)
    );
}

#[rstest::rstest]
#[test]
fn picker_cap_cuts_a_single_tree_larger_than_the_cap() {
    // Given one root with more children than the cap holds.
    let parent = jinn_core_types::SessionId::new();
    let mut root = summary("the root");
    root.session_id = parent.clone();
    let stored: Vec<_> = std::iter::once(root)
        .chain((0..60).map(|i| at(child_of(&parent, &format!("child {i}")), i + 1)))
        .collect();

    // When the picker is unfiltered.
    let rows = tree_rows(stored, "");

    // Then the first cap-many rows of the tree are listed.
    assert_eq!(rows.len(), crate::actions::PICKER_ROW_CAP);
    assert_eq!(titles(&rows)[..2], ["the root", "child 59"]);
}

#[rstest::rstest]
#[test]
fn picker_row_carries_the_last_modified_time() {
    // Given a session modified at a known instant.
    let stored = vec![at(summary("timed"), 1_700_000_000)];

    // When the picker lists it.
    let rows = tree_rows(stored, "");

    // Then the row carries the store's own timestamp.
    assert_eq!(
        rows[0].updated_at,
        jiff::Timestamp::from_second(1_700_000_000).expect("a valid second")
    );
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

#[rstest::rstest]
#[tokio::test]
async fn picker_rows_list_an_unarchived_session_once() {
    // Given a store holding one unarchived session — the read that used to
    // list it twice.
    let h = harness_with_store();
    let mut session = jinn_session_state::ChatSessionState::new();
    session.set_title("only once".to_owned());
    h.handle
        .session_store()
        .save(&session.capture_snapshot())
        .await
        .expect("seed the store");

    // When the page asks for the picker's rows.
    let reply = dispatch(
        ClientMsg::PickerRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the session appears exactly once.
    let [ServerMsg::PickerRows(body)] = reply.frames.as_slice() else {
        unreachable!("a picker request answers picker rows");
    };
    assert_eq!(body.rows.len(), 1);
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

/// The first row's model runs, as (text, matched) pairs.
fn model_runs(reply: &crate::protocol::Reply) -> Vec<(String, bool)> {
    let [ServerMsg::ModelRows(body)] = reply.frames.as_slice() else {
        unreachable!("a model-rows request answers model rows");
    };
    body.rows[0]
        .model_runs
        .iter()
        .map(|run| (run.text.clone(), run.matched))
        .collect()
}

/// The first row's provider runs, as (text, matched) pairs.
fn provider_runs(reply: &crate::protocol::Reply) -> Vec<(String, bool)> {
    let [ServerMsg::ModelRows(body)] = reply.frames.as_slice() else {
        unreachable!("a model-rows request answers model rows");
    };
    body.rows[0]
        .provider_runs
        .iter()
        .map(|run| (run.text.clone(), run.matched))
        .collect()
}

/// The text of the matched runs in `runs`, concatenated.
fn matched_text(runs: &[(String, bool)]) -> String {
    runs.iter()
        .filter(|(_, matched)| *matched)
        .map(|(text, _)| text.as_str())
        .collect()
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

#[rstest::rstest]
#[tokio::test]
async fn model_rows_filter_with_a_space_ands_each_term() {
    // Given two configured providers, one whose model and provider share
    // no letters with the other's.
    let h = harness_with_providers(two_providers());

    // When the page filters for a model name and a provider name, separated
    // by a space — the way the terminal picker reads two terms.
    let reply = dispatch(
        ClientMsg::ModelRows {
            filter: "aion alpha".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the row satisfying both terms is offered.
    assert_eq!(model_names(&reply), vec!["aion-alpha"]);
}

#[rstest::rstest]
#[tokio::test]
async fn model_rows_rank_the_closer_match_first() {
    // Given a title-shaped pair: one model starting with the filter, one
    // merely containing it later.
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
    let providers = jinn_provider_config::ProvidersConfig {
        providers: std::collections::BTreeMap::from([
            ("one".to_owned(), entry("gpt-4-turbo")),
            ("two".to_owned(), entry("my-gpt-4")),
        ]),
        aliases: vec![],
        default_provider: None,
        endpoint_defaults: vec![],
    };
    let h = harness_with_providers(providers);

    // When the page filters for "gpt 4".
    let reply = dispatch(
        ClientMsg::ModelRows {
            filter: "gpt 4".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then both match, and the prefix match ranks first.
    assert_eq!(model_names(&reply), vec!["gpt-4-turbo", "my-gpt-4"]);
}

#[rstest::rstest]
#[tokio::test]
async fn model_rows_mark_the_letters_the_filter_hit_in_the_model() {
    // Given two configured providers.
    let h = harness_with_providers(two_providers());

    // When the page filters by part of a model's name.
    let reply = dispatch(
        ClientMsg::ModelRows {
            filter: "chat".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then exactly those letters are the matched run of the model.
    assert_eq!(
        model_runs(&reply),
        vec![("deepseek-".to_owned(), false), ("chat".to_owned(), true),]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn model_rows_mark_the_letters_the_filter_hit_in_the_provider() {
    // Given one provider whose name shares no letters with its model.
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
    let providers = jinn_provider_config::ProvidersConfig {
        providers: std::collections::BTreeMap::from([("zephyr".to_owned(), entry("gpt-4"))]),
        aliases: vec![],
        default_provider: None,
        endpoint_defaults: vec![],
    };
    let h = harness_with_providers(providers);

    // When the page filters by the provider's name.
    let reply = dispatch(
        ClientMsg::ModelRows {
            filter: "zephyr".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the provider's whole name is the matched run, and the model is plain.
    assert_eq!(provider_runs(&reply), vec![("zephyr".to_owned(), true)]);
    assert_eq!(matched_text(&model_runs(&reply)), "");
}

#[rstest::rstest]
#[tokio::test]
async fn model_rows_with_an_empty_filter_carry_plain_runs() {
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

    // Then every run is plain.
    let [ServerMsg::ModelRows(body)] = reply.frames.as_slice() else {
        unreachable!("a model-rows request answers model rows");
    };
    assert!(
        body.rows
            .iter()
            .all(|row| row.model_runs.iter().all(|run| !run.matched))
    );
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
    /// Held so the connection stays registered for the page's life: a
    /// dropped guard unregisters at once, and the flush skips a page
    /// nobody is connected to.
    _connection: crate::connections::ConnectionGuard,
}

fn connected_page() -> Page {
    let h = harness();
    let (_id, inbox, connection) = h.handle.register();
    Page {
        handle: h.handle,
        inbox,
        state: h.state,
        _connection: connection,
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
fn a_refresh_summary_written_without_a_history_event_reaches_the_page() {
    // Given a connected page, shown a session with nothing pending, and then
    // the provider actor writing a refresh summary into history on its own —
    // no history event, the way a real refresh lands.
    let mut page = connected_page();
    let _ = flush_frames(&mut page);
    page.state
        .write()
        .session
        .active_session_mut()
        .push_entry(jinn_core_types::ChatEntry::transient("| ollama | 3 | ✅ |"));

    // When the model cache lands.
    crate::outbound::model_cache_stored(&page.handle);

    // Then the page is sent the summary it was never told about.
    let frames = drain(&mut page.inbox);
    let [ServerMsg::EntryUpsert(upsert)] = frames.as_slice() else {
        panic!("expected one upsert, got {frames:?}");
    };
    // The envelope names the kind itself; the projected entry carries the text.
    assert_eq!(upsert.entry.kind, "transient");
    let text = upsert.entry.entry.as_json()["kind"]["Transient"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains("ollama"), "summary missing: {text}");
}

/// Puts `count` messages behind the running turn, as the session actor does
/// when it receives an enqueue while busy.
fn queue_messages(page: &Page, texts: &[&str]) {
    let mut guard = page.state.write();
    let session = guard.session.active_session_mut();
    for text in texts {
        session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
            jinn_core_types::ChatEntry::user(*text),
        )));
    }
}

#[rstest::rstest]
#[test]
fn a_queued_message_reaches_the_page_as_the_queue_depth_on_the_next_tick() {
    // Given a streaming session with an empty queue, already flushed once.
    let mut page = connected_page();
    begin_streaming(&page);
    let _ = flush_frames(&mut page);

    // When a message is queued behind the running turn.
    queue_messages(&page, &["behind the turn"]);
    let frames = flush_frames(&mut page);

    // Then the page is told the depth is one.
    let [ServerMsg::Phase(phase)] = frames.as_slice() else {
        panic!("expected one phase frame, got {frames:?}");
    };
    assert_eq!(phase.queue_len, 1);
}

#[rstest::rstest]
#[test]
fn two_queued_messages_report_a_depth_of_two() {
    // Given a streaming session with an empty queue, already flushed once.
    let mut page = connected_page();
    begin_streaming(&page);
    let _ = flush_frames(&mut page);

    // When two messages are queued behind the running turn.
    queue_messages(&page, &["first", "second"]);
    let frames = flush_frames(&mut page);

    // Then the page is told the depth is two.
    let depth = frames
        .iter()
        .find_map(|frame| match frame {
            ServerMsg::Phase(phase) => Some(phase.queue_len),
            _ => None,
        })
        .expect("a phase frame");
    assert_eq!(depth, 2);
}

#[rstest::rstest]
#[test]
fn a_tick_with_an_unchanged_queue_sends_no_phase_frame() {
    // Given a streaming session with one queued message, already told to the page.
    let mut page = connected_page();
    begin_streaming(&page);
    queue_messages(&page, &["waiting"]);
    let _ = flush_frames(&mut page);

    // When the tick runs again and nothing about the queue changed.
    let frames = flush_frames(&mut page);

    // Then no phase frame repeats the depth the page already holds.
    assert!(
        frames
            .iter()
            .all(|frame| !matches!(frame, ServerMsg::Phase(_))),
        "unexpected phase frame in {frames:?}"
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
    assert_eq!(frames.len(), 0);
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
    let (_id, mut inbox, _connection) = h.handle.register();
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
    let (_id, mut inbox, _connection) = h.handle.register();
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
    let (_id, mut inbox, _connection) = h.handle.register();
    push(&h.state, ChatEntry::user("held"));
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);

    // When another reconcile runs with the history unchanged.
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));

    // Then the page receives nothing.
    assert_eq!(drain(&mut inbox).len(), 0);
}

#[rstest::rstest]
#[test]
fn an_entry_that_changed_while_settling_is_sent_again() {
    // Given a connected page that holds one entry as it stood before a turn.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();
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
    let (_id, mut inbox, _connection) = h.handle.register();
    push(&h.state, ChatEntry::user("in the snapshot"));
    mark_tail_delivered(&h);
    let _ = drain(&mut inbox);

    // When a reconcile runs with the history unchanged.
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));

    // Then the snapshot's entry is not re-sent.
    assert_eq!(drain(&mut inbox).len(), 0);
}

#[rstest::rstest]
#[test]
fn a_settle_after_a_tail_still_reaches_the_page() {
    // Given a page that holds the tail of a long history, delivered by a snapshot.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();
    push_many(&h.state, jinn_webui2_msg::TAIL_MAX_ENTRIES + 50);
    mark_tail_delivered(&h);
    let _ = drain(&mut inbox);

    // When a turn settles and appends one entry the ticks never streamed.
    push(&h.state, ChatEntry::assistant("settled reply"));
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));

    // Then that entry is the one frame the page receives — not the 50 older ones.
    let frames = drain(&mut inbox);
    assert!(matches!(frames.as_slice(), [ServerMsg::EntryUpsert(_)]));
    let ServerMsg::EntryUpsert(upsert) = &frames[0] else {
        unreachable!("matched above");
    };
    assert_eq!(
        upsert.entry.entry.as_json()["kind"]["Assistant"].as_str(),
        Some("settled reply")
    );
}

// ---------------------------------------------------------------------------
// Pin
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn toggling_an_unpinned_entry_publishes_a_relative_pin() {
    // Given a session holding one unpinned user entry.
    let h = harness();
    push(&h.state, ChatEntry::user("keep me"));
    let entry_id = h.state.read().session.active_session().history()[0]
        .id
        .clone();

    // When the page toggles that entry's pin.
    say(
        &h,
        ClientMsg::TogglePin {
            entry_id: entry_id.clone(),
        },
    )
    .await;

    // Then exactly one pin command is published, at the relative position,
    // naming the entry.
    let pins = h.audit.of_type::<PinChatEntry>();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].entry_id, entry_id);
    assert_eq!(pins[0].position, PinPosition::Relative);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_a_pinned_entry_publishes_an_unpin() {
    // Given a session holding one entry already pinned to the top.
    let h = harness();
    push(&h.state, ChatEntry::user("keep me"));
    let entry_id = h.state.read().session.active_session().history()[0]
        .id
        .clone();
    h.state
        .write()
        .session
        .active_session_mut()
        .pin_entry(&entry_id, PinPosition::Top);

    // When the page toggles that entry's pin.
    say(
        &h,
        ClientMsg::TogglePin {
            entry_id: entry_id.clone(),
        },
    )
    .await;

    // Then exactly one unpin command is published, naming the entry.
    let unpins = h.audit.of_type::<UnpinChatEntry>();
    assert_eq!(unpins.len(), 1);
    assert_eq!(unpins[0].entry_id, entry_id);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_an_unknown_entry_is_refused_and_publishes_nothing() {
    // Given a session whose history holds no entry with the id the page names.
    let h = harness();
    push(&h.state, ChatEntry::user("keep me"));

    // When the page toggles the pin on an id the session does not hold.
    let reply = dispatch(
        ClientMsg::TogglePin {
            entry_id: ChatEntryId::new(),
        },
        &h.handle,
    )
    .await;

    // Then the page is refused and nothing is published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names(), Vec::<&str>::new());
}

#[rstest::rstest]
#[tokio::test]
async fn forking_an_entry_publishes_the_fork_command_at_its_index() {
    // Given a session whose history holds three entries.
    let h = harness();
    push(&h.state, ChatEntry::user("first"));
    push(&h.state, ChatEntry::user("second"));
    push(&h.state, ChatEntry::user("third"));
    let second_id = h.state.read().session.active_session().history()[1]
        .id
        .clone();

    // When the page forks at the second entry.
    say(
        &h,
        ClientMsg::Fork {
            entry_id: second_id,
        },
    )
    .await;

    // Then one fork command is published, naming the active session and the
    // entry's index in its history — the ordinal the terminal's `f` sends.
    let forks = h.audit.of_type::<SessionForkRequested>();
    assert_eq!(forks.len(), 1);
    assert_eq!(forks[0].source_session_id, active_id(&h.state));
    assert_eq!(forks[0].at_ordinal, 1);
}

#[rstest::rstest]
#[tokio::test]
async fn forking_counts_a_leading_transient_as_no_ordinal() {
    // Given a session whose history opens with a transient — skill
    // discovery's table — ahead of a user entry and its reply.
    let h = harness();
    push(&h.state, ChatEntry::transient("skills discovered"));
    push(&h.state, ChatEntry::user("hello"));
    push(&h.state, ChatEntry::assistant("reply"));
    let user_id = h.state.read().session.active_session().history()[1]
        .id
        .clone();

    // When the page forks at the user entry.
    say(&h, ClientMsg::Fork { entry_id: user_id }).await;

    // Then the ordinal is 0 — the entry's place among the persisted entries
    // the store slices, not its place in the live history where the
    // transient counts as a slot.
    let forks = h.audit.of_type::<SessionForkRequested>();
    assert_eq!(forks[0].at_ordinal, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn forking_an_unknown_entry_is_refused_and_publishes_nothing() {
    // Given a session whose history holds no entry with the id the page names.
    let h = harness();
    push(&h.state, ChatEntry::user("keep me"));

    // When the page forks at an id the session does not hold.
    let reply = dispatch(
        ClientMsg::Fork {
            entry_id: ChatEntryId::new(),
        },
        &h.handle,
    )
    .await;

    // Then the page is refused and nothing is published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names(), Vec::<&str>::new());
}

#[rstest::rstest]
#[tokio::test]
async fn forking_an_entry_arms_the_loading_guard_for_the_source() {
    // Given a session with one entry to fork at.
    let h = harness();
    push(&h.state, ChatEntry::user("fork from here"));
    let entry_id = h.state.read().session.active_session().history()[0]
        .id
        .clone();

    // When the page forks at that entry.
    say(&h, ClientMsg::Fork { entry_id }).await;

    // Then the loading guard names the source — armed as the terminal arms
    // it, so the store actor re-points it at the child it is about to mint.
    let source = active_id(&h.state);
    let guard = h.state.read();
    let loading = guard.session.session_load_guard().expect("a load is armed");
    assert_eq!(loading.session_id, source);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_a_tool_result_pins_the_entry_the_page_named() {
    // Given a session whose history holds an assistant step with one tool
    // call and its result — the loop a pin takes whole.
    let h = harness();
    push(&h.state, ChatEntry::assistant("checking the worker"));
    push(
        &h.state,
        ChatEntry::tool_call("call-1", "bash", r#"{"command":"ls"}"#),
    );
    push(
        &h.state,
        ChatEntry::tool_result(
            "call-1",
            "bash",
            "src",
            jinn_core_types::ToolResultStatus::Success,
        ),
    );
    let result_id = h.state.read().session.active_session().history()[2]
        .id
        .clone();

    // When the page toggles the pin on the tool result.
    say(
        &h,
        ClientMsg::TogglePin {
            entry_id: result_id.clone(),
        },
    )
    .await;

    // Then one pin command is published, naming the tool result itself: the
    // session actor's own chunk rule then takes the whole loop, exactly as
    // the terminal's `p` does.
    let pins = h.audit.of_type::<PinChatEntry>();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].entry_id, result_id);
    assert_eq!(pins[0].position, PinPosition::Relative);
}

#[rstest::rstest]
#[test]
fn a_pin_changed_event_upserts_the_entry_the_page_holds_differently() {
    // Given a connected page that holds one entry as it stood unpinned.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();
    push(&h.state, ChatEntry::user("keep me"));
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);
    let entry_id = h.state.read().session.active_session().history()[0]
        .id
        .clone();

    // When the entry is pinned — as the terminal's `p` does — and the
    // pin event reaches the outbound actor.
    h.state
        .write()
        .session
        .active_session_mut()
        .pin_entry(&entry_id, PinPosition::Top);
    crate::outbound::pin_changed(
        &h.handle,
        &ChatEntryPinChanged {
            session_id: active_id(&h.state),
        },
    );

    // Then the page receives exactly one upsert, and it carries the pin.
    let frames = drain(&mut inbox);
    assert!(matches!(frames.as_slice(), [ServerMsg::EntryUpsert(_)]));
    let ServerMsg::EntryUpsert(upsert) = &frames[0] else {
        unreachable!("matched above");
    };
    let entry: ChatEntry = serde_json::from_value(upsert.entry.entry.as_json().clone())
        .expect("the projected entry deserializes back");
    assert_eq!(entry.id, entry_id);
    assert!(entry.is_pinned());
}

#[rstest::rstest]
#[test]
fn a_pin_changed_event_for_another_session_sends_nothing() {
    // Given a connected page holding one entry, already delivered.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();
    push(&h.state, ChatEntry::user("keep me"));
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);

    // When a pin event names a session the page is not viewing.
    crate::outbound::pin_changed(
        &h.handle,
        &ChatEntryPinChanged {
            session_id: jinn_core_types::SessionId::new(),
        },
    );

    // Then the page receives nothing.
    assert_eq!(drain(&mut inbox).len(), 0);
}

// ---------------------------------------------------------------------------
// Context toggle
// ---------------------------------------------------------------------------

/// The single context-override command the page published, with its target.
fn one_override_target(h: &Harness) -> ContextOverride {
    let published = h.audit.of_type::<SetEntryContextOverride>();
    assert_eq!(published.len(), 1, "exactly one override command");
    published[0].value
}

/// Toggles the context of the active session's only entry.
async fn toggle_only_entry(h: &Harness) {
    let entry_id = h.state.read().session.active_session().history()[0]
        .id
        .clone();
    say(h, ClientMsg::ToggleContext { entry_id }).await;
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_context_on_a_default_entry_publishes_forced_exclude() {
    // Given a session holding one user entry at the default override.
    let h = harness();
    push(&h.state, ChatEntry::user("hello"));

    // When the page toggles that entry's context.
    toggle_only_entry(&h).await;

    // Then the override command forces the entry out of context.
    assert_eq!(one_override_target(&h), ContextOverride::ForcedExclude);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_context_on_a_default_excluded_kind_publishes_forced_include() {
    // Given a session holding one thinking entry — a kind the context
    // leaves out by default.
    let h = harness();
    push(&h.state, ChatEntry::thinking("let me think"));

    // When the page toggles that entry's context.
    toggle_only_entry(&h).await;

    // Then the override command forces the entry into context.
    assert_eq!(one_override_target(&h), ContextOverride::ForcedInclude);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_context_on_a_worker_excluded_entry_publishes_forced_include() {
    // Given a session holding one entry the auto-prune worker excluded.
    let h = harness();
    let mut entry = ChatEntry::user("hello");
    entry.apply_context_override(
        ContextOverride::ForcedExclude,
        jinn_core_types::ChangeSource::Worker {
            name: "trivial_assistant".to_owned(),
        },
    );
    push(&h.state, entry);

    // When the page toggles that entry's context.
    toggle_only_entry(&h).await;

    // Then the override command forces the entry back into context.
    assert_eq!(one_override_target(&h), ContextOverride::ForcedInclude);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_context_on_a_forced_include_entry_publishes_forced_exclude() {
    // Given a session holding one entry already forced into context.
    let h = harness();
    let mut entry = ChatEntry::user("hello");
    entry.apply_context_override(
        ContextOverride::ForcedInclude,
        jinn_core_types::ChangeSource::User,
    );
    push(&h.state, entry);

    // When the page toggles that entry's context.
    toggle_only_entry(&h).await;

    // Then the override command forces the entry out of context.
    assert_eq!(one_override_target(&h), ContextOverride::ForcedExclude);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_context_on_a_forced_exclude_entry_publishes_forced_include() {
    // Given a session holding one entry already forced out of context.
    let h = harness();
    let mut entry = ChatEntry::user("hello");
    entry.apply_context_override(
        ContextOverride::ForcedExclude,
        jinn_core_types::ChangeSource::User,
    );
    push(&h.state, entry);

    // When the page toggles that entry's context.
    toggle_only_entry(&h).await;

    // Then the override command forces the entry into context.
    assert_eq!(one_override_target(&h), ContextOverride::ForcedInclude);
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_context_on_a_pinned_entry_is_refused_and_publishes_nothing() {
    // Given a session holding one entry already pinned to the top.
    let h = harness();
    push(&h.state, ChatEntry::user("keep me"));
    let entry_id = h.state.read().session.active_session().history()[0]
        .id
        .clone();
    h.state
        .write()
        .session
        .active_session_mut()
        .pin_entry(&entry_id, PinPosition::Top);

    // When the page toggles the context on the pinned entry.
    let reply = dispatch(
        ClientMsg::ToggleContext {
            entry_id: entry_id.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the page is refused and nothing is published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names(), Vec::<&str>::new());
}

#[rstest::rstest]
#[tokio::test]
async fn toggling_context_on_an_unknown_entry_is_refused_and_publishes_nothing() {
    // Given a session whose history holds no entry with the id the page names.
    let h = harness();
    push(&h.state, ChatEntry::user("keep me"));

    // When the page toggles the context on an id the session does not hold.
    let reply = dispatch(
        ClientMsg::ToggleContext {
            entry_id: ChatEntryId::new(),
        },
        &h.handle,
    )
    .await;

    // Then the page is refused and nothing is published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names(), Vec::<&str>::new());
}

// ---------------------------------------------------------------------------
// Context toggle: the live push
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn a_context_override_changed_event_upserts_the_entry_the_page_holds_differently() {
    // Given a connected page that holds one entry as it stood in context.
    let h = harness();
    push(&h.state, ChatEntry::user("keep me"));
    let (_id, mut inbox, _connection) = h.handle.register();
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);
    let entry_id = h.state.read().session.active_session().history()[0]
        .id
        .clone();

    // When the entry is forced out of context — as the terminal's `x`
    // does — and the override event reaches the outbound actor.
    h.state
        .write()
        .session
        .active_session_mut()
        .set_entry_context_override_by_id(&entry_id, ContextOverride::ForcedExclude);
    crate::outbound::context_override_changed(
        &h.handle,
        &ContextOverrideChanged {
            session_id: active_id(&h.state),
            entry_id: entry_id.clone(),
        },
    );

    // Then the page receives exactly one upsert, and it reads as out of
    // context with the exclusion carried.
    let frames = drain(&mut inbox);
    assert!(matches!(frames.as_slice(), [ServerMsg::EntryUpsert(_)]));
    let ServerMsg::EntryUpsert(upsert) = &frames[0] else {
        unreachable!("matched above");
    };
    let entry: ChatEntry = serde_json::from_value(upsert.entry.entry.as_json().clone())
        .expect("the projected entry deserializes back");
    assert_eq!(entry.id, entry_id);
    assert_eq!(entry.context_override(), ContextOverride::ForcedExclude);
    assert!(!upsert.entry.in_context);
}

#[rstest::rstest]
#[tokio::test]
async fn a_context_override_changed_event_for_another_session_sends_nothing() {
    // Given a connected page holding one entry, already delivered.
    let h = harness();
    push(&h.state, ChatEntry::user("keep me"));
    let (_id, mut inbox, _connection) = h.handle.register();
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);

    // When an override event names a session the page is not viewing.
    crate::outbound::context_override_changed(
        &h.handle,
        &ContextOverrideChanged {
            session_id: jinn_core_types::SessionId::new(),
            entry_id: ChatEntryId::new(),
        },
    );

    // Then the page receives nothing.
    assert_eq!(drain(&mut inbox).len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn a_context_override_changed_event_upserts_every_member_of_the_tool_loop() {
    // Given a connected page holding a three-entry tool loop, delivered.
    let h = harness();
    push(&h.state, ChatEntry::assistant("checking the worker"));
    push(
        &h.state,
        ChatEntry::tool_call("call-1", "bash", r#"{"command":"ls"}"#),
    );
    push(
        &h.state,
        ChatEntry::tool_result(
            "call-1",
            "bash",
            "file.txt",
            jinn_core_types::ToolResultStatus::Success,
        ),
    );
    let (_id, mut inbox, _connection) = h.handle.register();
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);
    let result_id = h.state.read().session.active_session().history()[2]
        .id
        .clone();

    // When the result is forced out of context — the write takes the whole
    // loop — and the override event names only that result.
    h.state
        .write()
        .session
        .active_session_mut()
        .set_entry_context_override_by_id(&result_id, ContextOverride::ForcedExclude);
    crate::outbound::context_override_changed(
        &h.handle,
        &ContextOverrideChanged {
            session_id: active_id(&h.state),
            entry_id: result_id.clone(),
        },
    );

    // Then every member of the loop reaches the page as an upsert.
    let frames = drain(&mut inbox);
    let upserts = frames
        .iter()
        .filter(|f| matches!(f, ServerMsg::EntryUpsert(_)))
        .count();
    assert_eq!(upserts, 3);
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
    let (_id, mut inbox, _connection) = h.handle.register();
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
    let (_id, mut inbox, _connection) = h.handle.register();
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
        &Seed {
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
    assert_eq!(tool_names(&reply), [] as [String; 0]);
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
        &Seed {
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
    assert_eq!(mcp_names(&reply), [] as [String; 0]);
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
        );
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
        );
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
        runtime.set_singleton_status("echo", jinn_mcp_msg::McpConnectionStatus::Running);
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
    let (_id, mut inbox, _connection) = h.handle.register();
    let _ = dispatch(
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
        );
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
    let (_id, mut inbox, _connection) = h.handle.register();

    // When the stored event is handled.
    crate::outbound::mcp_runtime_stored(&h.handle);

    // Then nothing was pushed.
    assert_eq!(drain(&mut inbox).len(), 0);
}

// ---------------------------------------------------------------------------
// Persona rows / switch
// ---------------------------------------------------------------------------

/// A harness with the given personas in the catalog.
fn harness_with_personas(personas: &[(&'static str, &'static str, bool)]) -> Harness {
    harness_seeded(
        empty_providers(),
        Arc::new(jinn_kernel::common::services::test_services::FakeSessionStore),
        &Seed {
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
    assert_eq!(h.state.read().session.active_session().history().len(), 0);
    assert_eq!(reply.frames.len(), 0);
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
    assert_eq!(h.state.read().session.active_session().history().len(), 0);
    assert_eq!(h.audit.names(), Vec::<&str>::new());
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

/// A 1×1 PNG, as the wire carries it.
const TINY_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

fn upload(name: &str, data_base64: &str) -> jinn_webui2_msg::ImageUploadBody {
    jinn_webui2_msg::ImageUploadBody {
        name: name.to_owned(),
        data_base64: data_base64.to_owned(),
    }
}

/// How many image attachments the last `EnqueueUserMessage` carried.
fn last_enqueued_attachment_count(h: &Harness) -> usize {
    let published = h.audit.of_type::<EnqueueUserMessage>();
    match &published[published.len() - 1].entry.kind {
        ChatEntryKind::User { attachments, .. } => attachments.len(),
        _ => 0,
    }
}

#[rstest::rstest]
#[tokio::test]
async fn send_with_an_image_enqueues_a_user_entry_carrying_it() {
    // Given an idle session.
    let h = harness();

    // When the page sends a message with one image.
    say(
        &h,
        ClientMsg::Send {
            text: "look".to_owned(),
            images: vec![upload("dot.png", TINY_PNG)],
        },
    )
    .await;

    // Then the enqueued user entry carries one image attachment.
    assert_eq!(last_enqueued_attachment_count(&h), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn send_with_an_image_and_no_text_is_accepted() {
    // Given an idle session.
    let h = harness();

    // When the page sends nothing but an image.
    say(
        &h,
        ClientMsg::Send {
            text: String::new(),
            images: vec![upload("dot.png", TINY_PNG)],
        },
    )
    .await;

    // Then the message is enqueued rather than refused as empty.
    assert_eq!(h.audit.of_type::<EnqueueUserMessage>().len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn send_of_a_non_image_is_refused_naming_its_position() {
    // Given an idle session.
    let h = harness();

    // When the page sends bytes that are not an image.
    let reply = dispatch(
        ClientMsg::Send {
            text: "look".to_owned(),
            images: vec![upload("notes.txt", "aGVsbG8=")],
        },
        &h.handle,
    )
    .await;

    // Then the refusal names the offender's position.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        panic!("expected one error frame, got {reply:?}");
    };
    assert!(
        error
            .message
            .contains("image 0 is not a recognized image format")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_refused_image_send_publishes_nothing() {
    // Given an idle session.
    let h = harness();

    // When the page sends bytes that are not an image.
    let _ = dispatch(
        ClientMsg::Send {
            text: "look".to_owned(),
            images: vec![upload("notes.txt", "aGVsbG8=")],
        },
        &h.handle,
    )
    .await;

    // Then nothing reached the bus.
    assert_eq!(h.audit.names().len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn send_with_an_oversize_image_is_refused_naming_its_size() {
    // Given an idle session.
    let h = harness();
    // One byte over the cap: 5 MB + 1 of zeroes, base64-encoded.
    let oversize = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(vec![0u8; crate::attachment::MAX_IMAGE_BYTES + 1])
    };

    // When the page sends it.
    let reply = dispatch(
        ClientMsg::Send {
            text: "look".to_owned(),
            images: vec![upload("big.bin", &oversize)],
        },
        &h.handle,
    )
    .await;

    // Then the refusal names the size, the cap, and the way out.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        panic!("expected one error frame, got {reply:?}");
    };
    assert!(
        error
            .message
            .contains("the limit is 5242880 — resize it first"),
        "{}",
        error.message
    );
}

#[rstest::rstest]
#[tokio::test]
async fn queue_with_an_image_still_enqueues() {
    // Given a session running a turn.
    let h = harness();
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page queues a message with one image.
    say(
        &h,
        ClientMsg::Queue {
            text: "after".to_owned(),
            images: vec![upload("dot.png", TINY_PNG)],
        },
    )
    .await;

    // Then the enqueued user entry carries one image attachment.
    assert_eq!(last_enqueued_attachment_count(&h), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn send_with_an_image_while_busy_is_refused_before_ingestion() {
    // Given a session running a turn.
    let h = harness();
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page sends an image to the busy session.
    let reply = dispatch(
        ClientMsg::Send {
            text: "look".to_owned(),
            images: vec![upload("dot.png", TINY_PNG)],
        },
        &h.handle,
    )
    .await;

    // Then the busy refusal is the answer, and nothing reached the bus.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    // And the session was never marked interacted.
    assert_eq!(h.audit.names().len(), 0);
}

// ---------------------------------------------------------------------------
// Applet rows / push
// ---------------------------------------------------------------------------

/// The one `applet_rows` frame a reply carries.
fn applet_body(reply: &crate::protocol::Reply) -> &jinn_webui2_msg::AppletRowsBody {
    let [ServerMsg::AppletRows(body)] = reply.frames.as_slice() else {
        unreachable!("one applet_rows frame expected: {reply:?}");
    };
    body
}

/// The applet ids one rows body lists, in order.
fn applet_ids(body: &jinn_webui2_msg::AppletRowsBody) -> Vec<&str> {
    body.rows.iter().map(|row| row.id.as_str()).collect()
}

#[rstest::rstest]
#[tokio::test]
async fn applet_rows_publish_the_rescan_then_the_wake() {
    // Given a harness with a saved applet.
    let h = harness_with_applets(&[("quiz", "Quiz")], Some("127.0.0.1:41234"));

    // When the page asks for the applets modal's rows.
    let _ = dispatch(
        ClientMsg::AppletRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the rescan reached the bus first and the wake second — the
    // TUI picker opener's own pair, in its order.
    assert_eq!(h.audit.names(), ["RescanApplets", "WakeAppletsServer"]);
}

#[rstest::rstest]
#[tokio::test]
async fn applet_rows_answer_one_frame_echoing_the_filter() {
    // Given a harness with a saved applet.
    let h = harness_with_applets(&[("quiz", "Quiz")], Some("127.0.0.1:41234"));

    // When the page asks under a filter that matches it.
    let reply = dispatch(
        ClientMsg::AppletRows {
            filter: "qui".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the one reply is the rows frame, in the filter asked.
    assert_eq!(reply.frames.len(), 1);
    let body = applet_body(&reply);
    assert_eq!(body.filter, "qui");
    assert_eq!(applet_ids(body), ["quiz"]);
}

#[rstest::rstest]
#[tokio::test]
async fn applet_rows_link_each_row_to_the_serving_bind() {
    // Given two applets and a serving server.
    let h = harness_with_applets(
        &[("sales", "Sales"), ("quiz", "Quiz")],
        Some("127.0.0.1:41234"),
    );

    // When the page asks for the rows.
    let reply = dispatch(
        ClientMsg::AppletRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then each row carries the applets URL — bind, `/applets/<id>/`, the
    // trailing slash — and the body names the serving phase.
    let body = applet_body(&reply);
    let hrefs: Vec<&str> = body
        .rows
        .iter()
        .filter_map(|row| row.href.as_deref())
        .collect();
    assert_eq!(
        hrefs,
        [
            "http://127.0.0.1:41234/applets/sales/",
            "http://127.0.0.1:41234/applets/quiz/"
        ]
    );
    assert!(body.serving);
    assert_eq!(body.bind, "127.0.0.1:41234");
}

#[rstest::rstest]
#[tokio::test]
async fn applet_rows_while_offline_answer_rows_without_an_href() {
    // Given a saved applet and a dormant server.
    let h = harness_with_applets(&[("quiz", "Quiz")], None);

    // When the page asks for the rows.
    let reply = dispatch(
        ClientMsg::AppletRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the row is still listed but carries no href, and the body says
    // the server is offline with an empty bind.
    let body = applet_body(&reply);
    assert_eq!(applet_ids(body), ["quiz"]);
    assert!(body.rows[0].href.is_none());
    assert!(!body.serving);
    assert_eq!(body.bind, "");
}

#[rstest::rstest]
#[tokio::test]
async fn applet_rows_refuse_an_href_for_an_id_failing_the_gate() {
    // Given a serving server whose catalog carries a traversal id.
    let h = harness_with_applets(&[("../escape", "Escape")], Some("127.0.0.1:41234"));

    // When the page asks for the rows.
    let reply = dispatch(
        ClientMsg::AppletRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the row renders without an href even though the server is
    // serving — a hand-edited catalog cannot inject a path.
    let body = applet_body(&reply);
    assert_eq!(applet_ids(body), ["../escape"]);
    assert!(body.rows[0].href.is_none());
}

#[rstest::rstest]
#[tokio::test]
async fn applet_rows_with_an_empty_filter_keep_the_catalog_order_unmarked() {
    // Given a catalog already ordered newest-first.
    let h = harness_with_applets(&[("newest", "Newest"), ("older", "Older")], None);

    // When the page asks with an empty filter.
    let reply = dispatch(
        ClientMsg::AppletRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then every row is in the catalog's order, and none is marked.
    let body = applet_body(&reply);
    assert_eq!(applet_ids(body), ["newest", "older"]);
    assert!(
        body.rows
            .iter()
            .all(|row| row.title_runs.iter().all(|run| !run.matched)),
        "an empty filter marks nothing: {:?}",
        body.rows
    );
}

#[rstest::rstest]
#[tokio::test]
async fn applet_rows_filter_by_title_and_mark_the_matched_run() {
    // Given three applets.
    let h = harness_with_applets(
        &[
            ("alpha", "Alpha Report"),
            ("beta", "Beta Dashboard"),
            ("gamma", "Gamma Quiz"),
        ],
        None,
    );

    // When the page asks under a filter only one title matches.
    let reply = dispatch(
        ClientMsg::AppletRows {
            filter: "alp".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then only that row answers, with its matched run marked.
    let body = applet_body(&reply);
    assert_eq!(applet_ids(body), ["alpha"]);
    let matched: Vec<&str> = body.rows[0]
        .title_runs
        .iter()
        .filter(|run| run.matched)
        .map(|run| run.text.as_str())
        .collect();
    assert_eq!(matched, ["Alp"]);
}

#[rstest::rstest]
#[tokio::test]
async fn applet_rows_remember_the_filter_for_a_later_push() {
    // Given a harness with a saved applet.
    let h = harness_with_applets(&[("quiz", "Quiz")], None);

    // When the page asks under a filter.
    let _ = dispatch(
        ClientMsg::AppletRows {
            filter: "qui".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the handle remembers that filter — the one a later push
    // answers in.
    assert_eq!(h.handle.applet_filter().as_deref(), Some("qui"));
}

#[rstest::rstest]
#[tokio::test]
async fn applets_rescanned_pushes_the_events_catalog_not_the_cells() {
    // Given a cell holding row A, while the event carries row B — the
    // republisher has not written the cell yet.
    let h = harness_with_applets(&[("stale", "Stale")], Some("127.0.0.1:41234"));
    let (_id, mut inbox, _connection) = h.handle.register();
    let _ = dispatch(
        ClientMsg::AppletRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;
    drain(&mut inbox);
    let event_catalog = jinn_applets_msg::AppletsCatalog {
        entries: vec![jinn_applets_msg::AppletEntry {
            id: "fresh".to_owned(),
            title: "Fresh".to_owned(),
            created_at: String::new(),
        }],
    };

    // When the rescanned event is handled.
    crate::outbound::applets_rescanned(&h.handle, &event_catalog);

    // Then the pushed frame lists B — the event's catalog — never the
    // cell's stale A.
    let frames = drain(&mut inbox);
    let [ServerMsg::AppletRows(body)] = frames.as_slice() else {
        unreachable!("one applet_rows frame expected: {frames:?}");
    };
    assert_eq!(applet_ids(body), ["fresh"]);
}

#[rstest::rstest]
#[tokio::test]
async fn applets_serving_changed_pushes_the_events_phase_over_the_cell_catalog() {
    // Given an open modal whose cell is offline and holds one applet.
    let h = harness_with_applets(&[("quiz", "Quiz")], None);
    let (_id, mut inbox, _connection) = h.handle.register();
    let _ = dispatch(
        ClientMsg::AppletRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;
    drain(&mut inbox);

    // When a serving event lands: the server came up at a resolved bind.
    crate::outbound::applets_serving_changed(&h.handle, true, "127.0.0.1:4321");

    // Then the pushed frame carries the event's phase — online, with the
    // event's bind in every href — over the cell's catalog.
    let frames = drain(&mut inbox);
    let [ServerMsg::AppletRows(body)] = frames.as_slice() else {
        unreachable!("one applet_rows frame expected: {frames:?}");
    };
    assert!(body.serving);
    assert_eq!(body.bind, "127.0.0.1:4321");
    assert_eq!(
        body.rows[0].href.as_deref(),
        Some("http://127.0.0.1:4321/applets/quiz/")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn applets_rescanned_with_no_modal_open_pushes_nothing() {
    // Given a harness with a saved applet and no modal that ever asked.
    let h = harness_with_applets(&[("quiz", "Quiz")], None);
    let (_id, mut inbox, _connection) = h.handle.register();

    // When a rescanned event is handled.
    crate::outbound::applets_rescanned(&h.handle, &jinn_applets_msg::AppletsCatalog::default());

    // Then nothing was pushed — nobody is looking.
    assert_eq!(drain(&mut inbox).len(), 0);
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn set_theme_publishes_the_app_state_update() {
    // Given a page.
    let h = harness();

    // When the page picks dark.
    say(
        &h,
        ClientMsg::SetTheme {
            theme: "dark".to_owned(),
        },
    )
    .await;

    // Then the app-state actor is asked to persist it.
    let updates = h.audit.of_type::<UpdateAppState>();
    assert_eq!(updates.len(), 1);
    assert!(matches!(
        updates[0].updates.as_slice(),
        [AppStateUpdate::SetWebTheme(Some(theme))] if theme == "dark"
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn set_theme_replies_with_the_chosen_mode() {
    // Given a page.
    let h = harness();

    // When the page picks light.
    let reply = dispatch(
        ClientMsg::SetTheme {
            theme: "light".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the reply carries the mode the page picked.
    assert!(matches!(
        reply.frames.as_slice(),
        [ServerMsg::Theme(theme)] if theme.theme == "light"
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn set_theme_of_an_unknown_mode_is_refused() {
    // Given a page.
    let h = harness();

    // When the page picks a mode outside the vocabulary.
    let reply = dispatch(
        ClientMsg::SetTheme {
            theme: "sepia".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the refusal names the mode and publishes nothing.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names().len(), 0);
}

// ---------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn set_library_publishes_the_store_command() {
    // Given a page with one live session.
    let h = harness();
    let id = active_id(&h.state);

    // When the page adds the session to the library.
    say(
        &h,
        ClientMsg::SetLibrary {
            session_id: id.clone(),
            in_library: true,
        },
    )
    .await;

    // Then the store's command is published, with the flag.
    let commands = h.audit.of_type::<SetSessionLibrary>();
    assert_eq!(commands.len(), 1);
    assert!(commands[0].in_library);
}

#[rstest::rstest]
#[tokio::test]
async fn set_library_of_an_unknown_session_is_refused() {
    // Given a page with an empty store.
    let h = harness_with_store();

    // When the page names a session nothing knows.
    let reply = dispatch(
        ClientMsg::SetLibrary {
            session_id: jinn_core_types::SessionId::new(),
            in_library: true,
        },
        &h.handle,
    )
    .await;

    // Then the request is refused and the store is not asked.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names().len(), 0);
}

// ---------------------------------------------------------------------------
// Archive session
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn archive_session_publishes_the_store_command() {
    // Given a page with one live, idle session.
    let h = harness();
    let id = active_id(&h.state);

    // When the page archives it.
    say(
        &h,
        ClientMsg::ArchiveSession {
            session_id: id.clone(),
        },
    )
    .await;

    // Then the store's command is published, naming the session.
    let commands = h.audit.of_type::<ArchiveSession>();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].session_id, id);
}

#[rstest::rstest]
#[tokio::test]
async fn archive_of_a_busy_session_is_refused_and_publishes_nothing() {
    // Given a session running a turn.
    let h = harness();
    let id = active_id(&h.state);
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page archives it.
    let reply = dispatch(
        ClientMsg::ArchiveSession {
            session_id: id.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told the session is busy and nothing was published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names().len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn archive_of_a_busy_session_names_the_busy_reason() {
    // Given a session running a turn.
    let h = harness();
    let id = active_id(&h.state);
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page archives it.
    let reply = dispatch(
        ClientMsg::ArchiveSession {
            session_id: id.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the refusal tells the user to cancel the turn first.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        panic!("expected one error frame, got {:?}", reply.frames);
    };
    assert_eq!(error.message, "session is busy — cancel the turn first");
}

#[rstest::rstest]
#[tokio::test]
async fn archive_of_an_unknown_session_is_refused() {
    // Given a page with an empty store.
    let h = harness_with_store();

    // When the page names a session nothing knows.
    let reply = dispatch(
        ClientMsg::ArchiveSession {
            session_id: jinn_core_types::SessionId::new(),
        },
        &h.handle,
    )
    .await;

    // Then the request is refused and nothing is published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names().len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn archive_of_a_stored_only_session_is_refused_as_already_archived() {
    // Given a session that is on disk and not in memory — an archived one.
    let h = harness_with_store();
    let id = store_only_session(&h, "already archived").await;

    // When the page archives it again.
    let reply = dispatch(
        ClientMsg::ArchiveSession {
            session_id: id.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told it is already archived and nothing is published.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        panic!("expected one error frame, got {:?}", reply.frames);
    };
    assert_eq!(error.message, format!("session {id} is already archived"));
    assert_eq!(h.audit.names().len(), 0);
}

// ---------------------------------------------------------------------------
// Teardown + archive (the terminal's `x`)
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn teardown_publishes_the_close_command() {
    // Given a page with one live, idle session.
    let h = harness();
    let id = active_id(&h.state);

    // When the page tears it down.
    let reply = dispatch(
        ClientMsg::TeardownSession {
            session_id: id.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the lifecycle actor's close command is published, naming the
    // session, and nothing is replied.
    let commands = h.audit.of_type::<CloseSession>();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].session_id, id);
    assert!(reply.frames.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_of_a_busy_session_is_refused_and_publishes_nothing() {
    // Given a session running a turn.
    let h = harness();
    let id = active_id(&h.state);
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page tears it down.
    let reply = dispatch(
        ClientMsg::TeardownSession {
            session_id: id.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told the session is busy and nothing was published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names().len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_of_a_busy_session_names_the_busy_reason() {
    // Given a session running a turn.
    let h = harness();
    let id = active_id(&h.state);
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page tears it down.
    let reply = dispatch(
        ClientMsg::TeardownSession {
            session_id: id.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the refusal is the same sentence an archive is refused with.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        panic!("expected one error frame, got {:?}", reply.frames);
    };
    assert_eq!(error.message, "session is busy — cancel the turn first");
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_of_an_unknown_session_is_refused() {
    // Given a page with an empty store.
    let h = harness_with_store();

    // When the page names a session nothing knows.
    let reply = dispatch(
        ClientMsg::TeardownSession {
            session_id: jinn_core_types::SessionId::new(),
        },
        &h.handle,
    )
    .await;

    // Then the request is refused and nothing is published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names().len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn teardown_of_a_stored_only_session_is_refused_as_already_archived() {
    // Given a session that is on disk and not in memory — an archived one.
    let h = harness_with_store();
    let id = store_only_session(&h, "already archived").await;

    // When the page tears it down.
    let reply = dispatch(
        ClientMsg::TeardownSession {
            session_id: id.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told it is already archived and nothing is published.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        panic!("expected one error frame, got {:?}", reply.frames);
    };
    assert_eq!(error.message, format!("session {id} is already archived"));
    assert_eq!(h.audit.names().len(), 0);
}

// ---------------------------------------------------------------------------
// Model refresh
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn refresh_models_publishes_the_discovery_command() {
    // Given a page.
    let h = harness();

    // When the page asks for a refresh.
    say(&h, ClientMsg::RefreshModels).await;

    // Then discovery is asked to run.
    assert_eq!(h.audit.names(), ["RefreshModels"]);
}

// ---------------------------------------------------------------------------
// New session
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// New session
// ---------------------------------------------------------------------------

/// One `[[project.entry]]` pointing at a scratch directory.
fn demo_project() -> jinn_preferences_config::schemas::ProjectConfig {
    jinn_preferences_config::schemas::ProjectConfig {
        // Built through the marked type so no raw owning path is spelled;
        // the config schema's field is the plain path, taken back out.
        path: marked_path::MarkedPathBuf::<marked_path::Absolute>::new("/tmp/demo-project")
            .expect("an absolute scratch path")
            .into_inner(),
    }
}

/// A lifecycle whose setup takes two positional arguments.
fn two_arg_lifecycle() -> jinn_preferences_config::schemas::SessionLifecycle {
    jinn_preferences_config::schemas::SessionLifecycle {
        name: "kickoff".to_owned(),
        description: None,
        setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
            "echo $1 $2".to_owned(),
        )),
        teardown: None,
    }
}

/// A harness with one project and one two-argument lifecycle configured.
fn harness_with_project_and_lifecycle() -> Harness {
    harness_seeded(
        empty_providers(),
        Arc::new(jinn_kernel::common::services::test_services::FakeSessionStore),
        &Seed {
            projects: vec![demo_project()],
            lifecycles: vec![two_arg_lifecycle()],
            ..Seed::default()
        },
    )
}

#[rstest::rstest]
#[test]
fn new_session_options_list_the_configured_projects_and_lifecycles() {
    // Given a config with one project and one script lifecycle.
    let h = harness_with_project_and_lifecycle();

    // When the page asks for the form's choices.
    let reply = actions::new_session_options(&h.handle);

    // Then the project and the lifecycle are listed, with how many arguments
    // its setup takes and the template's own spelling of each.
    let [ServerMsg::NewSessionOptions(options)] = reply.frames.as_slice() else {
        panic!("expected options, got {reply:?}");
    };
    assert_eq!(options.projects, ["/tmp/demo-project"]);
    assert_eq!(options.lifecycles[0].name, "kickoff");
    assert_eq!(options.lifecycles[0].param_count, 2);
    assert_eq!(options.lifecycles[0].params, ["$1", "$2"]);
}

#[rstest::rstest]
#[test]
fn new_session_options_name_a_named_placeholder_as_the_template_spells_it() {
    // Given a lifecycle whose setup takes a named argument.
    let h = harness_seeded(
        empty_providers(),
        Arc::new(jinn_kernel::common::services::test_services::FakeSessionStore),
        &Seed {
            projects: vec![demo_project()],
            lifecycles: vec![jinn_preferences_config::schemas::SessionLifecycle {
                name: "kickoff".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "deploy.sh <branch>".to_owned(),
                )),
                teardown: None,
            }],
            ..Seed::default()
        },
    );

    // When the page asks for the form's choices.
    let reply = actions::new_session_options(&h.handle);

    // Then the argument is named as the template spells it.
    let [ServerMsg::NewSessionOptions(options)] = reply.frames.as_slice() else {
        panic!("expected options, got {reply:?}");
    };
    assert_eq!(options.lifecycles[0].params, ["<branch>"]);
}

#[rstest::rstest]
#[test]
fn new_session_options_with_no_projects_list_none() {
    // Given a config with no `[[project.entry]]`.
    let h = harness();

    // When the page asks for the form's choices.
    let reply = actions::new_session_options(&h.handle);

    // Then the project list is empty, which the modal reads as its refusal.
    let [ServerMsg::NewSessionOptions(options)] = reply.frames.as_slice() else {
        panic!("expected options, got {reply:?}");
    };
    assert_eq!(options.projects.len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn create_session_of_an_unconfigured_project_is_refused() {
    // Given a config with one project.
    let h = harness_with_project_and_lifecycle();

    // When the page names a project the config does not list.
    let reply = dispatch(
        ClientMsg::CreateSession {
            project: "/tmp/not-configured".to_owned(),
            lifecycle: String::new(),
            args: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the request is refused and nothing is published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names().len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn create_session_of_an_unknown_lifecycle_is_refused() {
    // Given a config with one lifecycle named kickoff.
    let h = harness_with_project_and_lifecycle();

    // When the page names a lifecycle the config does not list.
    let reply = dispatch(
        ClientMsg::CreateSession {
            project: "/tmp/demo-project".to_owned(),
            lifecycle: "nope".to_owned(),
            args: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the request is refused and nothing is published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(h.audit.names().len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn create_session_with_too_few_args_is_refused() {
    // Given a config with a lifecycle that takes two arguments.
    let h = harness_with_project_and_lifecycle();

    // When the page sends one.
    let reply = dispatch(
        ClientMsg::CreateSession {
            project: "/tmp/demo-project".to_owned(),
            lifecycle: "kickoff".to_owned(),
            args: "only-one".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the refusal names the minimum it missed.
    let [ServerMsg::Error(error)] = reply.frames.as_slice() else {
        panic!("expected one error frame, got {reply:?}");
    };
    assert!(
        error.message.contains("takes 2 argument(s), got 1"),
        "{}",
        error.message
    );
}

#[rstest::rstest]
#[tokio::test]
async fn create_session_with_the_blank_lifecycle_makes_the_new_session_active() {
    // Given a config with one project.
    let h = harness_with_project_and_lifecycle();
    let before = active_id(&h.state);

    // When the page creates a session with no lifecycle.
    let reply = dispatch(
        ClientMsg::CreateSession {
            project: "/tmp/demo-project".to_owned(),
            lifecycle: String::new(),
            args: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then the instance's active session is a new one.
    assert_ne!(active_id(&h.state), before);
    // And the page is answered with a snapshot of it.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Snapshot(_)]));
}

// ---------------------------------------------------------------------------
// Outbound pushes: model refresh, library, follow
// ---------------------------------------------------------------------------

/// Drives the actor's `ModelCacheStored` behaviour with nothing spawned.
fn model_cache_stored(h: &Harness) {
    crate::outbound::model_cache_stored(&h.handle);
}

/// A harness with one provider model the picker can list.
fn harness_with_a_model() -> Harness {
    harness_with_providers(two_providers())
}

#[rstest::rstest]
#[tokio::test]
async fn model_cache_stored_pushes_fresh_rows_to_an_open_model_picker() {
    // Given a page whose model picker asked for rows.
    let h = harness_with_a_model();
    let (_id, mut inbox, _connection) = h.handle.register();
    let _ = dispatch(
        ClientMsg::ModelRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // When a discovery scan lands.
    model_cache_stored(&h);

    // Then the page receives one model-rows frame in the filter it asked for.
    let frames = drain(&mut inbox);
    assert!(matches!(
        frames.as_slice(),
        [ServerMsg::ModelRows(body)] if body.filter.is_empty()
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn model_cache_stored_pushes_nothing_when_no_picker_asked() {
    // Given a page that never opened the model picker.
    let h = harness_with_a_model();
    let (_id, mut inbox, _connection) = h.handle.register();

    // When a discovery scan lands.
    model_cache_stored(&h);

    // Then the page receives nothing.
    assert_eq!(drain(&mut inbox).len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn model_cache_stored_pushes_rows_naming_the_askers_session() {
    // Given a page whose model picker asked for rows.
    let h = harness_with_a_model();
    let (_id, mut inbox, _connection) = h.handle.register();
    let _ = dispatch(
        ClientMsg::ModelRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // When a discovery scan lands.
    model_cache_stored(&h);

    // Then the frame names the session the picker belongs to.
    let frames = drain(&mut inbox);
    let [ServerMsg::ModelRows(body)] = frames.as_slice() else {
        panic!("one model-rows frame expected, got {frames:?}");
    };
    assert_eq!(body.session_id, active_id(&h.state));
}

#[rstest::rstest]
#[test]
fn a_library_member_is_listed_under_library_and_not_sessions() {
    // Given a page with one loaded session marked as a library member.
    let h = harness();
    let id = active_id(&h.state);
    h.state
        .write()
        .session
        .active_session_mut()
        .set_in_library(true);

    // When the sidebar's rows are built.
    let (sessions, library, _) = crate::snapshot::session_rows(
        &h.state,
        &crate::settle::SettleTracker::default(),
        &std::collections::HashSet::new(),
    );

    // Then the session is in the library list and not the sessions list.
    assert_eq!(library[0].session_id, id);
    assert_eq!(sessions.len(), 0);
}

#[rstest::rstest]
#[test]
fn a_non_member_is_listed_under_sessions_and_not_library() {
    // Given a page with one loaded session that is not a member.
    let h = harness();

    // When the sidebar's rows are built.
    let (sessions, library, _) = crate::snapshot::session_rows(
        &h.state,
        &crate::settle::SettleTracker::default(),
        &std::collections::HashSet::new(),
    );

    // Then the session is in the sessions list and the library list is empty.
    assert_eq!(sessions[0].session_id, active_id(&h.state));
    assert_eq!(library.len(), 0);
}

#[rstest::rstest]
#[test]
fn membership_changed_pushes_a_sessions_frame() {
    // Given a page.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();

    // When the store actor announces a membership change.
    crate::outbound::library_membership_changed(&h.handle);

    // Then the page receives one sessions frame.
    let frames = drain(&mut inbox);
    assert!(matches!(frames.as_slice(), [ServerMsg::Sessions(_)]));
}

#[rstest::rstest]
#[test]
fn a_sessions_frame_carries_the_hydration_flag() {
    // Given an instance whose startup hydration is still running, and a page.
    let h = harness();
    h.state.write().session.begin_startup_hydration();
    let (_id, mut inbox, _connection) = h.handle.register();

    // When a session-list push is sent.
    crate::outbound::sessions_changed(&h.handle);

    // Then the frame tells the page the saved sessions are still loading.
    let frames = drain(&mut inbox);
    let [ServerMsg::Sessions(body)] = frames.as_slice() else {
        unreachable!("one sessions frame expected, got {frames:?}");
    };
    assert!(body.hydrating);
}

#[rstest::rstest]
#[test]
fn startup_hydration_finished_pushes_one_sessions_frame_that_is_not_hydrating() {
    // Given an instance whose hydration has just ended — the store clears the
    // flag before it publishes the event — and a page.
    let h = harness();
    h.state.write().session.begin_startup_hydration();
    let (_id, mut inbox, _connection) = h.handle.register();
    h.state.write().session.finish_startup_hydration();

    // When the end of hydration reaches the outbound actor.
    crate::outbound::startup_hydration_finished(&h.handle);

    // Then the page receives exactly one sessions frame, and it says nothing
    // is loading — which is what brings the rail's loading row down.
    let frames = drain(&mut inbox);
    let [ServerMsg::Sessions(body)] = frames.as_slice() else {
        unreachable!("one sessions frame expected, got {frames:?}");
    };
    assert!(!body.hydrating);
}

// ---------------------------------------------------------------------------
// The settle time on a session row
// ---------------------------------------------------------------------------

/// A phase change to `new_phase` at a fixed moment, as the session actor
/// publishes it.
fn phase_change(
    session_id: &jinn_core_types::SessionId,
    old_phase: jinn_slices::PhaseView,
    new_phase: jinn_slices::PhaseView,
    at: jiff::Timestamp,
) -> jinn_session_msg::SessionPhaseChanged {
    jinn_session_msg::SessionPhaseChanged {
        session_id: session_id.clone(),
        old_phase,
        new_phase,
        at,
    }
}

/// The one session row a pushed `Sessions` frame carries, in a harness
/// whose state holds exactly one session.
fn only_row(frame: &ServerMsg) -> &jinn_webui2_msg::SessionRowBody {
    let ServerMsg::Sessions(body) = frame else {
        panic!("a sessions frame expected, got {frame:?}");
    };
    let [row] = body.rows.as_slice() else {
        panic!("one row expected, got {:?}", body.rows);
    };
    row
}

/// A fixed moment, so a settle time can be asserted exactly.
fn settled_at() -> jiff::Timestamp {
    moment("2026-11-26T10:00:00Z")
}

/// An RFC 3339 moment, parsed the way the wire round-trip parses one.
fn moment(rfc3339: &str) -> jiff::Timestamp {
    rfc3339.parse().expect("a valid timestamp")
}

#[rstest::rstest]
#[test]
fn a_landing_on_idle_stamps_the_settle_time_on_the_row() {
    // Given a page, and a session in its working phase.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();
    let id = active_id(&h.state);
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the session lands on Idle.
    crate::outbound::phase_changed(
        &h.handle,
        &phase_change(
            &id,
            jinn_slices::PhaseView::Streaming,
            jinn_slices::PhaseView::Idle,
            settled_at(),
        ),
    );

    // Then the row the page receives carries that moment.
    let frames = drain(&mut inbox);
    let [_, ServerMsg::Sessions(_)] = frames.as_slice() else {
        panic!("a phase frame then a sessions frame expected, got {frames:?}");
    };
    assert_eq!(only_row(&frames[1]).last_settled_at, Some(settled_at()));
}

#[rstest::rstest]
#[test]
fn a_phase_change_that_stays_working_leaves_the_row_unstamped() {
    // Given a page, and a session in its streaming phase.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();
    let id = active_id(&h.state);
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the phase changes without landing on Idle.
    crate::outbound::phase_changed(
        &h.handle,
        &phase_change(
            &id,
            jinn_slices::PhaseView::Idle,
            jinn_slices::PhaseView::Streaming,
            settled_at(),
        ),
    );

    // Then the row carries no settle time.
    let frames = drain(&mut inbox);
    let [_, ServerMsg::Sessions(_)] = frames.as_slice() else {
        panic!("a phase frame then a sessions frame expected, got {frames:?}");
    };
    assert_eq!(only_row(&frames[1]).last_settled_at, None);
}

#[rstest::rstest]
#[test]
fn a_subagent_landing_on_idle_leaves_its_row_unstamped() {
    // Given a page, and a subagent session in its working phase.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();
    let id = active_id(&h.state);
    {
        let mut guard = h.state.write();
        let session = guard.session.active_session_mut();
        session.set_origin(jinn_session_msg::SessionOrigin::Subagent);
        session.begin_streaming();
    }

    // When the subagent lands on Idle.
    crate::outbound::phase_changed(
        &h.handle,
        &phase_change(
            &id,
            jinn_slices::PhaseView::Streaming,
            jinn_slices::PhaseView::Idle,
            settled_at(),
        ),
    );

    // Then its row carries no settle time.
    let frames = drain(&mut inbox);
    let [_, ServerMsg::Sessions(_)] = frames.as_slice() else {
        panic!("a phase frame then a sessions frame expected, got {frames:?}");
    };
    assert_eq!(only_row(&frames[1]).last_settled_at, None);
}

#[rstest::rstest]
#[test]
fn a_session_that_never_settled_carries_no_settle_time() {
    // Given a page, and a session that has run no turn.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();

    // When the session list is pushed for an unrelated reason.
    crate::outbound::library_membership_changed(&h.handle);

    // Then the row carries no settle time.
    let frames = drain(&mut inbox);
    assert_eq!(
        only_row(&frames[0]).last_settled_at,
        None,
        "frames: {frames:?}"
    );
}

#[rstest::rstest]
#[test]
fn a_later_settle_replaces_the_rows_earlier_time() {
    // Given a page, and a session that settled once.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();
    let id = active_id(&h.state);
    let first = moment("2026-11-26T10:00:00Z");
    let later = moment("2026-11-26T10:05:00Z");
    crate::outbound::phase_changed(
        &h.handle,
        &phase_change(
            &id,
            jinn_slices::PhaseView::Streaming,
            jinn_slices::PhaseView::Idle,
            first,
        ),
    );
    let _ = drain(&mut inbox);

    // When the session settles again.
    crate::outbound::phase_changed(
        &h.handle,
        &phase_change(
            &id,
            jinn_slices::PhaseView::Streaming,
            jinn_slices::PhaseView::Idle,
            later,
        ),
    );

    // Then the row carries the later moment.
    let frames = drain(&mut inbox);
    let [_, ServerMsg::Sessions(_)] = frames.as_slice() else {
        panic!("a phase frame then a sessions frame expected, got {frames:?}");
    };
    assert_eq!(only_row(&frames[1]).last_settled_at, Some(later));
}

#[rstest::rstest]
#[test]
fn an_archived_session_pushes_one_sessions_frame() {
    // Given a page.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();

    // When the store actor announces an archive.
    crate::outbound::sessions_changed(&h.handle);

    // Then the page receives one sessions frame.
    let frames = drain(&mut inbox);
    assert!(matches!(frames.as_slice(), [ServerMsg::Sessions(_)]));
}

#[rstest::rstest]
#[test]
fn a_failed_archive_pushes_one_error_frame() {
    // Given a page.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();

    // When the store actor announces a failed archive.
    crate::outbound::archive_failed(
        &h.handle,
        &jinn_session_msg::SessionArchiveFailed {
            session_id: jinn_core_types::SessionId::new(),
            error: "archiving the session write failed".to_owned(),
        },
    );

    // Then the page receives one error frame naming the reason.
    let frames = drain(&mut inbox);
    let [ServerMsg::Error(error)] = frames.as_slice() else {
        panic!("expected one error frame, got {frames:?}");
    };
    assert!(error.message.contains("archiving the session write failed"));
}

// ---------------------------------------------------------------------------
// The teardown busy dot
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn a_teardown_started_pushes_one_sessions_frame_with_the_flag_set() {
    // Given a page with one live session.
    let h = harness();
    let id = active_id(&h.state);
    let (_id, mut inbox, _connection) = h.handle.register();

    // When the lifecycle actor announces the teardown started.
    crate::outbound::teardown_started(
        &h.handle,
        &SessionTeardownStarted {
            session_id: id.clone(),
        },
    );

    // Then the page receives one sessions frame whose row carries the flag.
    let frames = drain(&mut inbox);
    let [frame] = frames.as_slice() else {
        panic!("expected one frame, got {frames:?}");
    };
    let row = only_row(frame);
    assert_eq!(row.session_id, id);
    assert!(row.is_tearing_down);
}

#[rstest::rstest]
#[test]
fn a_teardown_finished_pushes_one_sessions_frame_with_the_flag_clear() {
    // Given a page whose session is marked as tearing down.
    let h = harness();
    let id = active_id(&h.state);
    h.handle.mark_tearing_down(&id);
    let (_id, mut inbox, _connection) = h.handle.register();

    // When the lifecycle actor announces the teardown finished.
    crate::outbound::teardown_finished(
        &h.handle,
        &SessionTeardownFinished {
            session_id: id.clone(),
            error: None,
        },
    );

    // Then the page receives one sessions frame whose row has dropped the flag.
    let frames = drain(&mut inbox);
    let [frame] = frames.as_slice() else {
        panic!("expected one frame, got {frames:?}");
    };
    let row = only_row(frame);
    assert_eq!(row.session_id, id);
    assert!(!row.is_tearing_down);
}

#[rstest::rstest]
#[test]
fn a_finished_teardown_clears_a_flag_that_was_never_set() {
    // Given a page whose session was never marked — a teardown refused
    // before anything spawned publishes a finish and no start.
    let h = harness();
    let id = active_id(&h.state);
    let (_id, mut inbox, _connection) = h.handle.register();

    // When the lifecycle actor announces the teardown finished.
    crate::outbound::teardown_finished(
        &h.handle,
        &SessionTeardownFinished {
            session_id: id.clone(),
            error: Some("teardown refused".to_owned()),
        },
    );

    // Then the page still receives one sessions frame, with the flag clear.
    let frames = drain(&mut inbox);
    let [frame] = frames.as_slice() else {
        panic!("expected one frame, got {frames:?}");
    };
    let row = only_row(frame);
    assert!(!row.is_tearing_down);
}

#[rstest::rstest]
#[test]
fn a_snapshot_built_mid_teardown_carries_the_flag() {
    // Given a session whose teardown command is running.
    let h = harness();
    let id = active_id(&h.state);
    h.handle.mark_tearing_down(&id);

    // When a fresh page's snapshot is built.
    let frame = crate::snapshot::snapshot(
        &h.state,
        h.handle.revisions(),
        h.handle.settles(),
        &h.handle.tearing_down(),
    );

    // Then the row the snapshot carries has the flag set — a page that
    // connects mid-teardown sees the dot from its first frame.
    let [row] = frame.body.sessions.as_slice() else {
        panic!("one session row expected, got {:?}", frame.body.sessions);
    };
    assert_eq!(row.session_id, id);
    assert!(row.is_tearing_down);
}

#[rstest::rstest]
#[tokio::test]
async fn active_session_changed_pushes_one_snapshot() {
    // Given a page.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();

    // When the instance's active session moves.
    crate::outbound::follow_active_session(
        &h.handle,
        &jinn_kernel::protocol::system::ActiveSessionChanged {
            session_id: active_id(&h.state),
        },
    )
    .await;

    // Then the page receives exactly one snapshot, of that session.
    let frames = drain(&mut inbox);
    let [ServerMsg::Snapshot(body)] = frames.as_slice() else {
        panic!("one snapshot expected, got {frames:?}");
    };
    assert_eq!(body.session_id, Some(active_id(&h.state)));
}

#[rstest::rstest]
#[tokio::test]
async fn follow_snapshot_marks_the_new_sessions_history_delivered() {
    // Given a page whose active session has one entry.
    let h = harness();
    let (_id, mut inbox, _connection) = h.handle.register();
    push(&h.state, ChatEntry::user("already here"));

    // When the instance's active session moves to it.
    crate::outbound::follow_active_session(
        &h.handle,
        &jinn_kernel::protocol::system::ActiveSessionChanged {
            session_id: active_id(&h.state),
        },
    )
    .await;

    // Then a later flush streams nothing: the snapshot already carried it.
    let _ = drain(&mut inbox);
    let frames = flush_frames_h(&h, &mut inbox);
    assert_eq!(frames.len(), 0);
}

/// Runs one flush for a harness and returns every frame its page received.
fn flush_frames_h(
    h: &Harness,
    inbox: &mut tokio::sync::mpsc::UnboundedReceiver<ServerMsg>,
) -> Vec<ServerMsg> {
    crate::outbound::flush(&h.handle);
    let mut frames = Vec::new();
    while let Ok(frame) = inbox.try_recv() {
        frames.push(frame);
    }
    frames
}

/// Spawns the listener on a real trouper bus over an OS-assigned port,
/// with a recorder for the dashboard note registered first, and returns
/// every note the launch published together with the bound address.
///
/// The listener must publish through a real bus: the recording bus
/// captures a `publish` call without broadcasting it, and the dashboard
/// consumes the broadcast.
async fn spawn_listener_and_collect_notes() -> (Vec<jinn_slices::ServiceStatusUpdate>, String) {
    let fabric = jinn_testutil::bus_harness::TestHarness::new().await;
    let notes = fabric
        .spawn_recorder::<jinn_slices::ServiceStatusUpdate>()
        .await;

    // The handle the listener's shared state is built over. Its bus is
    // not the one the note rides; nothing in these tests dispatches.
    let h = harness();
    let deps = crate::listener::ListenerDeps {
        bind: "127.0.0.1:0".to_owned(),
        shared: Arc::new(crate::server::ServerShared {
            outbound: h.handle.clone(),
            version: "test".to_owned(),
            health: crate::health::WebUi2Health::new(fabric.bus()),
        }),
        bus: fabric.bus(),
    };
    let (bound, _outbound) = crate::listener::spawn(fabric.system(), &deps)
        .await
        .expect("the listener binds an OS-assigned port");

    let notes =
        jinn_testutil::bus_harness::await_recorded(&notes, 1, std::time::Duration::from_secs(2))
            .await;
    (notes, bound.connection_note())
}

#[rstest::rstest]
#[tokio::test]
async fn a_bound_listener_publishes_one_launch_note_on_its_own_row() {
    // Given a recorder for the dashboard note, registered before the bind.
    // When the listener binds.
    let (notes, _note) = spawn_listener_and_collect_notes().await;

    // Then exactly one note was published, addressed to the listener's row.
    assert_eq!(notes.len(), 1, "one bind publishes one note: {notes:?}");
    assert_eq!(
        notes[0].name,
        crate::listener::WEBUI2_ACTOR_NAME,
        "the note must land on the row the census created"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_launch_note_names_the_port_that_was_actually_bound() {
    // Given a listener bound to an OS-assigned port, so only the bound
    // address can know the port.
    // When it publishes its note.
    let (notes, note) = spawn_listener_and_collect_notes().await;

    // Then the note is the bound address's page URL, the same string the
    // caller logs.
    assert_eq!(
        notes[0].status_message.as_deref(),
        Some(note.as_str()),
        "the row and the log read one string"
    );
    assert!(
        note.starts_with("serving on http://127.0.0.1:") && note.ends_with("/web2"),
        "a specific loopback bind names its own address and the page path: {note}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_launch_note_describes_the_row() {
    // Given a recorder for the dashboard note.
    // When the listener binds.
    let (notes, _note) = spawn_listener_and_collect_notes().await;

    // Then the note carries the row's description.
    assert_eq!(
        notes[0].description.as_deref(),
        Some("Web UI (browser transcript, send, cancel)"),
        "the row is told what the slice is"
    );
}

// ---------------------------------------------------------------------------
// The session status bar: the persona and the context figure, pushed on change
// ---------------------------------------------------------------------------

/// The model the status-bar tests run against: one whose window the cache
/// records, so the figure has a denominator.
const WINDOWED_MODEL: &str = "openrouter/anthropic/claude-sonnet-4";

/// Stamps a 200k window for [`WINDOWED_MODEL`] into the provider cell, the
/// way a model refresh leaves it, so a session on that model has a figure.
fn seed_window(handle: &OutboundHandle) {
    handle.provider_cell().update(|cell| {
        cell.model_cache = Some(jinn_provider_config::ModelCache {
            entries: [(
                "openrouter".to_owned(),
                vec![jinn_provider_config::ModelInfo {
                    id: "anthropic/claude-sonnet-4".to_owned(),
                    context_length: Some(200_000),
                    input_modalities: jinn_provider_config::InputModalities::text(),
                }],
            )]
            .into_iter()
            .collect(),
            last_updated_at: None,
        });
    });
}

/// Puts the active session on [`WINDOWED_MODEL`] with a 100k assembled
/// prompt, so its figure reads `50.0%/200k`.
fn put_session_at_half_window(page: &Page) {
    page.state
        .write()
        .session
        .active_session_mut()
        .set_model(ModelSelection::Single(WINDOWED_MODEL.to_owned()));
    page.state
        .write()
        .session
        .active_session_mut()
        .set_context_sizes(100_000, 0);
}

/// The `ContextUsage` frames among `frames`, as their `(persona, label)`.
fn status_frames(frames: &[ServerMsg]) -> Vec<(String, String)> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            ServerMsg::ContextUsage(body) => Some((body.persona.clone(), body.label.clone())),
            _ => None,
        })
        .collect()
}

#[rstest::rstest]
#[tokio::test]
async fn hello_answers_a_snapshot_naming_the_sessions_persona() {
    // Given a session running the reviewer persona.
    let h = harness();
    h.state
        .write()
        .session
        .active_session_mut()
        .set_persona_name("reviewer".to_owned());

    // When a page connects holding nothing.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: None,
            revision: None,
        },
        &h.handle,
    )
    .await;

    // Then the snapshot names it.
    let [ServerMsg::Snapshot(snapshot)] = reply.frames.as_slice() else {
        unreachable!("a fresh hello answers a snapshot");
    };
    assert_eq!(snapshot.persona, "reviewer");
}

#[rstest::rstest]
#[test]
fn a_snapshot_of_no_session_carries_no_persona() {
    // Given a state whose active session has been removed from the map,
    // leaving its id active and nothing behind it.
    let h = harness();
    let orphan = active_id(&h.state);
    h.state.write().session.remove_without_replacement(&orphan);

    // When a snapshot is taken of it.
    let frame = crate::snapshot::snapshot(
        &h.state,
        &crate::revision::RevisionTracker::default(),
        &crate::settle::SettleTracker::default(),
        &h.handle.tearing_down(),
    );

    // Then the body carries no persona — there is no session to name.
    assert_eq!(frame.body.persona, "");
}

#[rstest::rstest]
#[test]
fn an_idle_connection_receives_no_status_frame() {
    // Given a connected page and nothing changed between two ticks.
    let mut page = connected_page();

    // When the tick runs twice.
    let first = flush_frames(&mut page);
    let second = flush_frames(&mut page);

    // Then neither pushes: the first records a baseline, and the second has
    // nothing new to say.
    assert!(
        status_frames(&first).is_empty(),
        "the baseline tick is silent: {first:?}"
    );
    assert!(
        status_frames(&second).is_empty(),
        "an idle tick is silent: {second:?}"
    );
}

#[rstest::rstest]
#[test]
fn a_changed_figure_is_pushed_on_the_next_tick() {
    // Given a connected page whose baseline tick has been drained, and then
    // the session put on a windowed model with a half-full prompt.
    let mut page = connected_page();
    seed_window(&page.handle);
    let baseline = flush_frames(&mut page);
    assert!(
        status_frames(&baseline).is_empty(),
        "the baseline tick is silent: {baseline:?}"
    );

    // When the session's figure changes and the next tick runs.
    put_session_at_half_window(&page);
    let frames = flush_frames(&mut page);

    // Then exactly one frame arrives, carrying the new figure.
    let status = status_frames(&frames);
    let [(_, label)] = status.as_slice() else {
        unreachable!("one frame for the changed figure: {frames:?}");
    };
    assert_eq!(label, "50.0%/200k");
}

#[rstest::rstest]
#[test]
fn a_persona_change_is_pushed_on_the_next_tick() {
    // Given a connected page whose baseline tick has been drained.
    let mut page = connected_page();
    let baseline = flush_frames(&mut page);
    assert!(
        status_frames(&baseline).is_empty(),
        "the baseline tick is silent: {baseline:?}"
    );

    // When the session's persona changes and the next tick runs.
    page.state
        .write()
        .session
        .active_session_mut()
        .set_persona_name("reviewer".to_owned());
    let frames = flush_frames(&mut page);

    // Then exactly one frame arrives, naming the new persona.
    let status = status_frames(&frames);
    let [(persona, _)] = status.as_slice() else {
        unreachable!("one frame for the changed persona: {frames:?}");
    };
    assert_eq!(persona, "reviewer");
}

#[rstest::rstest]
#[test]
fn a_provider_switch_pushes_the_model_and_the_figure_at_once() {
    // Given a connected page whose baseline tick has been drained, a session
    // on a windowed model, and then a figure that differs from the baseline.
    let mut page = connected_page();
    seed_window(&page.handle);
    let baseline = flush_frames(&mut page);
    assert!(
        status_frames(&baseline).is_empty(),
        "the baseline tick is silent: {baseline:?}"
    );
    let session_id = active_id(&page.state);
    put_session_at_half_window(&page);

    // When the provider switch lands, outside the tick.
    crate::outbound::provider_switched(
        &page.handle,
        &jinn_provider_selection_msg::ProviderSwitched {
            session_id: session_id.clone(),
            provider_name: "openrouter".to_owned(),
        },
    );

    // Then one model frame and one figure frame arrive together, without
    // waiting for the next tick.
    let frames = drain(&mut page.inbox);
    assert_eq!(
        frames
            .iter()
            .filter(|frame| matches!(frame, ServerMsg::Model(_)))
            .count(),
        1,
        "one model frame: {frames:?}"
    );
    let status = status_frames(&frames);
    let [(_, label)] = status.as_slice() else {
        unreachable!("one figure frame beside the model frame: {frames:?}");
    };
    assert_eq!(label, "50.0%/200k");
}
