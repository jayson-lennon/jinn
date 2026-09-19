//! Test harness for bus-based actor tests.
//!
//! Provides a [`TestHarness`] that spawns a `MessageBus` and offers convenience
//! methods for spawning actors, recorders, and publishing messages — eliminating
//! boilerplate from individual test functions.
#![allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "test harness"
)]

use std::marker::PhantomData;
use std::time::Duration;

use kameo::actor::{ActorRef, Spawn};
use kameo::prelude::{Context, Message};
use kameo_actors::message_bus::Register;

use crate::common::bus::BusMessage;
use crate::common::services::bus_service::BusService;

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

/// A test fixture that manages the message fabric and provides convenience
/// methods for spawning actors and recorders in tests.
pub struct TestHarness {
    bus: BusService,
    system: trouper::system::ActorSystem,
}

impl TestHarness {
    /// Create a new harness with a fresh fabric: a trouper `ActorSystem`
    /// (primary leg) plus a `Guaranteed`-delivery kameo leg feeding the
    /// not-yet-ported actors.
    // API symmetry with other harness methods; spawn requires runtime context.
    pub async fn new() -> Self {
        async {}.await;
        let system =
            trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
        let bus_actor =
            kameo_actors::message_bus::MessageBus::new(kameo_actors::DeliveryStrategy::Guaranteed);
        let bus_ref = Spawn::spawn(bus_actor);
        let bus = BusService::new_trouper(system.clone(), Some(bus_ref));
        Self { bus, system }
    }

    /// Create a new harness with a `BestEffort` kameo leg.
    ///
    /// Production uses `BestEffort` on the kameo leg, which drops messages on
    /// `MailboxFull` (the bus silently swallows them — it only checks
    /// `ActorNotRunning`). Use this variant to faithfully reproduce
    /// drop-driven wedges that the default `Guaranteed` harness cannot
    /// trigger. The trouper leg is unaffected (its inboxes backpressure).
    #[expect(
        clippy::unused_async,
        reason = "API symmetry with `new`; spawn requires runtime context"
    )]
    pub async fn new_best_effort() -> Self {
        let system =
            trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
        let bus_actor =
            kameo_actors::message_bus::MessageBus::new(kameo_actors::DeliveryStrategy::BestEffort);
        let bus_ref = Spawn::spawn(bus_actor);
        let bus = BusService::new_trouper(system.clone(), Some(bus_ref));
        Self { bus, system }
    }

    /// The wrapped `BusService` — pass to actor deps.
    pub fn bus(&self) -> BusService {
        self.bus.clone()
    }

    /// The harness's trouper system — spawn ported actors against it.
    #[must_use]
    pub const fn system(&self) -> &trouper::system::ActorSystem {
        &self.system
    }

    /// Assembles a harness from pre-built parts (fabric tests that hand-
    /// construct the `BusService` to control its legs).
    #[must_use]
    pub fn from_parts(bus: BusService, system: trouper::system::ActorSystem) -> Self {
        Self { bus, system }
    }

    /// Publish a typed message on the fabric (the `BusService`'s legs).
    pub async fn publish<M: BusMessage + trouper::schema::Schema + serde::Serialize>(
        &self,
        msg: M,
    ) {
        self.bus.publish(msg).await;
    }

    /// Spawn a kameo actor and wait for startup (bus registration complete).
    pub async fn spawn_actor<A: kameo::Actor>(&self, args: A::Args) -> ActorRef<A> {
        let actor = A::spawn(args);
        actor.wait_for_startup().await;
        actor
    }

    /// Spawn a kameo actor with a custom mailbox (e.g. unbounded) and wait for
    /// startup. Mirrors `spawn_actor` but lets the caller opt out of the default
    /// bounded(64) mailbox.
    pub async fn spawn_actor_with_mailbox<A: kameo::Actor>(
        &self,
        args: A::Args,
        mailbox: (
            kameo::mailbox::MailboxSender<A>,
            kameo::mailbox::MailboxReceiver<A>,
        ),
    ) -> ActorRef<A>
    where
        A::Args: Clone + Sync,
    {
        let actor = Spawn::spawn_with_mailbox(args, mailbox);
        actor.wait_for_startup().await;
        actor
    }

    /// Spawn a [`Recorder`] for type `M` on the **trouper leg** only: the
    /// fabric's primary path. A `harness.publish::<M>()` round-trips through
    /// `BusService`, its topic routing, and the schema adapter before the
    /// recorder sees the decoded message — tests validate the real delivery
    /// path. (Kameo-emitters' direct `tell`s to recorders are legacy kameo
    /// delivery; during the coexistence window recorders collect from both
    /// the trouper tap and any kameo `Register` a test performs itself.)
    pub async fn spawn_recorder<M>(&self) -> ActorRef<Recorder<M>>
    where
        M: BusMessage + trouper::schema::Schema + serde::Serialize + serde::de::DeserializeOwned,
    {
        let recorder = Recorder::<M>::spawn(());
        // Troupe leg: a service actor observing the schema's routed
        // deliveries, forwarding decoded messages into the same recorder.
        self.spawn_trouper_recorder::<M>(recorder.clone()).await;
        recorder
    }

    /// Spawns the trouper-side recorder actor subscribed to `M`'s schema
    /// traffic, forwarding decoded messages into the harness's [`Recorder`].
    ///
    /// Each tap gets a unique path (a process-wide counter) so parallel
    /// tests never share tap state; the recorder handle rides a process-
    /// wide registry because trouper's typed `start` only carries JSON args.
    async fn spawn_trouper_recorder<M>(&self, recorder: ActorRef<Recorder<M>>)
    where
        M: BusMessage + trouper::schema::Schema + serde::Serialize + serde::de::DeserializeOwned,
    {
        use std::collections::HashMap;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        use trouper::actor::{ActorPath, MsgHandler, ServiceActor};

        static TAP_SEQ: AtomicU64 = AtomicU64::new(0);

        fn recorders()
        -> &'static parking_lot::Mutex<HashMap<String, Arc<dyn std::any::Any + Send + Sync>>> {
            static TAP_RECORDERS: std::sync::OnceLock<
                parking_lot::Mutex<HashMap<String, Arc<dyn std::any::Any + Send + Sync>>>,
            > = std::sync::OnceLock::new();
            TAP_RECORDERS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
        }

        struct TroupeTap<M> {
            path: ActorPath,
            _msg: std::marker::PhantomData<fn() -> M>,
        }

        impl<M> TroupeTap<M>
        where
            M: BusMessage + trouper::schema::Schema + serde::Serialize + serde::de::DeserializeOwned,
        {
            fn path_for(seq: u64) -> ActorPath {
                ActorPath::new(format!(
                    "test.tap.{}.{}",
                    M::schema_id().name().replace("::", "."),
                    seq
                ))
            }
        }

        impl<M> ServiceActor for TroupeTap<M>
        where
            M: BusMessage + trouper::schema::Schema + serde::Serialize + serde::de::DeserializeOwned,
        {
            async fn start(
                args: &serde_json::Value,
            ) -> Result<Self, error_stack::Report<trouper::registry::RegistryError>> {
                // The spawner passes the tap's own path through the args so
                // `start` never has to recompute (or race on) it.
                let path = args["path"].as_str().expect("tap path arg").to_owned();
                Ok(Self {
                    path: ActorPath::new(path),
                    _msg: std::marker::PhantomData,
                })
            }
        }

        impl<M> MsgHandler<M> for TroupeTap<M>
        where
            M: BusMessage + trouper::schema::Schema + serde::Serialize + serde::de::DeserializeOwned,
        {
            async fn handle(&mut self, msg: M, _ctx: &mut trouper::context::MsgCtx<'_>) {
                let recorder = {
                    let table = recorders().lock();
                    table
                        .get(self.path.as_str())
                        .cloned()
                        .and_then(|any| {
                            any.downcast::<ActorRef<Recorder<M>>>()
                                .ok()
                                .map(|arc| (*arc).clone())
                        })
                };
                if let Some(recorder) = recorder {
                    let _ = recorder.tell(msg).await;
                }
            }
        }

        // Reserve the sequence slot first so each parallel test's tap gets
        // a distinct path.
        let seq = TAP_SEQ.fetch_add(1, Ordering::SeqCst);
        let path = TroupeTap::<M>::path_for(seq);
        recorders().lock().insert(path.as_str().to_owned(), Arc::new(recorder));
        self.system.register_schema::<M>();
        self.system.spawn_service::<TroupeTap<M>, _>(
            path.clone(),
            &serde_json::json!({ "path": path.as_str() }),
            trouper::system::SpawnOpts::default(),
            || {
                vec![std::sync::Arc::new(
                    trouper::actor::TypedServiceAdapter::<TroupeTap<M>, M>::new::<M>(),
                )]
            },
        );
        // The tap follows the schema's current route resolution: normally
        // the shared domain topic, or the override a test/slice route
        // registered for `M`.
        let topic = self.bus.routed_topic::<M>();
        self.system
            .subscribe(&path, &topic, None)
            .expect("tap subscribes the schema's routed topic");
    }

    /// Register a custom actor's recipient for type `M` on the kameo leg.
    pub async fn register<M: Clone + Send + 'static>(
        &self,
        recipient: kameo::actor::Recipient<M>,
    ) {
        if let Some(leg) = self.bus.kameo_leg_ref() {
            let _ = leg.ask(Register(recipient)).await;
        }
    }
    /// Build a [`Services`] with the harness bus wired into a test instance.
    ///
    /// This creates a `Services::new_fake()` and replaces its bus with the harness bus,
    /// so actors use the same bus the test is publishing to.
    pub async fn services(&self) -> crate::Services {
        let mut services = crate::Services::new_fake().await;
        services.bus = self.bus.clone();
        services
    }

    /// Build an [`ActorDeps`] with the harness bus wired into a test [`Services`].
    ///
    /// This creates a `Services::new()` and replaces its bus with the harness bus,
    /// so actors use the same bus the test is publishing to.
    pub async fn actor_deps(&self) -> crate::common::actor_deps::ActorDeps {
        let mut services = crate::Services::new_fake().await;
        services.bus = self.bus.clone();
        crate::common::actor_deps::ActorDeps { services }
    }
}

// ---------------------------------------------------------------------------
// await_recorded helper
// ---------------------------------------------------------------------------

/// Poll a [`Recorder`] until it has collected at least `min_count` messages, or
/// the timeout expires. Returns whatever has been collected (may be fewer than
/// `min_count` on timeout — the test assertion will then fail with a clear message).
pub async fn await_recorded<M: Clone + Send + 'static>(
    recorder: &ActorRef<Recorder<M>>,
    min_count: usize,
    timeout: Duration,
) -> Vec<M> {
    let deadline = tokio::time::Instant::now() + timeout;
    // `GetRecorded` drains the recorder, so every poll's messages must be
    // kept: a burst split across polls would otherwise be discarded piecemeal
    // below `min_count`. Accumulate until the minimum is met.
    let mut collected: Vec<M> = Vec::new();
    loop {
        collected.extend(
            recorder
                .ask(GetRecorded::new())
                .await
                .expect("get recorded"),
        );
        if collected.len() >= min_count {
            return collected;
        }
        if tokio::time::Instant::now() >= deadline {
            return collected;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ---------------------------------------------------------------------------
// Recorder actor
// ---------------------------------------------------------------------------

/// Query to retrieve collected messages from a [`Recorder`].
pub struct GetRecorded<M> {
    _phantom: PhantomData<M>,
}

impl<M> GetRecorded<M> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

/// A simple recorder actor that collects messages of type `M`.
/// Retrieve them with `recorder.ask(GetRecorded::<M>::new())`.
pub struct Recorder<M> {
    messages: Vec<M>,
}

impl<M: Send + 'static> kameo::Actor for Recorder<M> {
    type Args = ();
    type Error = kameo::error::Infallible;

    async fn on_start(_args: Self::Args, _actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        Ok(Self {
            messages: Vec::new(),
        })
    }
}

impl<M: Clone + Send + 'static> Message<M> for Recorder<M> {
    type Reply = ();

    async fn handle(&mut self, msg: M, _ctx: &mut Context<Self, Self::Reply>) {
        self.messages.push(msg);
    }
}

impl<M: Clone + Send + 'static> Message<GetRecorded<M>> for Recorder<M> {
    type Reply = Vec<M>;

    async fn handle(
        &mut self,
        _msg: GetRecorded<M>,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        std::mem::take(&mut self.messages)
    }
}
