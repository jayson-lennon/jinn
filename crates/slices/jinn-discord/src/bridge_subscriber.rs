//! The Discord bridge subscriber — session events to gateway channels,
//! on trouper.
//!
//! A [`ServiceActor`] subscribed to the `jinn.session` topic (fed by
//! the core bridge's forward routes). It replaces the former kameo
//! bridge actor: the crossing is now bus → relay → topic (kernel
//! wiring) + this subscriber (slice folding), and no kameo actor lives
//! in the slice.
//!
//! # What it folds
//!
//! - [`SessionPhaseChanged`] with `new_phase == Idle` →
//!   [`BridgeEvent::TurnFinished`]
//! - [`SessionSetupCompleted`] → [`BridgeEvent::SetupCompleted`]
//! - [`SessionTeardownFinished`] → [`BridgeEvent::TeardownFinished`]
//! - [`SessionArchived`] → [`BridgeEvent::Archived`]
//! - [`CreateThreadForSession`] → [`GatewayRequest::CreateThreadForSession`]
//!   on the gateway-request channel
//! - [`DiscordThreadCreated`] / [`DiscordThreadCreateFailed`] → a
//!   [`ChatEntry`] pushed directly into the session's history
//!
//! All other topic traffic is ignored. The bot never sees streaming
//! tokens or intermediate tool calls — it only acts on turn boundaries
//! and lifecycle results.

use jinn_core_types::SessionId;
use jinn_discord_msg::{
    BridgeEvent, CreateThreadForSession, CreateThreadReason, DiscordThreadCreateFailed,
    DiscordThreadCreated, ForumChannelError, GatewayRequest,
};
use jinn_domain::common::state::State;
use jinn_domain::protocol::ChatEntry;
use jinn_session_msg::{
    SessionArchived, SessionPhaseChanged, SessionSetupCompleted, SessionTeardownFinished,
};
use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// The Discord bridge subscriber.
///
/// Holds the sender halves of the two gateway kanal channels, a clone
/// of [`State`], and the session capability — the fold writes the
/// `gdc` (to-thread) result `ChatEntry` inline on outcome events.
pub struct DiscordBridgeSubscriber {
    /// Forwards topic events onto this channel as [`BridgeEvent`]s.
    tx: kanal::Sender<BridgeEvent>,
    /// Forwards `CreateThreadForSession` requests onto this channel as
    /// [`GatewayRequest`]s — the reverse direction (domain → gateway
    /// do-something).
    gateway_tx: kanal::Sender<GatewayRequest>,
    /// Shared application state — writes the `gdc` (to-thread) result
    /// `ChatEntry` back into the targeted session's history.
    state: State,
    /// Authority to push entries into sessions.
    session_cap: jinn_domain::common::tcaps::session::SessionCap,
}

/// Dependencies for [`DiscordBridgeSubscriber`].
#[derive(Clone)]
pub struct DiscordBridgeSubscriberDeps {
    /// Sender half of the bounded (64) bridge channel.
    pub tx: kanal::Sender<BridgeEvent>,
    /// Sender half of the bounded (16) gateway-request channel.
    pub gateway_tx: kanal::Sender<GatewayRequest>,
    /// Shared application state.
    pub state: State,
    /// Authority to push entries into sessions.
    pub session_cap: jinn_domain::common::tcaps::session::SessionCap,
}

impl DiscordBridgeSubscriber {
    /// Spawns the subscriber at `discord-bridge` and subscribes it to
    /// the `jinn.session` topic.
    ///
    /// A successful [`ActorSystem::subscribe`] is the ordering
    /// guarantee: the topic cursor is registered before any gateway
    /// traffic can flow (the channels are parked until the frontend
    /// spawns the gateway), so no crossing event is missed.
    ///
    /// # Panics
    ///
    /// Panics if the topic subscription fails, which can only happen
    /// on a broken actor system; the slice activation ordering relies
    /// on the cursor being registered.
    pub fn spawn(system: &ActorSystem, deps: DiscordBridgeSubscriberDeps) -> ActorPath {
        let DiscordBridgeSubscriberDeps {
            tx,
            gateway_tx,
            state,
            session_cap,
        } = deps;
        let path = trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new("discord-bridge"))
            .start_with({
                move || {
                    Box::pin(async move {
                        Ok(Self {
                            tx,
                            gateway_tx,
                            state,
                            session_cap,
                        })
                    })
                }
            })
            .handles::<SessionPhaseChanged>()
            .handles::<SessionSetupCompleted>()
            .handles::<SessionTeardownFinished>()
            .handles::<SessionArchived>()
            .handles::<CreateThreadForSession>()
            .handles::<DiscordThreadCreated>()
            .handles::<DiscordThreadCreateFailed>()
            .start();

        #[expect(
            clippy::expect_used,
            reason = "subscription failure is a broken actor system, not a caller bug;                       the channel-parked-before-gateway ordering relies on the cursor"
        )]
        system
            .subscribe(&path, &jinn_session_msg::session_topic(), None)
            .expect("discord bridge subscriber subscribes to the session topic");
        path
    }
}

impl ServiceActor for DiscordBridgeSubscriber {
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the channels, state,
        // and capability via `start_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("DiscordBridgeSubscriber is spawned via start_with"),
        )
    }
}

impl MsgHandler<SessionPhaseChanged> for DiscordBridgeSubscriber {
    async fn handle(&mut self, msg: SessionPhaseChanged, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_phase_changed(&msg);
    }
}

impl MsgHandler<SessionSetupCompleted> for DiscordBridgeSubscriber {
    async fn handle(&mut self, msg: SessionSetupCompleted, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_setup_completed(&msg);
    }
}

impl MsgHandler<SessionTeardownFinished> for DiscordBridgeSubscriber {
    async fn handle(&mut self, msg: SessionTeardownFinished, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_teardown_finished(&msg);
    }
}

impl MsgHandler<SessionArchived> for DiscordBridgeSubscriber {
    async fn handle(&mut self, msg: SessionArchived, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_archived(&msg);
    }
}

impl MsgHandler<CreateThreadForSession> for DiscordBridgeSubscriber {
    async fn handle(&mut self, msg: CreateThreadForSession, _ctx: &mut MsgCtx<'_>) {
        self.forward_gateway_request(GatewayRequest::CreateThreadForSession {
            session_id: msg.session_id,
            title: msg.title,
        });
    }
}

impl MsgHandler<DiscordThreadCreated> for DiscordBridgeSubscriber {
    async fn handle(&mut self, msg: DiscordThreadCreated, _ctx: &mut MsgCtx<'_>) {
        self.handle_created(&msg);
    }
}

impl MsgHandler<DiscordThreadCreateFailed> for DiscordBridgeSubscriber {
    async fn handle(&mut self, msg: DiscordThreadCreateFailed, _ctx: &mut MsgCtx<'_>) {
        self.handle_failed(&msg);
    }
}

impl DiscordBridgeSubscriber {
    /// Constructs a subscriber instance directly (for tests that call
    /// the fold helpers; added in the Phase 4 test port).
    #[cfg(test)]
    pub(crate) fn new(
        tx: kanal::Sender<BridgeEvent>,
        state: State,
        session_cap: jinn_domain::common::tcaps::session::SessionCap,
    ) -> Self {
        let (gateway_tx, _gateway_rx) = kanal::bounded(1);
        Self {
            tx,
            gateway_tx,
            state,
            session_cap,
        }
    }
}

impl DiscordBridgeSubscriber {
    /// Forward phase changes to the gateway **only** when the new phase is
    /// `Idle`. Non-idle transitions (Streaming, Sending, …) are dropped.
    pub(super) fn handle_session_phase_changed(&self, payload: &SessionPhaseChanged) {
        if payload.new_phase != jinn_session_msg::PhaseKind::Idle {
            return;
        }
        self.forward(BridgeEvent::TurnFinished {
            session_id: payload.session_id.clone(),
        });
    }

    /// Forward every setup completion (success or failure — the gateway
    /// formats the message from `cwd`/`error`).
    pub(super) fn handle_session_setup_completed(&self, payload: &SessionSetupCompleted) {
        self.forward(BridgeEvent::SetupCompleted {
            session_id: payload.session_id.clone(),
            cwd: payload.cwd.clone(),
            error: payload.error.clone(),
        });
    }

    /// Forward every teardown completion (success or failure — the gateway
    /// formats the message from `error`).
    pub(super) fn handle_session_teardown_finished(&self, payload: &SessionTeardownFinished) {
        self.forward(BridgeEvent::TeardownFinished {
            session_id: payload.session_id.clone(),
            error: payload.error.clone(),
        });
    }

    /// Forward every archive completion to the gateway.
    pub(super) fn handle_session_archived(&self, payload: &SessionArchived) {
        self.forward(BridgeEvent::Archived {
            session_id: payload.session_id.clone(),
        });
    }

    // ── to-thread feedback (reverse: gateway → jinn session history) ─────

    /// Handle `DiscordThreadCreated`: push a system `ChatEntry` mentioning the title.
    pub(super) fn handle_created(&self, msg: &DiscordThreadCreated) {
        let entry = ChatEntry::system(format!("Continuing in Discord thread: {}", msg.title));
        push_entry(&self.state, self.session_cap, &msg.session_id, entry);
    }

    /// Handle `DiscordThreadCreateFailed`: push an error `ChatEntry`.
    pub(super) fn handle_failed(&self, msg: &DiscordThreadCreateFailed) {
        let entry = ChatEntry::error(reason_message(&msg.reason));
        push_entry(&self.state, self.session_cap, &msg.session_id, entry);
    }

    /// Push one event onto the channel.
    ///
    /// A full channel means the gateway task is behind; rather than block the
    /// topic dispatch we drop with a warning. The next `Idle`/setup event
    /// will still arrive and trigger a fresh read from `State`.
    fn forward(&self, event: BridgeEvent) {
        tracing::info!(event = %event_discriminant(&event), "discord bridge forwarding");
        if !matches!(self.tx.try_send(event), Ok(true)) {
            tracing::warn!("discord bridge channel full — event dropped");
        }
    }

    /// Push one gateway request onto the request channel.
    ///
    /// Same drop-on-full semantics as [`forward`](Self::forward) — a full
    /// channel means the gateway task is behind, so we drop with a warning
    /// rather than block the topic dispatch.
    fn forward_gateway_request(&self, request: GatewayRequest) {
        tracing::info!("discord bridge forwarding gateway request");
        if !matches!(self.gateway_tx.try_send(request), Ok(true)) {
            tracing::warn!("discord gateway request channel full — request dropped");
        }
    }
}

/// Short label identifying a [`BridgeEvent`] variant for log lines.
///
/// The events themselves may carry large payloads (session ids are fine,
/// but keeping a single helper avoids per-arm `Display` requirements).
fn event_discriminant(event: &BridgeEvent) -> &'static str {
    match event {
        BridgeEvent::SetupCompleted { .. } => "SetupCompleted",
        BridgeEvent::TurnFinished { .. } => "TurnFinished",
        BridgeEvent::TeardownFinished { .. } => "TeardownFinished",
        BridgeEvent::Archived { .. } => "Archived",
    }
}

/// Push a `ChatEntry` into a session by id; drop silently if the session is
/// gone (closed/archived concurrently since the `gdc` request was emitted).
fn push_entry(
    state: &State,
    session_cap: jinn_domain::common::tcaps::session::SessionCap,
    session_id: &SessionId,
    entry: ChatEntry,
) {
    state.with_session(&session_cap, |view| {
        if let Some(session) = view.session.map().get_mut(session_id) {
            session.push_entry(entry);
        } else {
            tracing::debug!(
                %session_id,
                "to-thread result arrived for a session that no longer exists; dropping",
            );
        }
    });
}

/// Render a human-readable message for each failure reason.
fn reason_message(reason: &CreateThreadReason) -> String {
    match reason {
        CreateThreadReason::AlreadyBound => concat!(
            "Can't continue in Discord: this session is already in a Discord ",
            "thread — continue there."
        )
        .to_owned(),
        CreateThreadReason::ForumChannel(ForumChannelError::Missing) => concat!(
            "Can't continue in Discord: no `forum_channel` is set in ",
            "`[discord]`. Set it to the numeric channel id (snowflake) ",
            "of a `GUILD_FORUM` channel the bot can manage."
        )
        .to_owned(),
        CreateThreadReason::ForumChannel(ForumChannelError::Invalid { value }) => {
            format!(
                "Can't continue in Discord: `forum_channel` must be a numeric channel id (snowflake), but it's set to `{value}`. Copy the channel id in Discord (right-click → Copy Channel ID) and paste it into `[discord] forum_channel`."
            )
        }
        CreateThreadReason::CreateFailed(detail) => {
            format!("Couldn't create the Discord thread: {detail}")
        }
        CreateThreadReason::MappingWriteFailed => concat!(
            "Discord thread was created, but jinn couldn't record the binding — ",
            "the thread exists but won't receive replies. See the logs."
        )
        .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::protocol::ChatEntryKind;
    use jinn_domain::protocol::SessionId;

    /// Build a bridge subscriber with one seeded session, plus its session id.
    ///
    /// The `tx` channel is a throwaway — these tests exercise the to-thread
    /// feedback handlers, not the forwarding path.
    fn subscriber_with_session() -> (DiscordBridgeSubscriber, SessionId) {
        let (tx, _rx) = kanal::bounded(1);
        let state = State::new(AppState::default());
        let session_id = SessionId::new();
        // Seed the session so `push_entry` finds it.
        state.with_session(&jinn_domain::common::tcaps::mint::mint_session_cap(), |v| {
            v.session.map().get_or_create(&session_id);
        });
        let subscriber = DiscordBridgeSubscriber::new(
            tx,
            state,
            jinn_domain::common::tcaps::mint::mint_session_cap(),
        );
        (subscriber, session_id)
    }

    /// `reason_message` for `AlreadyBound` mentions continuing in the existing thread.
    #[rstest::rstest]
    #[test]
    fn reason_message_already_bound_is_descriptive() {
        let msg = reason_message(&CreateThreadReason::AlreadyBound);
        assert!(msg.contains("already in a Discord thread"));
    }

    /// `reason_message` for `ForumChannel(Missing)` explains how to set the field.
    #[rstest::rstest]
    #[test]
    fn reason_message_forum_channel_missing_explains_how_to_set() {
        let msg = reason_message(&CreateThreadReason::ForumChannel(
            ForumChannelError::Missing,
        ));
        assert!(msg.contains("no `forum_channel` is set"));
        assert!(msg.contains("snowflake"));
        assert!(msg.contains("GUILD_FORUM"));
    }

    /// `reason_message` for `ForumChannel(Invalid)` shows the bad value and what
    /// a snowflake looks like.
    #[rstest::rstest]
    #[test]
    fn reason_message_forum_channel_invalid_shows_bad_value() {
        let msg = reason_message(&CreateThreadReason::ForumChannel(
            ForumChannelError::Invalid {
                value: "sessions".to_owned(),
            },
        ));
        assert!(
            msg.contains("`sessions`"),
            "expected the bad value in the message: {msg}"
        );
        assert!(msg.contains("snowflake"));
        assert!(msg.contains("Copy Channel ID"));
    }

    /// `reason_message` for `CreateFailed` includes the Discord error detail.
    #[rstest::rstest]
    #[test]
    fn reason_message_create_failed_includes_detail() {
        let msg = reason_message(&CreateThreadReason::CreateFailed("boom".to_owned()));
        assert!(msg.contains("boom"));
    }

    /// `reason_message` for `MappingWriteFailed` describes the orphaned-thread state.
    #[rstest::rstest]
    #[test]
    fn reason_message_mapping_write_failed_describes_orphan() {
        let msg = reason_message(&CreateThreadReason::MappingWriteFailed);
        assert!(msg.contains("won't receive replies"));
    }

    /// A `Created` event pushes a system entry mentioning the title.
    #[rstest::rstest]
    #[test]
    fn created_pushes_system_entry_with_title() {
        // Given a subscriber with one session.
        let (subscriber, session_id) = subscriber_with_session();

        // When handling a Created event.
        subscriber.handle_created(&DiscordThreadCreated {
            session_id: session_id.clone(),
            title: "My Cool Session".to_owned(),
        });

        // Then the session's last history entry is a System entry with the title.
        let guard = subscriber.state.read();
        let last = guard.session(&session_id).history().last().expect("entry");
        assert!(matches!(last.kind, ChatEntryKind::System(_)));
        assert!(last.text().contains("My Cool Session"));
    }

    /// A `Failed(AlreadyBound)` event pushes an error entry.
    #[rstest::rstest]
    #[test]
    fn failed_already_bound_pushes_error_entry() {
        // Given a subscriber with one session.
        let (subscriber, session_id) = subscriber_with_session();

        // When handling a Failed(AlreadyBound) event.
        subscriber.handle_failed(&DiscordThreadCreateFailed {
            session_id: session_id.clone(),
            reason: CreateThreadReason::AlreadyBound,
        });

        // Then the session's last history entry is an Error entry.
        let guard = subscriber.state.read();
        let last = guard.session(&session_id).history().last().expect("entry");
        assert!(matches!(last.kind, ChatEntryKind::Error(_)));
    }

    /// A result for a session that doesn't exist is dropped, not panicked.
    #[rstest::rstest]
    #[test]
    fn result_for_missing_session_is_dropped() {
        // Given a state with no sessions.
        let state = State::new(AppState::default());
        let session_id = SessionId::new();

        // When pushing an entry for a session that doesn't exist.
        push_entry(
            &state,
            jinn_domain::common::tcaps::mint::mint_session_cap(),
            &session_id,
            ChatEntry::system("nope"),
        );

        // Then no panic occurred (reaching here is the assertion).
    }

    // ── trouper transport tests ───────────────────────────────────────
    //
    // The fold tests above call the handlers directly. They prove the
    // folding logic works but NOT that the subscriber is wired to the
    // `jinn.session` topic end to end. A dropped `handles::<M>()` call
    // (the bug this port could introduce) would pass every one of those
    // tests. The tests below publish through the trouper system and
    // assert the subscriber's output channels, so they fail if any
    // subscription is dropped.

    use jinn_session_msg::PhaseKind;
    use std::time::Duration;

    #[rstest::rstest]
    #[tokio::test]
    async fn spawned_subscriber_forwards_turn_finished_from_topic() {
        // Given a subscriber spawned against a real trouper system, with
        // its bridge channel drained here.
        let fabric = jinn_testutil::TestFabric::new();
        let (tx, rx) = kanal::bounded::<BridgeEvent>(8);
        let (gw_tx, _gw_rx) = kanal::bounded::<GatewayRequest>(4);
        DiscordBridgeSubscriber::spawn(
            fabric.system(),
            DiscordBridgeSubscriberDeps {
                tx,
                gateway_tx: gw_tx,
                state: State::new(AppState::default()),
                session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
            },
        );
        let sid = SessionId::new();

        // When an Idle phase change is published on the session topic.
        fabric
            .send_to_topic(
                &SessionPhaseChanged {
                    session_id: sid.clone(),
                    old_phase: PhaseKind::Streaming,
                    new_phase: PhaseKind::Idle,
                },
                &jinn_session_msg::session_topic(),
            )
            .await;

        // Then exactly one TurnFinished was forwarded.
        let event = rx.to_async().recv().await.expect("event forwarded");
        match event {
            BridgeEvent::TurnFinished { session_id } => {
                assert_eq!(session_id, sid);
            }
            other => panic!("expected TurnFinished, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn spawned_subscriber_ignores_non_idle_phase_changes() {
        // Given a spawned subscriber.
        let fabric = jinn_testutil::TestFabric::new();
        let (tx, rx) = kanal::bounded::<BridgeEvent>(8);
        let (gw_tx, _gw_rx) = kanal::bounded::<GatewayRequest>(4);
        DiscordBridgeSubscriber::spawn(
            fabric.system(),
            DiscordBridgeSubscriberDeps {
                tx,
                gateway_tx: gw_tx,
                state: State::new(AppState::default()),
                session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
            },
        );
        let sid = SessionId::new();

        // When a non-idle phase change is published on the session topic.
        fabric
            .send_to_topic(
                &SessionPhaseChanged {
                    session_id: sid.clone(),
                    old_phase: PhaseKind::Idle,
                    new_phase: PhaseKind::Streaming,
                },
                &jinn_session_msg::session_topic(),
            )
            .await;

        // Then nothing is forwarded within a settle window.
        let mut settled = false;
        for _ in 0..50 {
            if matches!(rx.try_recv(), Ok(None)) {
                settled = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(settled, "non-idle transition must not forward");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn spawned_subscriber_forwards_setup_completed_from_topic() {
        // Given a spawned subscriber.
        let fabric = jinn_testutil::TestFabric::new();
        let (tx, rx) = kanal::bounded::<BridgeEvent>(8);
        let (gw_tx, _gw_rx) = kanal::bounded::<GatewayRequest>(4);
        DiscordBridgeSubscriber::spawn(
            fabric.system(),
            DiscordBridgeSubscriberDeps {
                tx,
                gateway_tx: gw_tx,
                state: State::new(AppState::default()),
                session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
            },
        );
        let sid = SessionId::new();

        // When a failed setup completion is published on the session topic.
        fabric
            .send_to_topic(
                &SessionSetupCompleted {
                    session_id: sid.clone(),
                    cwd: std::path::PathBuf::from("/repo"),
                    error: Some("boom".to_owned()),
                },
                &jinn_session_msg::session_topic(),
            )
            .await;

        // Then exactly one SetupCompleted was forwarded with the payload.
        let event = rx.to_async().recv().await.expect("event forwarded");
        match event {
            BridgeEvent::SetupCompleted {
                session_id,
                cwd,
                error,
            } => {
                assert_eq!(session_id, sid);
                assert_eq!(cwd, std::path::PathBuf::from("/repo"));
                assert_eq!(error.as_deref(), Some("boom"));
            }
            other => panic!("expected SetupCompleted, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn spawned_subscriber_forwards_teardown_finished_from_topic() {
        // Given a spawned subscriber.
        let fabric = jinn_testutil::TestFabric::new();
        let (tx, rx) = kanal::bounded::<BridgeEvent>(8);
        let (gw_tx, _gw_rx) = kanal::bounded::<GatewayRequest>(4);
        DiscordBridgeSubscriber::spawn(
            fabric.system(),
            DiscordBridgeSubscriberDeps {
                tx,
                gateway_tx: gw_tx,
                state: State::new(AppState::default()),
                session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
            },
        );
        let sid = SessionId::new();

        // When a failed teardown finish is published on the session topic.
        fabric
            .send_to_topic(
                &SessionTeardownFinished {
                    session_id: sid.clone(),
                    error: Some("boom".to_owned()),
                },
                &jinn_session_msg::session_topic(),
            )
            .await;

        // Then exactly one TeardownFinished was forwarded with the payload.
        let event = rx.to_async().recv().await.expect("event forwarded");
        match event {
            BridgeEvent::TeardownFinished { session_id, error } => {
                assert_eq!(session_id, sid);
                assert_eq!(error.as_deref(), Some("boom"));
            }
            other => panic!("expected TeardownFinished, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn spawned_subscriber_forwards_archived_from_topic() {
        // Given a spawned subscriber.
        let fabric = jinn_testutil::TestFabric::new();
        let (tx, rx) = kanal::bounded::<BridgeEvent>(8);
        let (gw_tx, _gw_rx) = kanal::bounded::<GatewayRequest>(4);
        DiscordBridgeSubscriber::spawn(
            fabric.system(),
            DiscordBridgeSubscriberDeps {
                tx,
                gateway_tx: gw_tx,
                state: State::new(AppState::default()),
                session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
            },
        );
        let sid = SessionId::new();

        // When an archive event is published on the session topic.
        fabric
            .send_to_topic(
                &SessionArchived {
                    session_id: sid.clone(),
                },
                &jinn_session_msg::session_topic(),
            )
            .await;

        // Then exactly one Archived was forwarded.
        let event = rx.to_async().recv().await.expect("event forwarded");
        match event {
            BridgeEvent::Archived { session_id } => {
                assert_eq!(session_id, sid);
            }
            other => panic!("expected Archived, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn spawned_subscriber_routes_create_thread_to_gateway_channel() {
        // Given a spawned subscriber.
        let fabric = jinn_testutil::TestFabric::new();
        let (tx, _rx) = kanal::bounded::<BridgeEvent>(8);
        let (gw_tx, gw_rx) = kanal::bounded::<GatewayRequest>(4);
        DiscordBridgeSubscriber::spawn(
            fabric.system(),
            DiscordBridgeSubscriberDeps {
                tx,
                gateway_tx: gw_tx,
                state: State::new(AppState::default()),
                session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
            },
        );
        let sid = SessionId::new();

        // When a CreateThreadForSession command is published on the session topic.
        fabric
            .send_to_topic(
                &CreateThreadForSession {
                    session_id: sid.clone(),
                    title: "my thread".to_owned(),
                },
                &jinn_session_msg::session_topic(),
            )
            .await;

        // Then exactly one GatewayRequest::CreateThreadForSession landed on
        // the gateway-request channel.
        let request = gw_rx.to_async().recv().await.expect("request forwarded");
        let GatewayRequest::CreateThreadForSession { session_id, title } = request;
        assert_eq!(session_id, sid);
        assert_eq!(title, "my thread");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn spawned_subscriber_writes_thread_created_into_session_history() {
        // Given a spawned subscriber whose state holds one seeded session.
        let fabric = jinn_testutil::TestFabric::new();
        let (tx, _rx) = kanal::bounded::<BridgeEvent>(8);
        let (gw_tx, _gw_rx) = kanal::bounded::<GatewayRequest>(4);
        let state = State::new(AppState::default());
        let sid = SessionId::new();
        let cap = jinn_domain::common::tcaps::mint::mint_session_cap();
        state.with_session(&cap, |v| {
            v.session.map().get_or_create(&sid);
        });
        DiscordBridgeSubscriber::spawn(
            fabric.system(),
            DiscordBridgeSubscriberDeps {
                tx,
                gateway_tx: gw_tx,
                state: state.clone(),
                session_cap: cap,
            },
        );

        // When the gateway's thread-created event crosses the session topic.
        fabric
            .send_to_topic(
                &DiscordThreadCreated {
                    session_id: sid.clone(),
                    title: "Threaded".to_owned(),
                },
                &jinn_session_msg::session_topic(),
            )
            .await;

        // Then the session's history gained a System entry mentioning the title.
        let last = {
            let mut found = None;
            for _ in 0..200 {
                let guard = state.read();
                if let Some(entry) = guard.session(&sid).history().last() {
                    found = Some(entry.text().to_owned());
                    break;
                }
                drop(guard);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            found.expect("history entry written")
        };
        assert!(last.contains("Threaded"), "entry text: {last}");
    }
}
