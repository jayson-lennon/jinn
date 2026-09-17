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
use kameo_actors::message_bus::{Publish, Register};

use crate::common::services::bus_service::BusService;

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

/// A test fixture that manages a [`MessageBus`] and provides convenience methods
/// for spawning actors and recorders in tests.
pub struct TestHarness {
    bus: BusService,
    bus_ref: ActorRef<kameo_actors::message_bus::MessageBus>,
}

impl TestHarness {
    /// Create a new harness with a fresh `MessageBus`.
    // API symmetry with other harness methods; spawn requires runtime context.
    pub async fn new() -> Self {
        async {}.await;
        let bus =
            kameo_actors::message_bus::MessageBus::new(kameo_actors::DeliveryStrategy::Guaranteed);
        let bus_ref = Spawn::spawn(bus);
        let bus = BusService::new(bus_ref.clone());
        Self { bus, bus_ref }
    }

    /// Create a new harness with a `BestEffort` delivery bus.
    ///
    /// Production uses `BestEffort`, which drops messages on `MailboxFull` (the bus
    /// silently swallows them — it only checks `ActorNotRunning`). Use this variant
    /// to faithfully reproduce drop-driven wedges that the default `Guaranteed`
    /// harness cannot trigger.
    #[expect(
        clippy::unused_async,
        reason = "API symmetry with `new`; spawn requires runtime context"
    )]
    pub async fn new_best_effort() -> Self {
        let bus =
            kameo_actors::message_bus::MessageBus::new(kameo_actors::DeliveryStrategy::BestEffort);
        let bus_ref = Spawn::spawn(bus);
        let bus = BusService::new(bus_ref.clone());
        Self { bus, bus_ref }
    }

    /// The wrapped `BusService` — pass to actor deps.
    pub fn bus(&self) -> BusService {
        self.bus.clone()
    }

    /// Publish a typed message on the bus.
    pub async fn publish<M: Clone + Send + 'static>(&self, msg: M) {
        self.bus_ref
            .tell(Publish(msg))
            .await
            .expect("publish should succeed");
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

    /// Spawn a [`Recorder`] and register it on the bus for type `M`.
    pub async fn spawn_recorder<M: Clone + Send + 'static>(&self) -> ActorRef<Recorder<M>> {
        let recorder = Recorder::<M>::spawn(());
        self.bus_ref
            .ask(Register(recorder.clone().recipient::<M>()))
            .await
            .expect("register recorder");
        recorder
    }

    /// Register a custom actor's recipient for type `M` on the bus.
    pub async fn register<M: Clone + Send + 'static>(&self, recipient: kameo::actor::Recipient<M>) {
        self.bus_ref
            .ask(Register(recipient))
            .await
            .expect("register recipient");
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
