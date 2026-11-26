//! Every open WebSocket connection, and the one way a frame reaches them.
//!
//! Each connection's socket task owns a receiver; the registry holds the
//! sender under the connection's id. `broadcast` is the single mechanism
//! every pushed `ServerMsg` leaves through. A connection leaves the
//! registry the moment its socket closes — its guard unregisters on drop —
//! and never lingers until a later broadcast fails against it.

use std::sync::Arc;

use jinn_webui2_msg::ServerMsg;
use parking_lot::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// One connection's id in the registry: assigned at registration, never
/// reused, and the handle a connection's guard unregisters by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(u64);

#[derive(Debug, Default)]
struct Inner {
    next_id: u64,
    connections: std::collections::HashMap<ConnectionId, UnboundedSender<ServerMsg>>,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionRegistry {
    inner: Arc<Mutex<Inner>>,
}

impl ConnectionRegistry {
    /// Adds a connection and returns its id with the receiver its socket
    /// task reads. The id is live until the returned guard is dropped.
    pub fn register(&self) -> (ConnectionId, UnboundedReceiver<ServerMsg>, ConnectionGuard) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut inner = self.inner.lock();
        let id = ConnectionId(inner.next_id);
        inner.next_id += 1;
        inner.connections.insert(id, tx);
        let guard = ConnectionGuard {
            registry: self.clone(),
            id: Some(id),
        };
        (id, rx, guard)
    }

    /// Removes a connection. Its guard calls this on drop; a second call
    /// for the same id does nothing.
    pub fn unregister(&self, id: ConnectionId) {
        self.inner.lock().connections.remove(&id);
    }

    /// Sends one frame to every open connection, dropping any whose
    /// receiver has gone.
    pub fn broadcast(&self, msg: &ServerMsg) {
        let mut inner = self.inner.lock();
        inner
            .connections
            .retain(|_, tx| tx.send(msg.clone()).is_ok());
    }

    /// How many connections are currently open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().connections.len()
    }

    /// Whether no connection is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A registered connection's lifetime: dropping it unregisters the
/// connection, so a socket task cannot exit and leave its sender behind.
#[derive(Debug)]
pub struct ConnectionGuard {
    registry: ConnectionRegistry,
    id: Option<ConnectionId>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.registry.unregister(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ping() -> ServerMsg {
        ServerMsg::Ping
    }

    #[rstest::rstest]
    #[test]
    fn a_registered_connection_receives_a_broadcast() {
        // Given one registered connection.
        let registry = ConnectionRegistry::default();
        let (_id, mut rx, _guard) = registry.register();

        // When a frame is broadcast.
        registry.broadcast(&ping());

        // Then the connection receives it.
        assert!(rx.try_recv().is_ok());
    }

    #[rstest::rstest]
    #[test]
    fn a_dropped_guard_unregisters_its_connection_at_once() {
        // Given one registered connection.
        let registry = ConnectionRegistry::default();
        let (_id, _rx, guard) = registry.register();

        // When the guard is dropped — the socket closed.
        drop(guard);

        // Then the registry no longer counts it, before any broadcast.
        assert_eq!(registry.len(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn an_unregistered_connection_receives_no_broadcast() {
        // Given a connection whose guard has been dropped.
        let registry = ConnectionRegistry::default();
        let (_id, mut rx, guard) = registry.register();
        drop(guard);

        // When a frame is broadcast.
        registry.broadcast(&ping());

        // Then the dead connection is not handed it.
        assert!(rx.try_recv().is_err());
    }

    #[rstest::rstest]
    #[test]
    fn a_connection_whose_receiver_is_dropped_is_removed_on_the_next_broadcast() {
        // Given a connection whose receiver has been dropped, guard still held.
        let registry = ConnectionRegistry::default();
        let (_id, rx, _guard) = registry.register();
        drop(rx);

        // When a frame is broadcast.
        registry.broadcast(&ping());

        // Then the dead connection is removed.
        assert_eq!(registry.len(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn two_connections_each_receive_the_same_broadcast() {
        // Given two registered connections.
        let registry = ConnectionRegistry::default();
        let (_a, mut rx_a, _guard_a) = registry.register();
        let (_b, mut rx_b, _guard_b) = registry.register();

        // When a frame is broadcast.
        registry.broadcast(&ping());

        // Then each receives exactly one.
        assert!(rx_a.try_recv().is_ok());
        assert!(rx_b.try_recv().is_ok());
        assert!(rx_a.try_recv().is_err());
        assert!(rx_b.try_recv().is_err());
    }

    #[rstest::rstest]
    #[test]
    fn each_registration_receives_a_distinct_id() {
        // Given two registrations.
        let registry = ConnectionRegistry::default();
        let (a, _rx_a, _guard_a) = registry.register();
        let (b, _rx_b, _guard_b) = registry.register();

        // Then their ids differ.
        assert_ne!(a, b);
    }
}
