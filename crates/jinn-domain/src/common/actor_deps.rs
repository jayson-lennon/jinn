//! Universal actor dependencies.
//!
//! [`ActorDeps`] bundles the dependencies that every kameo actor needs.
//! Each actor's `Args`/`Deps` struct includes this as a field, so adding
//! a new common dependency (e.g., shutdown signal, actor host ref) only
//! requires changing this one struct instead of 30+ Args structs.
//!
//! The [`BusPublish`] extension trait lets actors call `self.publish(msg).await`
//! instead of drilling through `self.deps.services.bus.publish(msg)`.

use crate::Services;

use super::services::bus_service::BusService;

/// Universal dependencies injected into every kameo actor's `Args`.
///
/// Wrap this in each actor's specific `Args` struct alongside
/// actor-specific fields:
///
/// ```ignore
/// struct MyActorArgs {
///     deps: ActorDeps,
///     session_id: SessionId,
/// }
/// ```
#[derive(Clone, Debug)]
pub struct ActorDeps {
    /// Application-wide runtime services (bus, config store, LLM factory, etc.).
    pub services: Services,
}

impl ActorDeps {
    /// Returns a reference to the bus service.
    pub fn bus(&self) -> &BusService {
        &self.services.bus
    }

    /// Register a [`Recipient`] on the bus for a specific message type.
    ///
    /// Use [`Self::subscribe_recipient`] if you need to subscribe to
    /// multiple message types from the same actor — call `actor_ref.clone().recipient::<M>()`
    /// to obtain each recipient without consuming the original `actor_ref`.
    ///
    /// ```ignore
    /// // Single message type:
    /// args.deps.subscribe(actor_ref.recipient::<MyMessage>()).await;
    ///
    /// // Multiple message types from the same actor:
    /// args.deps.subscribe(actor_ref.clone().recipient::<Msg1>()).await;
    /// args.deps.subscribe(actor_ref.clone().recipient::<Msg2>()).await;
    /// args.deps.subscribe(actor_ref.recipient::<Msg3>()).await; // last one can consume
    /// ```
    pub async fn subscribe<M: Clone + Send + 'static>(
        &self,
        recipient: kameo::prelude::Recipient<M>,
    ) {
        self.services.bus.register(recipient).await;
    }

    /// Convenience: publish a typed message to the bus.
    ///
    /// ```ignore
    /// self.deps.publish(MyMessage { ... }).await;
    /// ```
    pub async fn publish<M: crate::common::bus::BusMessage + trouper::schema::Schema + serde::Serialize>(
        &self,
        msg: M,
    ) {
        self.services.bus.publish(msg).await;
    }
}

// ── Extension trait ──────────────────────────────────────────────────────

/// Extension trait for publishing messages from kameo actors.
///
/// Implement on your actor struct to get `self.publish(msg).await`:
///
/// ```ignore
/// impl BusPublish for MyActor {
///     fn bus(&self) -> &BusService {
///         &self.deps.services.bus
///     }
/// }
/// ```
///
/// Then inside any handler:
/// ```ignore
/// self.publish(PushChatEntry { ... }).await;
/// ```
pub trait BusPublish {
    /// Returns a reference to the bus service.
    /// Typically delegates to `&self.deps.services.bus`.
    fn bus(&self) -> &BusService;

    /// Publish a typed message to the bus.
    ///
    /// Fire-and-forget: logs a warning on delivery failure but does not propagate.
    fn publish<M: crate::common::bus::BusMessage + trouper::schema::Schema + serde::Serialize>(
        &self,
        msg: M,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        let bus = self.bus().clone();
        Box::pin(async move {
            bus.publish(msg).await;
        })
    }

    /// Register a recipient on the bus.
    ///
    /// Convenience wrapper around `BusService::register`.
    fn bus_register<M: Clone + Send + 'static>(
        &self,
        recipient: kameo::actor::Recipient<M>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        let bus = self.bus().clone();
        Box::pin(async move {
            bus.register(recipient).await;
        })
    }
}
