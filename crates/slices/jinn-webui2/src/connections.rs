//! Every open WebSocket connection, and the one way a frame reaches them.
//!
//! Each connection's socket task owns a receiver; the registry holds the
//! sender. `broadcast` is the single mechanism every pushed `ServerMsg`
//! leaves through. A connection whose socket has closed is dropped from
//! the registry on the next broadcast.

use std::sync::Arc;

use jinn_webui2_msg::ServerMsg;
use parking_lot::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Debug, Clone, Default)]
pub struct ConnectionRegistry {
    inner: Arc<Mutex<Vec<UnboundedSender<ServerMsg>>>>,
}

impl ConnectionRegistry {
    /// Adds a connection and returns the receiver its socket task reads.
    pub fn register(&self) -> UnboundedReceiver<ServerMsg> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.inner.lock().push(tx);
        rx
    }

    /// Sends one frame to every open connection.
    pub fn broadcast(&self, msg: ServerMsg) {
        self.inner.lock().retain(|tx| tx.send(msg.clone()).is_ok());
    }

    /// How many connections are currently open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Whether no connection is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
